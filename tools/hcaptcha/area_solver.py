"""hCaptcha area_select solver vs Halligan local benchmark.
Grid-overlay + d1 per-cell decisions (2-level zoom), UI click on canvas, server verdict.
Usage: python area_solver.py [challenge_id] [port]"""
import base64, io, json, subprocess, sys, time, urllib.request
sys.path.insert(0, r'C:\Users\THW-User\Desktop\nokk\tools\hcaptcha')
from cdp import CDP
from PIL import Image, ImageDraw
import requests

NOKK = r'C:\Users\THW-User\Desktop\nokk\target\debug\nokk.exe'
BASE = 'http://127.0.0.1:3334'
D1DIR = r'C:\Users\THW-User\AppData\Local\Temp\opencode\models\d1-3B'

DRIVE = """(async () => {
  const id = '%s';
  const data = await fetch('./challenge/' + id).then(r => r.json());
  new Captcha(id, document.querySelector('#challenge'), data.subtype, data.instruction, data.instruction_image, data.images);
  document.querySelector('#test').style.opacity = 1;
  document.querySelector('#test').style.visibility = 'visible';
  const fr = document.querySelector('#challenge iframe');
  if (fr) fr.src = fr.src;
  return data.subtype + '|' + data.instruction.slice(0,100);
})()"""
CGEO = """(() => { const c = document.querySelector('canvas');
  if (!c || !c.width) return 'NONE';
  const r = c.getBoundingClientRect();
  return JSON.stringify({x: r.x, y: r.y, w: r.width, h: r.height, cw: c.width, ch: c.height,
    prompt: (document.querySelector('.prompt-text')||{textContent:''}).textContent.trim().slice(0,100)}); })()"""
IFR = """(() => { const f = [...document.querySelectorAll('iframe')].find(i => (i.src||'').includes('challenge_area'));
  if (!f) return 'NONE'; const r = f.getBoundingClientRect(); return JSON.stringify([r.x, r.y]); })()"""

_d1 = {}
def d1():
    if 'm' not in _d1:
        import torch
        from transformers import AutoModel
        dev = "cuda" if torch.cuda.is_available() else "cpu"
        dt = torch.bfloat16 if dev == "cuda" else torch.float32
        _d1['m'] = AutoModel.from_pretrained(D1DIR, trust_remote_code=True, dtype=dt).to(dev)
        print('d1 ready', flush=True)
    return _d1['m']

STOPWORDS = {'the','a','an','on','in','of','to','all','that','with','where','and','or','please','click','select','find','tap','things','would','usually','shown','lives','live','more','than','you','it','is','are','what','does','show','this','image','photo','picture','items','objects','entities','similar','following','pattern','each','once','there','none','left','squares','cost','money','spray','item','animal','animals','reference'}

def prompt_words(prompt):
    import re
    return [w for w in re.findall(r'[a-z]{3,}', prompt.lower()) if w not in STOPWORDS]

REGIONS = ['top-left', 'top-center', 'top-right',
           'middle-left', 'center', 'middle-right',
           'bottom-left', 'bottom-center', 'bottom-right']

def moon_region(img, noun):
    import base64 as _b, io as _io, json as _j, urllib.request as _u
    buf = _io.BytesIO(); img.save(buf, format='PNG')
    body = _j.dumps({"model": "moondream",
        "prompt": "In which ninth of this image is the %s? Regions are top-left, top-center, top-right, middle-left, center, middle-right, bottom-left, bottom-center, bottom-right. Reply with only the region name." % noun,
        "images": [_b.b64encode(buf.getvalue()).decode()],
        "stream": False, "options": {"num_predict": 30}}).encode()
    for _ in range(3):
        try:
            req = _u.Request("http://127.0.0.1:11434/api/generate", data=body,
                             headers={"Content-Type": "application/json"})
            r = (_j.loads(_u.urlopen(req, timeout=300).read()).get("response", "") or "").strip().lower()
            for i, name in enumerate(REGIONS):
                if name in r:
                    return i, r[:80]
        except Exception:
            pass
    return None, ''

