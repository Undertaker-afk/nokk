# Model Card: hCaptcha tile classifier

## Versioning

- Name: `hcaptcha-vN` (e.g. `hcaptcha-v1`, `hcaptcha-v2`).
- Bump minor on data-only refresh, major on arch/taxonomy change.
- Runtime loads only `crates/nokk-captcha/models/hcaptcha-vN.int8.onnx`
  (that dir is NOT owned by `tools/hcaptcha/`; do not write it from here).

## Intended use

- Offline 3x3 tile scoring for hCaptcha `image_label_binary` challenges.
- Target: 95%+ per-tile binary accuracy on held-out eval.
- CPU-only, 2-core budget. Not for people detection or identity.

## Training

- See `train.py` + `requirements.txt`; MobileNetV3-S default, ResNet18 alt.
- Head: sigmoid multi-label (`BCEWithLogitsLoss`) by default — one logit per
  class, per-tile binary decision via calibrated thresholds. `--head softmax`
  keeps the legacy multiclass baseline.
- Data: opt-in dumps (`collector.md`), dedup by SHA-256, taxonomy in `taxonomy.yaml`.
  Layout `<data>/<class>/*.png`; split 80% train / 10% calib / 10% eval (`--seed`).
- Offline flags: `--no-pretrained` (random init, zero downloads),
  `--no-augment` (ablation only), `--no-gate` (report but exit 0).
- Export ONNX opset 12+, static INT8 quant with 10% calibration split.

## Accuracy plan (what changed and why)

1. **Hard-negative mining (2 rounds, default).**
   - Round 0: uniform train for `--epochs`.
   - Mine on train logits: per class, negatives with
     `sigmoid >= 0.5 - margin` (near-miss FP) and positives with
     `sigmoid < 0.5 + margin` (miss), margin 0.15, top-256/class.
   - Round 1: refit `max(1, epochs//2)` on train + mined set via
     `WeightedRandomSampler` so each batch is `--hard-ratio` (default 0.5)
     mined rows. Stats in `report.json -> mining`.
   - Disable with `--rounds 1`. Tune with `--topk-per-class`, `--hard-ratio`.
2. **Mask-overlay augmentation (`RandomMaskOverlay`, default `p=0.5`).**
   - Random Circle / Square / Bar occluders, size 8-30% of tile edge, fill
     random/gray/black/white. Applied after flip + color jitter, before
     normalize. Motivation: mask-overlay ablations drop plain ViT 50-80pp;
     training with occluders forces use of unoccluded evidence.
3. **Prompt-template engineering (CLIP fallback text side).**
   - Templates (`train.py::PROMPT_TEMPLATES`), primary first:
     `a photo of a {label} on street`, `a photo of a {label}`,
     `a cropped photo of a {label}`, `a low-resolution photo of a {label}`,
     `a traffic camera photo of a {label}`, `a close-up photo of a {label}`.
   - `{label}` = canonical key with `_` -> space. Per-label text embedding =
     L2-normalize each template embedding, mean-pool, re-normalize.
4. **Per-tile sigmoid calibration (calib split, no peeking at eval).**
   - Step 1: temperature scaling — grid-search T in
     `[0.2, 0.5, 0.8, 1.0, 1.25, 1.5, 2.0, 3.0]`, argmin BCE NLL.
   - Step 2: per-label threshold sweep `0.05..0.95` (step 0.05), argmax F1;
     ties break toward `threshold_init` from `taxonomy.yaml`.
   - Artifacts: `thresholds.json` (`temperature`, `thresholds`),
     `report.json` (`per_class` P/R/F1/thresh/n, `binary_acc`, `macro_f1`).
   - Runtime rule: tile positive iff `sigmoid(logit_c / T) >= thresh[c]`.
5. **TinyCLIP / MobileCLIP-S0 fallback (spec + placeholder export).**
   - Preferred image tower: MobileCLIP-S0; alt: TinyCLIP. Text tower encodes
     the 6 templates x top-30 labels -> `text_emb_top30.npz` (L2-normed,
     `[30, dim]`) + `text_emb_top30.json` manifest.
   - `train.py::export_text_embeddings` writes the layout offline with
     deterministic placeholder vectors (`placeholder: true`) so the cosine
     path is testable with no network; replace with real weights pre-release.
   - Score: `cos(image_emb, text_emb[label]) -> sigmoid((s - bias_c) / temp)`.
   - Invoke ONLY when `prompt_norm` is OOV (extended taxonomy) OR primary
     `max_c sigmoid < margin`. Primary INT8 stays the fast path.
   - Fallback gates: text-emb total delta must keep model `< 25 MB`;
     cosine scoring `< 150 ms / 9` on 2 cores, else ship primary-only.

## Gates (must pass before release)

| Gate | Limit | Measured how |
|---|---|---|
| Size | < 25 MB | `model.int8.onnx` bytes / 1e6 |
| Latency | < 150 ms / 9 tiles, 2 cores | median of 20 `ort` runs, 5 warmup, batch-9 |
| Metrics | per-category P/R/F1 + `binary_acc` in `report.json` | calibrated sigmoid on eval split |

`train.py` exits non-zero on gate failure unless `--no-gate`.

## Outputs (`--out`)

- `fp32.pt`, `model.fp32.onnx`, `model.int8.onnx`
- `thresholds.json`, `text_emb_top30.npz` + `.json`, `report.json`

## How to run training once data collected

```bash
pip install -r requirements.txt
# 1. Collect opt-in tiles (see collector.md), then arrange <tiles>/<class>/*.png
#    with class names = taxonomy.yaml keys (top-30).
# 2. Train (CPU-only, ~2-core gate match):
python train.py --data ./tiles --taxonomy ./taxonomy.yaml --out ./out --arch mv3 --epochs 5
# 3. Inspect gates + calibration:
cat ./out/report.json
cat ./out/thresholds.json
# 4. Ablations (optional):
python train.py --data ./tiles --out ./out-softmax --head softmax --rounds 1 --no-augment --no-gate
# 5. Hand off: copy model.int8.onnx to crates/nokk-captcha/models/ only via owner.
```

## Ethics / privacy

- Opt-in collection only, no PII, no real images in `fixtures/`.
- Fixtures use fake SHA-256 strings and anonymized prompts.

## Release checklist

1. `python train.py --data <tiles> --taxonomy taxonomy.yaml --out out/` passes gates.
2. Confirm `binary_acc` target and no per-class recall collapse on rare labels.
3. Replace placeholder `text_emb_top30.npz` with real MobileCLIP-S0/TinyCLIP
   embeddings if claiming fallback support; re-verify size/latency gates.
4. Copy `model.int8.onnx` to `crates/nokk-captcha/models/` only via owner.
5. Add row below.

## History

| Version | Arch | Notes |
|---|---|---|
| hcaptcha-v0 | mv3 | Placeholder, untrained baseline |
| hcaptcha-v1 (spec) | mv3 sigmoid + mining + mask-overlay + calib + CLIP-fallback spec | Code+spec only, no weights yet |
