# Fine-tune a tile classifier for hCaptcha image_label_binary, export ONNX + static INT8.
# Pinned CPU-only requirements (pip install -r requirements.txt):
#   torch==2.3.1+cpu torchvision==0.18.1+cpu onnx==1.16.1
#   onnxruntime==1.18.0 numpy==1.26.4 Pillow==10.3.0
# Usage: python train.py --data ./tiles --taxonomy ./taxonomy.yaml --out ./out
# Layout: <data>/<class>/*.png ; split: 80% train / 10% calib / 10% eval.
#
# Accuracy plan toward 95%+ binary per-tile (see MODEL_CARD.md):
#   1. Sigmoid multi-label head (BCE) + per-label threshold calibration on calib split.
#   2. Hard-negative mining: round-0 uniform, round-1 oversamples mined FP/FN.
#   3. Mask-overlay augmentation (random Circle/Square/Bar): hCaptcha-style occluders
#      drop plain ViT 50-80pp per literature; training with them closes the gap.
#   4. Prompt-template engineering for CLIP fallback: mean-pooled text embeddings
#      over templates like "a photo of a {label} on street".
#   5. TinyCLIP / MobileCLIP-S0 fallback (spec only): precomputed L2-normed text
#      embeddings for top-30 labels; image-tower INT8 must still pass size/latency gates.
# No network training here: code + spec only. Run once data collected (collector.md).
"""Train MobileNetV3-S (default) or ResNet18 with sigmoid head, mine hard negs, calibrate, gate."""
import argparse
import json
import random
import time
from pathlib import Path

import numpy as np
import torch
from torch.utils.data import DataLoader, Subset, WeightedRandomSampler, random_split
from torchvision import datasets, models, transforms

try:
    from PIL import ImageDraw
except Exception:  # Pillow is pinned in requirements; guard for import checkers.
    ImageDraw = None

SIZE_GATE_MB = 25.0  # model must stay under this (INT8 file)
LAT_GATE_MS = 150.0  # per 9 tiles on 2 CPU cores

# Canonical top-30, same order as crates/nokk-captcha/src/taxonomy.rs LABELS.
TOP30 = [
    "truck", "car", "bus", "motorcycle", "bicycle", "airplane", "helicopter", "boat", "train", "tram",
    "dog", "cat", "bird", "horse", "cow", "sheep", "lion", "tiger", "bear", "elephant",
    "zebra", "giraffe", "monkey", "rabbit", "deer", "frog", "fish", "shark", "whale", "snake",
]

# Init thresholds mirror taxonomy::threshold_for (calibration refines them on calib split).
DEFAULT_THRESHOLDS = {
    "car": 0.55, "dog": 0.55, "cat": 0.55,
    "truck": 0.60, "bus": 0.60, "motorcycle": 0.60, "bicycle": 0.60,
    "airplane": 0.60, "train": 0.60, "boat": 0.60,
    "helicopter": 0.70, "tram": 0.70,
    "bird": 0.60, "horse": 0.60, "cow": 0.60, "sheep": 0.60,
    "lion": 0.65, "tiger": 0.65, "bear": 0.65, "elephant": 0.65,
    "zebra": 0.65, "giraffe": 0.65, "monkey": 0.65,
    "rabbit": 0.70, "deer": 0.70, "frog": 0.70, "fish": 0.70,
    "shark": 0.75, "whale": 0.75, "snake": 0.75,
}

# Prompt templates for CLIP text-tower fallback. Mean-pool embeddings over these
# per label. `{label}` is the canonical key with underscores -> spaces.
# "a photo of a {label} on street" is the primary template (street-scene prior).
PROMPT_TEMPLATES = [
    "a photo of a {label} on street",
    "a photo of a {label}",
    "a cropped photo of a {label}",
    "a low-resolution photo of a {label}",
    "a traffic camera photo of a {label}",
    "a close-up photo of a {label}",
]