def moon_describe(crops):
    import base64 as _b, io as _io, json as _j, urllib.request as _u
    out = []
    for c in crops:
        buf = _io.BytesIO(); c.save(buf, format='PNG')
        body = _j.dumps({"model": "moondream",
            "prompt": "List the main objects visible in this photo, briefly.",
            "images": [_b.b64encode(buf.getvalue()).decode()],
            "stream": False, "options": {"num_predict": 60}}).encode()
        req = _u.Request("http://127.0.0.1:11434/api/generate", data=body,
                         headers={"Content-Type": "application/json"})
        try:
            r = _j.loads(_u.urlopen(req, timeout=300).read()).get("response", "") or ""
        except Exception:
            r = ""
        out.append(r.strip()[:200])
    return out

def wordmatch(descs, words):
    import re
    scores = []
    for d in descs:
        toks = set(re.findall(r'[a-z]+', d.lower()))
        scores.append(sum(1 for w in words for t in toks if t == w or t.startswith(w) or w.startswith(t)))
    return scores

def d1_half(img, noun, axis):
    """Binary choice: which half holds the target? axis 0=x, 1=y."""
    import torch
    m = d1()
    if axis == 0:
        crit = {"left": "The %s is in the LEFT half of the image." % noun,
                "right": "The %s is in the RIGHT half of the image." % noun}
        ins = "Is the %s in the left half or the right half?" % noun
    else:
        crit = {"top": "The %s is in the TOP half of the image." % noun,
                "bottom": "The %s is in the BOTTOM half of the image." % noun}
        ins = "Is the %s in the top half or the bottom half?" % noun
    q = {"half": {"type": "choice", "instructions": ins, "criteria": crit}}
    with torch.inference_mode():
        res = m.system_one_batch([(None, q, [img])])
    a = res[0]["answers"]["half"]
    print('half:', a.get("choice"), {k: round(v, 2) for k, v in (a.get("probabilities") or {}).items()}, flush=True)
    return a

def d1_choice(grid_img, labels, noun):
    """One forward pass: which labeled cell contains the target?"""
    import torch
    m = d1()
    crit = {lab: "The %s is in the cell labeled %s." % (noun, lab) for lab in labels}
    q = {"where": {"type": "choice",
                    "instructions": "Which labeled grid cell contains the %s? Answer with the cell label." % noun,
                    "criteria": crit}}
    with torch.inference_mode():
        res = m.system_one_batch([(None, q, [grid_img])])
    a = res[0]["answers"]["where"]
    print('choice:', a.get("choice"), 'probs:', {k: round(v, 2) for k, v in (a.get("probabilities") or {}).items()}, flush=True)
    return a

def d1_batch(crops, prompt):
    import torch
    m = d1()
    q = 'Task: %s. Does this cropped photo region clearly show what the task asks for?' % prompt
    reqs = [(None, {"c%d" % i: {"type": "noul", "instructions": q}}, [c]) for i, c in enumerate(crops)]
    with torch.inference_mode():
        res = m.system_one_batch(reqs)
    return [res[i]["answers"]["c%d" % i]["noul"] for i in range(len(crops))]

def qwen_ground(crop_img, noun):
    """Native bbox grounding via qwen2.5vl. Returns (nx, ny) in crop coords or None."""
    import base64 as _b, io as _io, json as _j, re as _re, urllib.request as _u
    buf = _io.BytesIO(); crop_img.save(buf, format='PNG')
    body = _j.dumps({"model": "qwen2.5vl:7b",
        "prompt": "Find the %s in this image. Output its bounding box as JSON with keys x_min, y_min, x_max, y_max in 0-1000 scale." % noun,
        "images": [_b.b64encode(buf.getvalue()).decode()],
        "stream": False, "options": {"num_predict": 120, "temperature": 0}}).encode()
    try:
        req = _u.Request("http://127.0.0.1:11434/api/generate", data=body,
                         headers={"Content-Type": "application/json"})
        txt = (_j.loads(_u.urlopen(req, timeout=600).read()).get("response", "") or "")
        print('ground raw:', txt.strip()[:200], flush=True)
        m = _re.search(r'\{[^{}]*"x_min"\s*:\s*(\d+)[^{}]*"y_min"\s*:\s*(\d+)[^{}]*"x_max"\s*:\s*(\d+)[^{}]*"y_max"\s*:\s*(\d+)', txt)
        if not m:
            m2 = _re.search(r'"bbox_2d"\s*:\s*\[\s*(\d+)\s*,\s*(\d+)\s*,\s*(\d+)\s*,\s*(\d+)\s*\]', txt)
            if m2:
                x0, y0, x1, y1 = map(int, m2.groups())
            else:
                return None
        else:
            x0, y0, x1, y1 = map(int, m.groups())
        return ((x0 + x1) / 2 / 1000, (y0 + y1) / 2 / 1000)
    except Exception as e:
        print('ground err:', str(e)[:100], flush=True)
        return None

