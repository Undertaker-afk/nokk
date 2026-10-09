"""End-to-end hCaptcha Enterprise solver: nokk (CDP) + d1-3B (tiles) + human presses.
Usage: python solver.py [url] [--headless-port N]
Flow per crumb: execute/press checkbox -> read prompt+tiles -> d1 classify ->
press selected tiles -> submit -> check token. Repeat until token or budget out.
"""
import base64, io, json, re, subprocess, sys, time, urllib.request
import requests
sys.path.insert(0, r'C:\Users\THW-User\Desktop\nokk\tools\hcaptcha')
from cdp import CDP
from PIL import Image

NOKK = r'C:\Users\THW-User\Desktop\nokk\target\debug\nokk.exe'
OLLAMA = 'http://127.0.0.1:11434/api/generate'
D1DIR = r'C:\Users\THW-User\AppData\Local\Temp\opencode\models\d1-3B'

EXTRACT = """(() => {
  const tiles = [...document.querySelectorAll('.task-image')];
  return JSON.stringify({
    prompt: ((document.querySelector('#prompt-question')||{}).textContent||'').slice(0,200),
    tiles: tiles.map(el => {
      const inner = el.querySelector('.image');
      let u = inner ? (inner.style.backgroundImage || '') : '';
      const m = /url\\("?([^"')]+)"?\\)/.exec(u);
      return m ? m[1] : null;
    }),
    submit: !!document.querySelector('.button-submit'),
    ref: (() => { const im = document.querySelector('.examples img'); if (!im) return null; return im.src; })(),
  });
})()"""
TAG = "document.querySelectorAll('.task').forEach((el,i)=>{el.id='tile'+i});'tagged'"
TOKEN = """(() => { const i = document.querySelector('[name=h-captcha-response],[name=g-recaptcha-response]'); return i ? i.value.slice(0,40) : ''; })()"""


def ollama_describe(img, label):
    buf = io.BytesIO(); img.save(buf, format='PNG')
    body = json.dumps({"model": "moondream",
        "prompt": "Describe the main subject of this photo in one short sentence.",
        "images": [base64.b64encode(buf.getvalue()).decode()],
        "stream": False, "options": {"num_predict": 60}}).encode()
    req = urllib.request.Request(OLLAMA, data=body, headers={"Content-Type": "application/json"})
    return json.loads(urllib.request.urlopen(req, timeout=300).read()).get("response", "")


_d1 = {}
def d1():
    if 'm' not in _d1:
        import torch
        from transformers import AutoModel
        dev = "cuda" if torch.cuda.is_available() else "cpu"
        dt = torch.bfloat16 if dev == "cuda" else torch.float32
        _d1['m'] = AutoModel.from_pretrained(D1DIR, trust_remote_code=True, dtype=dt).to(dev)
        _d1['t'] = __import__('torch')
        _d1['dev'] = dev
        print('d1 on', dev, flush=True)
    return _d1['m']


def d1_classify(tiles, ref, prompt):
    import torch
    m = d1()
    q = 'Task: %s. Does this candidate photo satisfy the task?' % prompt
    reqs = []
    for i in range(len(tiles)):
        imgs = ([ref] + [tiles[i]]) if ref else [tiles[i]]
        reqs.append((None, {"t%d" % i: {"type": "noul", "instructions": q}}, imgs))
    with torch.inference_mode():
        res = m.system_one_batch(reqs)
    return [res[i]["answers"]["t%d" % i]["noul"] for i in range(len(tiles))]


WORDS = {'car': ['car', 'cars', 'taxi', 'vehicle', 'automobile', 'sedan', 'suv'],
    'bus': ['bus', 'buses', 'coach'], 'truck': ['truck', 'trucks', 'lorry', 'pickup'],
    'motorcycle': ['motorcycle', 'motorbike', 'scooter', 'moped'],
    'bicycle': ['bicycle', 'bike', 'cyclist'], 'train': ['train', 'tram', 'subway', 'railway'],
    'boat': ['boat', 'ship', 'sailboat', 'yacht', 'ferry'], 'airplane': ['airplane', 'plane', 'aircraft', 'jet'],
    'dog': ['dog', 'puppy', 'dogs'], 'cat': ['cat', 'cats', 'kitten'],
    'bird': ['bird', 'birds', 'duck', 'owl', 'eagle'], 'horse': ['horse', 'horses'],
    'animal': ['dog', 'cat', 'bird', 'horse', 'animal', 'cow', 'sheep', 'elephant', 'bear', 'zebra', 'giraffe']}