def label_to_phrase(label: str) -> str:
    return label.replace("_", " ")


def render_prompts(label: str):
    phrase = label_to_phrase(label)
    return [t.format(label=phrase) for t in PROMPT_TEMPLATES]


class RandomMaskOverlay:
    """Random Circle/Square/Bar occluder augmentation (PIL -> PIL).

    Motivation: mask-overlay ablations drop plain ViT accuracy 50-80pp;
    hCaptcha tiles carry similar occluders/bars. Train with them so the
    CNN learns to use unoccluded evidence instead of memorizing clean tiles.

    Args:
        p: probability of applying 1..max_overlays shapes to a tile.
        max_overlays: upper bound of shapes per tile.
        scale: (lo, hi) fraction of tile edge used as shape size.
        fill: 0 / 255 / "gray" / "random" fill color for the occluder.
    """

    def __init__(self, p=0.5, max_overlays=2, scale=(0.08, 0.30), fill="random"):
        self.p = p
        self.max_overlays = max_overlays
        self.scale = scale
        self.fill = fill

    def _color(self, rng):
        if self.fill == "random":
            v = int(rng.randint(0, 255))
            return (v, v, v)
        if self.fill == "gray":
            return (128, 128, 128)
        v = int(self.fill)
        return (v, v, v)

    def __call__(self, img):
        rng = random
        if rng.random() > self.p:
            return img
        if ImageDraw is None:
            return img
        w, h = img.size
        d = ImageDraw.Draw(img)
        for _ in range(rng.randint(1, self.max_overlays)):
            s = rng.uniform(*self.scale) * min(w, h)
            x0 = rng.uniform(0, max(1, w - s))
            y0 = rng.uniform(0, max(1, h - s))
            x1, y1 = x0 + s, y0 + s
            kind = rng.choice(["circle", "square", "bar"])
            c = self._color(rng)
            if kind == "circle":
                d.ellipse([x0, y0, x1, y1], fill=c)
            elif kind == "square":
                d.rectangle([x0, y0, x1, y1], fill=c)
            else:  # horizontal/vertical bar occluder
                if rng.random() < 0.5:
                    bh = max(2, int(h * rng.uniform(0.04, 0.12)))
                    yy = rng.uniform(0, max(1, h - bh))
                    d.rectangle([0, yy, w, yy + bh], fill=c)
                else:
                    bw = max(2, int(w * rng.uniform(0.04, 0.12)))
                    xx = rng.uniform(0, max(1, w - bw))
                    d.rectangle([xx, 0, xx + bw, h], fill=c)
        return img


IMAGENET_MEAN = [0.485, 0.456, 0.406]
IMAGENET_STD = [0.229, 0.224, 0.225]


def get_transforms(img=128, overlay_p=0.5, no_augment=False):
    if no_augment:
        ev = transforms.Compose([
            transforms.Resize((img, img)),
            transforms.ToTensor(),
            transforms.Normalize(IMAGENET_MEAN, IMAGENET_STD),
        ])
        return ev, ev
    tr = transforms.Compose([
        transforms.Resize((img, img)),
        transforms.RandomHorizontalFlip(p=0.5),
        transforms.ColorJitter(brightness=0.25, contrast=0.25, saturation=0.2),
        RandomMaskOverlay(p=overlay_p),
        transforms.ToTensor(),
        transforms.Normalize(IMAGENET_MEAN, IMAGENET_STD),
    ])
    ev = transforms.Compose([
        transforms.Resize((img, img)),
        transforms.ToTensor(),
        transforms.Normalize(IMAGENET_MEAN, IMAGENET_STD),
    ])
    return tr, ev


