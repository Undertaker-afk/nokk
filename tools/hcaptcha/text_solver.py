"""Text captcha solver: extract -> ddddocr + TrOCR vote -> type -> submit -> verdict."""
import base64, io, json, subprocess, sys, time, urllib.request
sys.path.insert(0, r'C:\Users\THW-User\Desktop\nokk\tools\hcaptcha')
from cdp import CDP
from PIL import Image, ImageFile
ImageFile.LOAD_TRUNCATED_IMAGES = True

URL = 'https://captcha.com/demos/features/captcha-demo.aspx'
GETSRC = "document.getElementById('demoCaptcha_CaptchaImage').getAttribute('src')"
READVERDICT = "String(document.querySelector('#validationResult').textContent || '').slice(0,120)"
RELOAD = "document.getElementById('demoCaptcha_ReloadLink').click()"

def clean(s):
    return ''.join(ch for ch in s.upper() if ch.isalnum())

_trocr = {}
def trocr_read(img):
    if 'm' not in _trocr:
        import torch
        from transformers import VisionEncoderDecoderModel, TrOCRProcessor
        _trocr['p'] = TrOCRProcessor.from_pretrained(r"C:\Users\THW-User\AppData\Local\Temp\opencode\models\ocr-captcha-v3")
        _trocr['m'] = VisionEncoderDecoderModel.from_pretrained(r"C:\Users\THW-User\AppData\Local\Temp\opencode\models\ocr-captcha-v3")
        _trocr['dev'] = "cuda" if torch.cuda.is_available() else "cpu"
        _trocr['m'].to(_trocr['dev'])
        print('trocr on', _trocr['dev'], flush=True)
    import torch
    bg = Image.new("RGBA", img.size, (255, 255, 255))
    combined = Image.alpha_composite(bg, img.convert("RGBA")).convert("RGB")
    pv = _trocr['p'](combined, return_tensors="pt").pixel_values.to(_trocr['dev'])
    with torch.inference_mode():
        ids = _trocr['m'].generate(pv)
    return _trocr['p'].batch_decode(ids, skip_special_tokens=True)[0].strip()

def dddd_read(img):
    import ddddocr
    if not hasattr(dddd_read, 'o'):
        dddd_read.o = ddddocr.DdddOcr(show_ad=False)
    return dddd_read.o.classification(img).strip()

def connect(port):
    import urllib.request as _u
    ver = json.loads(_u.urlopen('http://127.0.0.1:%d/json/version' % port, timeout=15).read())
    c = CDP(ver['webSocketDebuggerUrl'])
    try:
        lst = json.loads(_u.urlopen('http://127.0.0.1:%d/json/list' % port, timeout=15).read())
    except Exception:
        lst = []
    pages = [t for t in lst if t.get('type') == 'page']
    if pages:
        tgt = pages[0]['id']
    else:
        tgt = c.call('Target.createTarget', {'url': 'about:blank'})['targetId']
    sess = c.call('Target.attachToTarget', {'targetId': tgt, 'flatten': True})['sessionId']
    call = lambda m, p=None, timeout=120: c.call(m, p, timeout, sess)
    return c, call

def solve(port=9395, rounds=4):
    nokk = subprocess.Popen(
        [r'C:\Users\THW-User\Desktop\nokk\target\debug\nokk.exe', '--port', str(port)],
        stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    time.sleep(3)
    c = None
    try:
        c, call = connect(port)
        call('Page.navigate', {'url': URL})
        time.sleep(10)
        wins = 0
        for rnd in range(rounds):
            src = call('Runtime.evaluate', {'expression': GETSRC, 'returnByValue': True}, timeout=60)['result']['value']
            img = Image.open(io.BytesIO(base64.b64decode(src.split(',', 1)[1]))).convert('RGB')
            img.load()
            a, b = clean(dddd_read(img)), clean(trocr_read(img))
            print('ROUND %d dddd=%r trocr=%r' % (rnd, a, b), flush=True)
            cands = ([a] if a == b else [a, b]) if (a or b) else []
            won = False
            for gi, guess in enumerate(cands):
                if not guess:
                    continue
                print('try %d: %r' % (gi, guess), flush=True)
                call('Runtime.evaluate', {'expression': "document.getElementById('captchaCode').value=''"}, timeout=60)
                call('Nokk.type', {'selector': '#captchaCode', 'text': guess})
                try:
                    call('Nokk.press', {'selector': '#validateCaptchaButton'})
                except Exception as e:
                    print('press-navigated:', str(e)[:80], flush=True)
                time.sleep(5)
                try: c.close()
                except Exception: pass
                time.sleep(1)
                c, call = connect(port)
                time.sleep(2)
                v = call('Runtime.evaluate', {'expression': READVERDICT, 'returnByValue': True}, timeout=60)['result']['value']
                print('verdict:', v.encode('ascii','replace').decode()[:150], flush=True)
                if 'correct' in v.lower() or 'success' in v.lower():
                    wins += 1; won = True
                    call('Page.navigate', {'url': URL})
                    time.sleep(8)
                    break
        return {'wins': wins, 'rounds': rounds}
    finally:
        try: c.close()
        except Exception: pass
        nokk.terminate()

if __name__ == '__main__':
    print(json.dumps(solve()), flush=True)