def som_dots(img, n=5):
    """Numbered dots overlay; return (img, [(label, x, y)])."""
    W, H = img.size
    g = img.copy()
    d = ImageDraw.Draw(g)
    pts = []
    for r in range(n):
        for c in range(n):
            lab = r * n + c + 1
            x, y = int((c + 0.5) * W / n), int((r + 0.5) * H / n)
            pts.append((lab, x, y))
            d.ellipse([x-9, y-9, x+9, y+9], fill=(255, 0, 0), outline=(255, 255, 255))
            d.text((x-5, y-8), str(lab), fill=(255, 255, 255))
    return g, pts

def d1_mark(grid_img, labels, noun):
    """Set-of-Marks: which numbered dot sits on the target?"""
    import torch
    m = d1()
    crit = {str(lab): "Numbered dot %s sits directly on the %s." % (lab, noun) for lab in labels}
    q = {"dot": {"type": "choice",
                 "instructions": "Which numbered red dot sits directly on the %s? Answer with the dot number." % noun,
                 "criteria": crit}}
    with torch.inference_mode():
        res = m.system_one_batch([(None, q, [grid_img])])
    a = res[0]["answers"]["dot"]
    print('marks:', a.get("choice"), {k: round(v, 2) for k, v in (a.get("probabilities") or {}).items()}, flush=True)
    return a

def overlay_grid(img, n, origin=(0, 0), size=None):
    W, H = img.size
    ox, oy = origin
    sw, sh = size or (W, H)
    g = img.copy()
    d = ImageDraw.Draw(g)
    boxes = []
    for r in range(n):
        for c in range(n):
            x0, y0 = ox + c*sw//n, oy + r*sh//n
            x1, y1 = ox + (c+1)*sw//n, oy + (r+1)*sh//n
            boxes.append((x0, y0, x1, y1))
            d.rectangle([x0, y0, x1, y1], outline=(255, 0, 0), width=2)
            d.text((x0+4, y0+4), '%s%d' % (chr(65+c), r+1), fill=(255, 0, 0))
    return g, boxes