def solve(url, port=9350, budget=420, backend='d1'):
    nokk = subprocess.Popen([NOKK, '--port', str(port)],
                            stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    time.sleep(3)
    t_end = time.time() + budget
    try:
        ver = json.loads(urllib.request.urlopen('http://127.0.0.1:%d/json/version' % port, timeout=15).read())
        c = CDP(ver['webSocketDebuggerUrl'])
        tgt = c.call('Target.createTarget', {'url': 'about:blank'})['targetId']
        sess = c.call('Target.attachToTarget', {'targetId': tgt, 'flatten': True})['sessionId']
        call = lambda m, p=None, timeout=120: c.call(m, p, timeout, sess)
        call('Page.navigate', {'url': url})
        time.sleep(10)
        # trigger via official API (DOM checkbox clicks are ignored by bundle)
        call('Runtime.evaluate', {'expression': 'hcaptcha.execute()', 'returnByValue': True}, timeout=60)
        for crumb in range(3):
            if time.time() > t_end:
                return {'solved': False, 'why': 'budget'}
            frames = call('Nokk.frames')['frames']
            chal = [f for f in frames if 'frame=challenge' in f.get('url', '')]
            if not chal:
                time.sleep(5); continue
            ctx = chal[0]['executionContextId']
            # wait for tiles
            data = None
            for _ in range(12):
                r = call('Runtime.evaluate', {'contextId': ctx, 'expression': EXTRACT, 'returnByValue': True}, timeout=60)
                data = json.loads(r['result']['value'])
                if data['tiles'] and all(data['tiles']):
                    break
                time.sleep(5)
            if not data or not data['tiles'] or not all(data['tiles']):
                # maybe passive pass (no challenge)?
                tok = call('Runtime.evaluate', {'expression': TOKEN, 'returnByValue': True}, timeout=60)['result']['value']
                if tok:
                    return {'solved': True, 'token': tok, 'crumbs': crumb}
                time.sleep(5); continue
            prompt = data['prompt']
            print('CRUMB %d PROMPT: %s' % (crumb, prompt), flush=True)
            tiles = []
            for u in data['tiles']:
                im = Image.open(io.BytesIO(requests.get(u, timeout=30).content)).convert('RGB')
                tiles.append(im.resize((384, 384)))
            ref = None
            if data.get('ref'):
                try:
                    ref = Image.open(io.BytesIO(requests.get(data['ref'], timeout=30).content)).convert('RGB').resize((384, 384))
                    print('ref image ok', flush=True)
                except Exception as e:
                    print('ref fetch failed:', e, flush=True)
            if backend == 'd1':
                t0 = time.time()
                scores = d1_classify(tiles, ref, prompt)
                print('scores:', ['%.2f' % s for s in scores], '(%.0fs)' % (time.time()-t0), flush=True)
                order = sorted(range(len(scores)), key=lambda i: -scores[i])
                conf = [i for i in order if scores[i] >= 0.60]
                if 1 <= len(conf) <= 6:
                    picks = conf
                elif not conf:
                    picks = order[:1]
                else:
                    picks = order[:4]
            else:
                picks = []
                for i, im in enumerate(tiles):
                    desc = ollama_describe(im, prompt)
                    hit = any(w in desc.lower() for ws in WORDS.values() for w in ws)
                    print('tile %d: %s -> %s' % (i, desc.strip()[:80], hit), flush=True)
                    if hit:
                        picks.append(i)
            print('PICKS:', picks, flush=True)
            if not picks:
                print('no confident picks, skipping submit', flush=True)
                call('Runtime.evaluate', {'contextId': ctx, 'expression': 'document.querySelector(".button-skip,.skip").click()', 'returnByValue': True}, timeout=60)
                time.sleep(6); continue
            call('Runtime.evaluate', {'contextId': ctx, 'expression': TAG, 'returnByValue': True}, timeout=60)
            for i in picks:
                if time.time() > t_end:
                    return {'solved': False, 'why': 'budget'}
                print('press tile', i, call('Nokk.press', {'frameUrl': 'frame=challenge', 'selector': '#tile%d' % i}), flush=True)
                time.sleep(1.5)
            time.sleep(2)
            print('submit:', call('Nokk.press', {'frameUrl': 'frame=challenge', 'selector': '.button-submit'}), flush=True)
            time.sleep(8)
            tok = call('Runtime.evaluate', {'expression': TOKEN, 'returnByValue': True}, timeout=60)['result']['value']
            if tok:
                return {'solved': True, 'token': tok + '...', 'crumbs': crumb + 1}
        tok = call('Runtime.evaluate', {'expression': TOKEN, 'returnByValue': True}, timeout=60)['result']['value']
        return {'solved': bool(tok), 'token': (tok[:40] + '...') if tok else ''}
    finally:
        try: c.close()
        except Exception: pass
        nokk.terminate()


if __name__ == '__main__':
    url = sys.argv[1] if len(sys.argv) > 1 else 'https://nopecha.com/demo/hcaptcha'
    be = sys.argv[2] if len(sys.argv) > 2 else 'd1'
    print(json.dumps(solve(url, backend=be)), flush=True)
