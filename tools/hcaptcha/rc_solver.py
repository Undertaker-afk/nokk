"""reCAPTCHA v2 image solver: nokk (CDP) + d1-3B. Usage: python rc_solver.py [url]."""
import io, json, subprocess, sys, time, urllib.request
import requests
sys.path.insert(0, r'C:\Users\THW-User\Desktop\nokk\tools\hcaptcha')
from cdp import CDP
from PIL import Image

NOKK = r'C:\Users\THW-User\Desktop\nokk\target\debug\nokk.exe'
D1DIR = r'C:\Users\THW-User\AppData\Local\Temp\opencode\models\d1-3B'
UA = {'User-Agent': 'Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/148.0.0.0 Safari/537.36',
      'Referer': 'https://www.google.com/recaptcha/api2/bframe'}

EXTRACT = """(() => { try {
  const tiles = [...document.querySelectorAll('.rc-imageselect-tile')];
  const q = document.querySelector('.rc-imageselect-instructions');
  return JSON.stringify({
    prompt: q ? String(q.textContent || '').replace(/\\s+/g, ' ').slice(0,200) : null,
    tiles: tiles.map(el => { const img = el.querySelector('img');
      return img ? String(img.getAttribute('src') || '') : null; }),
    verify: !!document.querySelector('#recaptcha-verify-button'),
  });
} catch (e) { return 'JSERR:' + (e && e.message); } })()"""
TAG = "document.querySelectorAll('.rc-imageselect-tile').forEach((el,i)=>{el.id='vtile'+i});'tagged'"
TOKEN = """(() => { const i = document.querySelector('[name=g-recaptcha-response]'); return i ? i.value.slice(0,40) : ''; })()"""

_d1 = {}
def d1():
    if 'm' not in _d1:
        import torch
        from transformers import AutoModel
        dev = "cuda" if torch.cuda.is_available() else "cpu"
        dt = torch.bfloat16 if dev == "cuda" else torch.float32
        _d1['m'] = AutoModel.from_pretrained(D1DIR, trust_remote_code=True, dtype=dt).to(dev)
        _d1['dev'] = dev
        print('d1 on', dev, flush=True)
    return _d1['m']

def d1_classify(tiles, prompt):
    import torch
    m = d1()
    q = 'Task: %s. Does this candidate photo satisfy the task?' % prompt
    reqs = [(None, {"t%d" % i: {"type": "noul", "instructions": q}}, [tiles[i]]) for i in range(len(tiles))]
    with torch.inference_mode():
        res = m.system_one_batch(reqs)
    return [res[i]["answers"]["t%d" % i]["noul"] for i in range(len(tiles))]