def solve(cid, port=9420):
    srv = subprocess.Popen([sys.executable, '-X', 'utf8',
        r'C:\Users\THW-User\Desktop\ReCAP-Agent\halligan_captchas\server.py'],
        stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    time.sleep(4)
    nokk = subprocess.Popen([NOKK, '--port', str(port)],
                            stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    time.sleep(3)
    try:
        ver = json.loads(urllib.request.urlopen('http://127.0.0.1:%d/json/version' % port, timeout=15).read())
        c = CDP(ver['webSocketDebuggerUrl'])
        tgt = c.call('Target.createTarget', {'url': 'about:blank'})['targetId']
        sess = c.call('Target.attachToTarget', {'targetId': tgt, 'flatten': True})['sessionId']
        call = lambda m, p=None, timeout=120: c.call(m, p, timeout, sess)
        call('Page.navigate', {'url': '%s/hcaptcha/%s' % (BASE, cid)})
        time.sleep(5)
        print('drive:', call('Runtime.evaluate', {'expression': DRIVE % cid, 'awaitPromise': True}, timeout=60), flush=True)
        d = requests.get('%s/hcaptcha/challenge/%s' % (BASE, cid), timeout=20).json()
        prompt = d['instruction']
        print('PROMPT:', prompt, flush=True)
        img = Image.open(io.BytesIO(base64.b64decode(d['images'][0]))).convert('RGB')
        print('img:', img.size, flush=True)
        # binary search: halves on clean image (no overlay to misread)
        words = prompt_words(prompt)
        print('words:', words, flush=True)
        noun = ' '.join(words[:3]) if words else prompt[:40]
        W, H = img.size
        big = img.resize((512, 512)) if max(W, H) < 400 else img
        # primary: native grounding on FULL image
        gfull = qwen_ground(big, noun)
        if gfull is not None:
            # refine: re-ground on quadrant crop around first fix for precision
            qx0 = max(0, int((gfull[0] - 0.25) * W)); qy0 = max(0, int((gfull[1] - 0.25) * H))
            qx1 = min(W, int((gfull[0] + 0.25) * W)); qy1 = min(H, int((gfull[1] + 0.25) * H))
            quad = img.crop((qx0, qy0, qx1, qy1))
            g2 = qwen_ground(quad.resize((512, 512)), noun)
            if g2 is not None:
                nx = (qx0 + g2[0] * (qx1 - qx0)) / W
                ny = (qy0 + g2[1] * (qy1 - qy0)) / H
            else:
                nx, ny = gfull
            print('TARGET norm: %.3f %.3f (labels %s)' % (nx, ny, d['labels']), flush=True)
        else:
            ax = d1_half(big, noun, 0)
            ay = d1_half(big, noun, 1)
            cx = 0.25 if ax.get('choice') == 'left' else (0.75 if ax.get('choice') == 'right' else 0.5)
            cy = 0.25 if ay.get('choice') == 'top' else (0.75 if ay.get('choice') == 'bottom' else 0.5)
            print('bisect-fallback: (%.2f, %.2f)' % (cx, cy), flush=True)
            qx0, qy0 = (cx - 0.25) * W, (cy - 0.25) * H
            qx1, qy1 = (cx + 0.25) * W, (cy + 0.25) * H
            quad = img.crop((int(qx0), int(qy0), int(qx1), int(qy1)))
            g = qwen_ground(quad.resize((512, 512)), noun)
            if g is None:
                print('ground failed; quadrant center fallback', flush=True)
                nx, ny = cx, cy
            else:
                nx = (qx0 + g[0] * (qx1 - qx0)) / W
                ny = (qy0 + g[1] * (qy1 - qy0)) / H
            print('TARGET norm: %.3f %.3f (labels %s)' % (nx, ny, d['labels']), flush=True)
        # wait for canvas, compute page coords with their margin formula
        oxoy = None
        ageo = None
        for _ in range(8):
            time.sleep(4)
            frs = call('Nokk.frames')['frames']
            area = [f for f in frs if 'challenge_area' in f.get('url', '')]
            if not area:
                continue
            g = call('Runtime.evaluate', {'contextId': area[0]['executionContextId'], 'expression': CGEO, 'returnByValue': True}, timeout=60)['result']['value']
            if g != 'NONE':
                ageo = json.loads(g)
                if ageo['h'] > 0:
                    break
        print('canvas:', ageo, flush=True)
        if ageo and ageo.get('h', 0) > 0:
            ox, oy = json.loads(call('Runtime.evaluate', {'expression': IFR, 'returnByValue': True}, timeout=60)['result']['value'])
            # replicate load_area margins (canvas attr size vs image aspect)
            cw, chh = ageo['cw'], ageo['ch']
            W, H = img.size
            if H >= W:
                marginY = 20; hh = chh - 40; ww = W * (hh / H); marginX = (cw - ww) / 2
            else:
                marginX = 10; ww = cw - 20; hh = H * (ww / W); marginY = (chh - hh) / 2
            # canvas CSS scale vs attr pixels
            sx = ageo['w'] / cw
            click_fx, click_fy = marginX + nx * ww, marginY + ny * hh
            px_, py_ = ox + ageo['x'] + click_fx * sx, oy + ageo['y'] + click_fy * sx
            print('click page coords:', (round(px_, 1), round(py_, 1)), flush=True)
            call('Input.dispatchMouseEvent', {'type': 'mouseMoved', 'x': px_ - 40, 'y': py_ - 20})
            time.sleep(0.4)
            call('Input.dispatchMouseEvent', {'type': 'mouseMoved', 'x': px_, 'y': py_})
            time.sleep(0.3)
            call('Input.dispatchMouseEvent', {'type': 'mousePressed', 'x': px_, 'y': py_, 'button': 'left', 'clickCount': 1})
            time.sleep(0.2)
            call('Input.dispatchMouseEvent', {'type': 'mouseReleased', 'x': px_, 'y': py_, 'button': 'left', 'clickCount': 1})
            time.sleep(3)
        else:
            print('canvas never drew; verdict via direct submit only', flush=True)
        # verdict via direct submit (ground truth from server)
        r = requests.post('%s/hcaptcha/submit' % BASE, json={'id': int(cid), 'state': [nx, ny], 'challenge_type': 'area'}, timeout=20).json()
        print('SERVER VERDICT:', r, flush=True)
        c.close()
        return r.get('solved', False)
    finally:
        nokk.terminate()
        srv.terminate()

if __name__ == '__main__':
    cid = sys.argv[1] if len(sys.argv) > 1 else '74'
    print('SOLVED:' if solve(cid) else 'FAILED:', cid, flush=True)