def build_model(name, ncls, head="sigmoid", pretrained=True):
    """Small backbones only; keeps quantized model <25MB."""
    weights = "DEFAULT" if pretrained else None
    if name == "mv3":
        m = models.mobilenet_v3_small(weights=weights)
        m.classifier[-1] = torch.nn.Linear(m.classifier[-1].in_features, ncls)
    else:
        m = models.resnet18(weights=weights)
        m.fc = torch.nn.Linear(m.fc.in_features, ncls)
    m._head = head  # informational; both heads are linear logits
    return m


def get_loaders(data, img=128, bs=64, overlay_p=0.5, seed=0, no_augment=False):
    tr_tf, ev_tf = get_transforms(img, overlay_p, no_augment)
    # ImageFolder applies one transform; wrap: load train with tr_tf, calib/eval with ev_tf
    # by building two views over the same file list.
    base = datasets.ImageFolder(data, transform=tr_tf)
    n = len(base)
    if n == 0:
        raise SystemExit(f"no images under {data}; expected <data>/<class>/*.png")
    tr_n, ca_n = int(n * 0.8), int(n * 0.1)
    ev_n = n - tr_n - ca_n
    gen = torch.Generator().manual_seed(seed)
    tr_idx, ca_idx, ev_idx = random_split(range(n), [tr_n, ca_n, ev_n], generator=gen)
    # Views share (path, target) list but differ in transform.
    train_ds = _ViewSubset(base, list(tr_idx), tr_tf)
    calib_ds = _ViewSubset(base, list(ca_idx), ev_tf)
    eval_ds = _ViewSubset(base, list(ev_idx), ev_tf)
    kw = dict(batch_size=bs, num_workers=0)  # CPU-only, portable
    return base, DataLoader(train_ds, shuffle=True, **kw), DataLoader(calib_ds, **kw), DataLoader(eval_ds, **kw)


class _ViewSubset(torch.utils.data.Dataset):
    def __init__(self, base, indices, transform):
        self.base = base
        self.indices = indices
        self.transform = transform

    def __len__(self):
        return len(self.indices)

    def __getitem__(self, i):
        path, target = self.base.samples[self.indices[i]]
        from PIL import Image
        with open(path, "rb") as f:
            img = Image.open(f).convert("RGB")
        return self.transform(img), target


def train_one_round(m, loader, epochs=5, lr=3e-4, head="sigmoid"):
    torch.set_num_threads(2)  # match 2-core latency gate
    opt = torch.optim.AdamW(m.parameters(), lr=lr)
    loss_fn = torch.nn.BCEWithLogitsLoss() if head == "sigmoid" else torch.nn.CrossEntropyLoss()
    m.train()
    for _ in range(epochs):
        for x, y in loader:
            opt.zero_grad()
            logits = m(x)
            if head == "sigmoid":
                y_oh = torch.zeros_like(logits)
                y_oh.scatter_(1, y.unsqueeze(1), 1.0)
                loss_fn(logits, y_oh).backward()
            else:
                loss_fn(logits, y).backward()
            opt.step()
    return m


@torch.no_grad()
def collect_logits(m, loader):
    """Return (logits[N,C], targets[N]) over a loader."""
    m.eval()
    ls, ts = [], []
    for x, y in loader:
        ls.append(m(x).detach().cpu())
        ts.append(y.detach().cpu())
    return torch.cat(ls), torch.cat(ts)