def solve(url, port=9380, budget=420):
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
        call('Nokk.press', {'frameUrl': 'api2/anchor', 'selector': '.recaptcha-checkbox'})
        rounds = 0
        while time.time() < t_end and rounds < 8:
            frames = call('Nokk.frames')['frames']
            bf = [f for f in frames if 'bframe' in f.get('url', '')]
            if not bf:
                time.sleep(3); continue
            ctx = bf[0]['executionContextId']
            ev = lambda e: call('Runtime.evaluate', {'contextId': ctx, 'expression': e, 'returnByValue': True}, timeout=60)['result']['value']
            data = None
            for _ in range(10):
                raw = ev(EXTRACT)
                if raw.startswith('JSERR'):
                    time.sleep(3); continue
                data = json.loads(raw)
                if data['tiles'] and all(data['tiles']):
                    break
                time.sleep(4)
            tok = call('Runtime.evaluate', {'expression': TOKEN, 'returnByValue': True}, timeout=60)['result']['value']
            if tok:
                return {'solved': True, 'token': tok + '...', 'rounds': rounds}
            if not data or not data['tiles'] or not all(data['tiles']):
                time.sleep(4); continue
            prompt = data['prompt']
            print('ROUND %d PROMPT: %s' % (rounds, prompt), flush=True)
            tiles = []
            ok = True
            import hashlib, os
            os.makedirs(r'C:\Users\THW-User\AppData\Local\Temp\opencode\rc-tiles', exist_ok=True)
            raws = []
            for u in data['tiles']:
                try:
                    r = requests.get(u, headers=UA, timeout=25)
                    r.raise_for_status()
                    h = hashlib.sha256(r.content).hexdigest()[:12]
                    print('tile bytes:', len(r.content), h, u[-30:], flush=True)
                    im = Image.open(io.BytesIO(r.content)).convert('RGB')
                    im.save(r'C:\Users\THW-User\AppData\Local\Temp\opencode\rc-tiles\r%d_%s.png' % (rounds, h))
                    raws.append(im)
                except Exception as e:
                    print('tile fetch failed:', str(e)[:100], flush=True)
                    ok = False; break
            if not ok:
                time.sleep(4); continue
            uniq = {}
            for im in raws:
                uniq.setdefault(hashlib.sha256(im.tobytes()).hexdigest(), im)
            if len(uniq) == 1 and len(raws) in (9, 16):
                # static composite variant: one shared image, tiles are grid crops
                n = int(len(raws) ** 0.5)
                base = raws[0]
                W, H = base.size
                print('composite %dx%d split %dx%d' % (W, H, n, n), flush=True)
                tiles = [base.crop((c*W//n, r*H//n, (c+1)*W//n, (r+1)*H//n)).resize((384, 384))
                         for r in range(n) for c in range(n)]
            else:
                tiles = [im.resize((384, 384)) for im in raws]
            t0 = time.time()
            scores = d1_classify(tiles, prompt)
            print('scores:', ['%.2f' % s for s in scores], '(%.0fs)' % (time.time()-t0), flush=True)
            order = sorted(range(len(scores)), key=lambda i: -scores[i])
            conf = [i for i in order if scores[i] >= 0.60]
            spread = max(scores) - min(scores)
            if spread < 0.05:
                # shared-payload variant: identical pixels -> all-or-nothing
                picks = list(range(len(scores))) if sum(scores)/len(scores) >= 0.60 else []
                print('shared-image: mean=%.2f picks=%d tiles' % (sum(scores)/len(scores), len(picks)), flush=True)
            elif max(scores) < 0.60:
                picks = []  # nothing looks like the target: verify empty (dynamic flow)
            elif 1 <= len(conf) <= 6:
                picks = conf
            elif not conf:
                picks = order[:1]
            else:
                picks = order[:4]
            print('PICKS:', picks, flush=True)
            ev(TAG)
            for i in picks:
                if time.time() > t_end:
                    return {'solved': False, 'why': 'budget'}
                call('Nokk.press', {'frameUrl': 'bframe', 'selector': '#vtile%d' % i})
                time.sleep(1.2)
            try:
                st = ev("[...document.querySelectorAll('.rc-imageselect-tile')].map(el => (el.querySelector('.rc-imageselect-checkbox')||{}).className || '').join(' | ')")
                print('TOGGLE-CHECK:', st.encode('ascii','replace').decode()[:300], flush=True)
            except Exception as e:
                print('toggle-check failed:', str(e)[:100], flush=True)
            time.sleep(2)
            print('verify:', call('Nokk.press', {'frameUrl': 'bframe', 'selector': '#recaptcha-verify-button'}), flush=True)
            time.sleep(8)
            rounds += 1
        tok = call('Runtime.evaluate', {'expression': TOKEN, 'returnByValue': True}, timeout=60)['result']['value']
        return {'solved': bool(tok), 'token': (tok[:40] + '...') if tok else '', 'rounds': rounds}
    finally:
        try: c.close()
        except Exception: pass
        nokk.terminate()

if __name__ == '__main__':
    print(json.dumps(solve(sys.argv[1] if len(sys.argv) > 1 else 'https://www.google.com/recaptcha/api2/demo')), flush=True)
