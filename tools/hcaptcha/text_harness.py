"""Self-hosted text captcha harness: distorted+noisy PNGs, known answers, /verify endpoint."""
import io, json, random, string, threading
from http.server import BaseHTTPRequestHandler, HTTPServer
from urllib.parse import urlparse, parse_qs
from PIL import Image, ImageDraw, ImageFont

STORE = {}
ALPHA = string.ascii_uppercase + string.digits

def make_text(rng):
    return ''.join(rng.choice(ALPHA) for _ in range(rng.randint(4, 6)))

def make_png(text, seed):
    rng = random.Random(seed)
    W, H = 220, 70
    img = Image.new('RGB', (W, H), (255, 255, 255))
    d = ImageDraw.Draw(img)
    try:
        font = ImageFont.truetype("arial.ttf", 44)
    except Exception:
        font = ImageFont.load_default()
    x = 12
    for ch in text:
        d.text((x, rng.randint(8, 20)), ch, font=font,
               fill=(rng.randint(0, 80), rng.randint(0, 80), rng.randint(0, 80)))
        x += rng.randint(28, 36)
    for _ in range(rng.randint(2, 4)):  # strikethrough arcs
        x0, y0 = rng.randint(0, W), rng.randint(0, H)
        d.arc([x0, y0, x0 + rng.randint(40, 120), y0 + rng.randint(20, 50)],
              rng.randint(0, 360), rng.randint(0, 360), fill=(120, 120, 120))
    for _ in range(250):  # salt noise
        d.point((rng.randint(0, W - 1), rng.randint(0, H - 1)), fill=(180, 180, 180))
    buf = io.BytesIO()
    img.save(buf, format='PNG')
    return buf.getvalue()

PAGE = """<!DOCTYPE html><html><body>
<h2>Local text captcha</h2>
<img id="cap" src="/img?id=%s">
<input id="answer" type="text">
<button id="go">Verify</button>
<div id="verdict"></div>
<script>document.getElementById('go').onclick = async () => {
  const a = document.getElementById('answer').value;
  const r = await fetch('/verify?id=%s&guess=' + encodeURIComponent(a)).then(r => r.text());
  document.getElementById('verdict').textContent = r; };</script>
</body></html>"""

class H(BaseHTTPRequestHandler):
    def log_message(self, *a):
        pass
    def do_GET(self):
        u = urlparse(self.path)
        q = parse_qs(u.query)
        if u.path == '/':
            cid = 'c%d' % random.randint(0, 10**9)
            STORE[cid] = make_text(random.Random())
            body = (PAGE % (cid, cid)).encode()
            self.send_response(200); self.send_header('Content-Type', 'text/html')
            self.send_header('Content-Length', str(len(body))); self.end_headers()
            self.wfile.write(body)
        elif u.path == '/img':
            cid = q.get('id', [''])[0]
            png = make_png(STORE.get(cid, '????'), hash(cid))
            self.send_response(200); self.send_header('Content-Type', 'image/png')
            self.send_header('Content-Length', str(len(png))); self.end_headers()
            self.wfile.write(png)
        elif u.path == '/verify':
            cid = q.get('id', [''])[0]
            ok = q.get('guess', [''])[0].upper() == STORE.get(cid, '')
            body = ('CORRECT' if ok else 'WRONG').encode()
            self.send_response(200); self.send_header('Content-Type', 'text/plain')
            self.send_header('Content-Length', str(len(body))); self.end_headers()
            self.wfile.write(body)
        else:
            self.send_response(404); self.end_headers()

if __name__ == '__main__':
    print('ANSWERS ARE SERVER-SIDE ONLY', flush=True)
    HTTPServer(('127.0.0.1', 9407), H).serve_forever()