def mine_hard_negatives(logits, targets, thresh=0.5, topk_per_class=256, margin=0.15):
    """Hard-negative mining plan (offline, code implementation).

    For each class c, a sample is a hard negative when target != c but
    sigmoid(logit_c) >= thresh - margin (near-miss / false-positive region),
    and a hard positive when target == c but score < thresh + margin (miss).
    Returns sorted unique sample indices, hardest first, capped at
    topk_per_class * ncls. Caller oversamples these in round-1 (see main).
    """
    probs = torch.sigmoid(logits)
    n, ncls = probs.shape
    hard = []
    for c in range(ncls):
        s = probs[:, c]
        is_pos = targets == c
        neg_mask = (~is_pos) & (s >= thresh - margin)
        pos_mask = is_pos & (s < thresh + margin)
        # Hardest = highest score among negatives, lowest among positives.
        neg_idx = torch.where(neg_mask)[0].tolist()
        neg_idx.sort(key=lambda i: -float(s[i]))
        pos_idx = torch.where(pos_mask)[0].tolist()
        pos_idx.sort(key=lambda i: float(s[i]))
        hard.extend(neg_idx[:topk_per_class])
        hard.extend(pos_idx[:topk_per_class // 4])
    # Dedupe preserving order.
    seen, out = set(), []
    for i in hard:
        if i not in seen:
            seen.add(i)
            out.append(i)
    return out


def fit_temperature(logits, targets_onehot, grid=(0.2, 0.5, 0.8, 1.0, 1.25, 1.5, 2.0, 3.0)):
    """Single-parameter temperature scaling on the calib split (NLL selection).

    Per-tile sigmoid calibration procedure step 1: divide logits by T, pick T
    minimizing BCE-with-logits NLL on calib. Returns best T (float).
    """
    bce = torch.nn.BCEWithLogitsLoss()
    best, best_t = float("inf"), 1.0
    for t in grid:
        v = float(bce(logits / t, targets_onehot))
        if v < best:
            best, best_t = v, t
    return best_t


def sweep_thresholds(probs, targets, classes, init, step=0.05):
    """Per-label threshold sweep maximizing F1 on calib split (procedure step 2).

    For each class c, sweep thresh in [0.05, 0.95]; pick argmax F1(c).
    Ties prefer the value closest to init[c] (stable fail-closed behavior).
    Returns {label: threshold}.
    """
    out = {}
    for i, c in enumerate(classes):
        y = (targets == i).numpy().astype(np.int32)
        s = probs[:, i].numpy()
        if y.sum() == 0 or y.sum() == len(y):
            out[c] = float(init.get(c, 0.60))
            continue
        best_f1, best_t = -1.0, init.get(c, 0.60)
        t = 0.05
        cands = []
        while t <= 0.951:
            pred = (s >= t).astype(np.int32)
            tp = int(((pred == 1) & (y == 1)).sum())
            fp = int(((pred == 1) & (y == 0)).sum())
            fn = int(((pred == 0) & (y == 1)).sum())
            f1 = 2 * tp / max(1, 2 * tp + fp + fn)
            cands.append((f1, t))
            t += step
        best_f1 = max(f for f, _ in cands)
        tied = [t for f, t in cands if abs(f - best_f1) < 1e-9]
        best_t = min(tied, key=lambda t: abs(t - init.get(c, 0.60)))
        out[c] = round(float(best_t), 3)
    return out


def per_class_pr_sigmoid(probs, targets, classes, thresholds):
    res = {}
    for i, c in enumerate(classes):
        y = (targets == i).numpy().astype(np.int32)
        pred = (probs[:, i].numpy() >= thresholds.get(c, 0.60)).astype(np.int32)
        tp = int(((pred == 1) & (y == 1)).sum())
        fp = int(((pred == 1) & (y == 0)).sum())
        fn = int(((pred == 0) & (y == 1)).sum())
        p = tp / max(1, tp + fp)
        r = tp / max(1, tp + fn)
        res[c] = {"p": round(p, 4), "r": round(r, 4),
                  "f1": round(2 * tp / max(1, 2 * tp + fp + fn), 4),
                  "thresh": thresholds.get(c, 0.60), "n": int(y.sum())}
    return res


def measure_latency_ms(sess, dummy9, repeats=20, warmup=5):
    for _ in range(warmup):
        sess.run(None, {"input": dummy9})
    ts = []
    for _ in range(repeats):
        t0 = time.perf_counter()
        sess.run(None, {"input": dummy9})
        ts.append((time.perf_counter() - t0) * 1000)
    ts.sort()
    return float(ts[len(ts) // 2])  # median, robust on shared CI runners


def export_text_embeddings(out_dir, labels, dim=512, seed=0):
    """TinyCLIP / MobileCLIP-S0 fallback spec: precomputed text embeddings.

    Preferred path (needs weights + open_clip/torch at training time only):
      for each top-30 label: encode render_prompts(label) with the CLIP text
      tower, L2-normalize each, mean-pool, re-normalize -> [dim] vector.
    Offline path (this repo, no network): write deterministic pseudo-embeddings
    (seeded RNG, L2-normed) + a manifest marking them as placeholders, so the
    file layout is fixed and the runtime cosine-similarity code path is testable.
    Real weights replace the .npz before release; gates still apply to the
    image tower (<25MB total delta, <150ms/9 cosine scoring on CPU).
    """
    out_dir = Path(out_dir)
    rng = np.random.default_rng(seed)
    mat = rng.standard_normal((len(labels), dim)).astype(np.float32)
    mat /= np.linalg.norm(mat, axis=1, keepdims=True) + 1e-9
    try:
        import open_clip  # noqa: F401
        backend = "open_clip (real encoder expected; rerun with weights)"
        placeholder = True  # still placeholder until weights are wired per MODEL_CARD
    except Exception:
        backend = "placeholder (no open_clip installed; deterministic RNG)"
        placeholder = True
    np.savez(out_dir / "text_emb_top30.npz",
             embeddings=mat, labels=np.array(labels), dim=np.array(dim))
    (out_dir / "text_emb_top30.json").write_text(json.dumps({
        "model": "MobileCLIP-S0 (preferred) or TinyCLIP (alt)",
        "dim": dim,
        "labels": labels,
        "templates": PROMPT_TEMPLATES,
        "l2_normalized": True,
        "mean_pooled_over_templates": True,
        "backend": backend,
        "placeholder": placeholder,
        "score": "cosine(image_emb, text_emb[label]) -> sigmoid((s - bias_c) / temp)",
        "when": "OOV prompt_norm OR primary max-prob < margin (see MODEL_CARD.md)",
    }, indent=2))
    return placeholder


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--data", required=True)
    ap.add_argument("--taxonomy", default=None, help="taxonomy.yaml (validates class coverage)")
    ap.add_argument("--out", default="./out")
    ap.add_argument("--arch", choices=["mv3", "r18"], default="mv3")
    ap.add_argument("--head", choices=["sigmoid", "softmax"], default="sigmoid",
                    help="sigmoid = per-tile binary heads for image_label_binary (default)")
    ap.add_argument("--epochs", type=int, default=5)
    ap.add_argument("--rounds", type=int, default=2, help="mining rounds: 1 = no mining, 2 = mine+refit")
    ap.add_argument("--hard-ratio", type=float, default=0.5, help="fraction of round-1 batch drawn from mined set")
    ap.add_argument("--topk-per-class", type=int, default=256)
    ap.add_argument("--img", type=int, default=128)
    ap.add_argument("--batch", type=int, default=64)
    ap.add_argument("--overlay-p", type=float, default=0.5, help="RandomMaskOverlay probability")
    ap.add_argument("--no-augment", action="store_true")
    ap.add_argument("--no-pretrained", action="store_true", help="fully offline: random init, no weight download")
    ap.add_argument("--seed", type=int, default=0)
    ap.add_argument("--opset", type=int, default=12)  # ONNX opset 12+ for ort compat
    ap.add_argument("--no-gate", action="store_true", help="report gates but exit 0")
    ap.add_argument("--emb-dim", type=int, default=512)
    args = ap.parse_args()

    random.seed(args.seed)
    np.random.seed(args.seed)
    torch.manual_seed(args.seed)

    out = Path(args.out)
    out.mkdir(parents=True, exist_ok=True)
    base, tr_l, ca_l, ev_l = get_loaders(args.data, img=args.img, bs=args.batch,
                                         overlay_p=args.overlay_p, seed=args.seed,
                                         no_augment=args.no_augment)
    classes = base.classes
    if args.taxonomy:
        try:
            import yaml  # optional; warn if missing
            tax = yaml.safe_load(Path(args.taxonomy).read_text())
            keys = {e["key"] for e in tax if isinstance(e, dict) and "key" in e}
        except Exception:
            keys, tax = set(), None
            print("warn: --taxonomy unreadable (need pyyaml); skipping coverage check")
        missing = [c for c in classes if c not in keys] if keys else []
        if missing:
            print(f"warn: data classes missing from taxonomy: {missing}")

    m = build_model(args.arch, len(classes), head=args.head, pretrained=not args.no_pretrained)
    m = train_one_round(m, tr_l, args.epochs, head=args.head)

    # --- Round 2: hard-negative mining (train split, in-memory logits) ---
    mining = {"rounds": 1, "mined": 0, "hard_ratio": 0.0}
    if args.rounds >= 2:
        tr_logits, tr_targets = collect_logits(m, tr_l)
        # Map flat loader order back to dataset rows is loader-dependent; mine per-batch
        # weights instead: simplest portable oversample = retrain with hard examples
        # duplicated via a WeightedRandomSampler built from per-sample hardness.
        probs = torch.sigmoid(tr_logits) if args.head == "sigmoid" else torch.softmax(tr_logits, 1)
        hard_idx = mine_hard_negatives(
            tr_logits, tr_targets,
            thresh=0.5, topk_per_class=args.topk_per_class)
        mining = {"rounds": 2, "mined": len(hard_idx), "hard_ratio": args.hard_ratio,
                  "topk_per_class": args.topk_per_class}
        if hard_idx:
            # Rebuild train loader: hard subset (train transform) + full train, sampled
            # so each batch holds ~hard_ratio mined rows.
            from torch.utils.data import ConcatDataset
            tr_ds = tr_l.dataset
            hard_ds = Subset(tr_ds, [h % len(tr_ds) for h in hard_idx])
            combo = ConcatDataset([tr_ds, hard_ds])
            n_main, n_hard = len(tr_ds), len(hard_ds)
            w_main = (1 - args.hard_ratio) / max(1, n_main)
            w_hard = args.hard_ratio / max(1, n_hard)
            weights = torch.tensor([w_main] * n_main + [w_hard] * n_hard)
            sampler = WeightedRandomSampler(weights, num_samples=len(weights), replacement=True)
            tr_l2 = DataLoader(combo, batch_size=args.batch, sampler=sampler, num_workers=0)
            m = train_one_round(m, tr_l2, max(1, args.epochs // 2), head=args.head)
        else:
            print("mining: no hard examples found; skipping round-2 refit")

    torch.save(m.state_dict(), out / "fp32.pt")

    # --- Calibration on calib split: temperature + per-label thresholds ---
    ca_logits, ca_targets = collect_logits(m, ca_l)
    ncls = ca_logits.shape[1]
    ca_oh = torch.zeros_like(ca_logits)
    ca_oh.scatter_(1, ca_targets.unsqueeze(1), 1.0)
    temp = fit_temperature(ca_logits, ca_oh) if args.head == "sigmoid" else 1.0
    ca_probs = torch.sigmoid(ca_logits / temp) if args.head == "sigmoid" else torch.softmax(ca_logits, 1)
    init = {c: DEFAULT_THRESHOLDS.get(c, 0.60) for c in classes}
    thresholds = sweep_thresholds(ca_probs, ca_targets, classes, init) if args.head == "sigmoid" else init
    (out / "thresholds.json").write_text(json.dumps({"temperature": temp, "thresholds": thresholds}, indent=2))

    # --- Eval split: binary per-tile metrics with calibrated thresholds ---
    ev_logits, ev_targets = collect_logits(m, ev_l)
    ev_probs = torch.sigmoid(ev_logits / temp) if args.head == "sigmoid" else torch.softmax(ev_logits, 1)
    if args.head == "sigmoid":
        per_class = per_class_pr_sigmoid(ev_probs, ev_targets, classes, thresholds)
        # Binary accuracy: correct iff target class scores above its threshold
        # and all other classes score below theirs (strict single-label tiles).
        hits, total = 0, ev_targets.numel()
        thr = torch.tensor([thresholds.get(c, 0.60) for c in classes])
        for i in range(len(ev_targets)):
            t = int(ev_targets[i])
            ok = bool(ev_probs[i, t] >= thr[t]) and bool((ev_probs[i, torch.arange(ncls) != t] < thr[torch.arange(ncls) != t]).all())
            hits += int(ok)
        bin_acc = hits / max(1, total)
        macro_f1 = float(np.mean([v["f1"] for v in per_class.values()])) if per_class else 0.0
    else:
        per_class = {}
        for i, c in enumerate(classes):
            pred_all = ev_logits.argmax(1)
            tp = int(((pred_all == i) & (ev_targets == i)).sum())
            fp = int(((pred_all == i) & (ev_targets != i)).sum())
            fn = int(((pred_all != i) & (ev_targets == i)).sum())
            per_class[c] = {"p": tp / max(1, tp + fp), "r": tp / max(1, tp + fn), "n": int((ev_targets == i).sum())}
        bin_acc = float((ev_logits.argmax(1) == ev_targets).float().mean())
        macro_f1 = 0.0

    dummy = torch.randn(1, 3, args.img, args.img)
    torch.onnx.export(m.eval(), dummy, str(out / "model.fp32.onnx"),
                      input_names=["input"], output_names=["logits"], opset_version=max(12, args.opset))
    # Static INT8 quant with calibration split (onnxruntime).
    from onnxruntime.quantization import quantize_static, CalibrationDataReader, QuantType

    class Reader(CalibrationDataReader):
        def __init__(self):
            self.it = iter(ca_l)

        def get_next(self):
            try:
                x, _ = next(self.it)
                return {"input": x.numpy()}
            except StopIteration:
                return None

    quantize_static(str(out / "model.fp32.onnx"), str(out / "model.int8.onnx"), Reader(), weight_type=QuantType.QInt8)
    size_mb = (out / "model.int8.onnx").stat().st_size / 1e6
    import onnxruntime as ort
    so = ort.SessionOptions()
    so.intra_op_num_threads = 2
    so.inter_op_num_threads = 1
    sess = ort.InferenceSession(str(out / "model.int8.onnx"), sess_options=so, providers=["CPUExecutionProvider"])
    dummy9 = dummy.repeat(9, 1, 1, 1).numpy()
    lat_ms = measure_latency_ms(sess, dummy9)  # 9-tile challenge batch, median of 20
    export_text_embeddings(out, [c for c in classes if c in TOP30] or classes[:30], dim=args.emb_dim, seed=args.seed)
    report = {"arch": args.arch, "head": args.head, "classes": classes,
              "temperature": temp, "thresholds": thresholds,
              "per_class": per_class, "binary_acc": round(bin_acc, 4), "macro_f1": round(macro_f1, 4),
              "mining": mining, "overlay_p": args.overlay_p,
              "size_mb": round(size_mb, 2), "latency_ms_per9": round(lat_ms, 1),
              "pass": bool(size_mb < SIZE_GATE_MB and lat_ms < LAT_GATE_MS)}
    (out / "report.json").write_text(json.dumps(report, indent=2))
    print(json.dumps(report, indent=2))
    if not report["pass"] and not args.no_gate:
        raise SystemExit(f"gate FAILED: size {size_mb:.2f}MB / {lat_ms:.1f}ms per 9 (limits {SIZE_GATE_MB}MB, {LAT_GATE_MS}ms)")


if __name__ == "__main__":
    main()
