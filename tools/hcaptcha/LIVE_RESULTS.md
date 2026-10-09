# hCaptcha Enterprise Live Probe — Raw Facts Only (no solving, no bypass)
Date (UTC): 2026-10-08
Probe method: WebFetch + curl.exe via PowerShell (desktop UA), exa fetch. No token submission.

## 1. https://nopecha.com/demo/hcaptcha (and #enterprise)

### 1.1 Static HTML (`GET /demo/hcaptcha`)
- Status: `HTTP/1.1 200 OK` (Cloudflare `cf-cache-status: HIT`, `Age: 2827`)
- Headers: `Content-Type: text/html; charset=utf-8`, `Transfer-Encoding: chunked`, `Server: cloudflare`, `Cache-Control: max-age=14400`
- Body size: `575 bytes` (`curl SIZE:575`, `TYPE:text/html; charset=utf-8`)
- `#enterprise` fragment: server response identical (fragment is client-side only, not sent to server). WebFetch of `/demo/hcaptcha` and `/demo/hcaptcha#enterprise` returned identical HTML.
- Raw HTML (truncated to body-relevant tags):
```html
<html><head><title>NopeCHA - hCaptcha Demo</title><style>...</style><script src="/js/layout.js"></script><script src="/js/demo/hcaptcha.js?v=2"></script><script src="https://js.hcaptcha.com/1/api.js" async="async" defer="defer"></script></head><body><div id="parent"></div></body></html>
```
- Sitekey in static HTML: NONE (no `data-sitekey`, no `h-captcha` div). Widget container is empty `<div id="parent"></div>`, populated by JS.
- Widget params in static HTML: NONE (`hl` absent, `rqdata` absent, `data-size` absent, `data-theme` absent).
- hCaptcha API loader: `https://js.hcaptcha.com/1/api.js` with `async defer`.

### 1.2 Demo JS (`GET /js/demo/hcaptcha.js?v=2`)
- Full URL: `https://nopecha.com/js/demo/hcaptcha.js?v=2`
- Status: `HTTP 200`, `SIZE:4228 bytes`, `TYPE:application/javascript; charset=UTF-8`
- Supporting file `/js/layout.js`: `HTTP 200`, `SIZE:29830 bytes`, `TYPE:application/javascript; charset=UTF-8`
- `HEAD https://js.hcaptcha.com/1/api.js`: `HTTP/1.1 405 Method Not Allowed` (expected — GET-only endpoint; `Server: cloudflare`). Not a blocker.
- Active `CAPTCHAS` entry (uncommented):
  - `hash: '#enterprise'`
  - `label: 'hCaptcha Enterprise'`
  - `sitekey: '58366d97-3e8c-4b57-a679-4a41c8423be3'` (source comment: `https://www.familytreenow.com/InternalCaptcha`)
  - `callback: 'on_token_3'`, `response: 'token_3'`
  - `on_token_3(token)` only sets `#token_3` to `success` (no verify POST), unlike `on_token_4/5` which POST to `https://<SUB_API>.nopecha.com/captcha/verify/hcaptcha` with `{token, type:'enterprise'}`.
- Commented-out entries (present in file but inactive):
  - `b4c45857-0e23-48e6-9017-e28fff99ffb2` (`#easy`, publisher)
  - `2c823188-d286-4a5e-9d7e-c0c9290393f6` (`#moderate`, publisher)
  - `ab803303-ac41-41aa-9be1-7b4e01b91e2c` (`#hard`, publisher)
  - `f5561ba9-8f1e-40ca-9b5b-a0b3f719ef34` (`#enterprise`, Discord, `//-` commented)
  - `20000000-ffff-ffff-ffff-000000000002` (`#enterprise_human` test-human)
  - `30000000-ffff-ffff-ffff-000000000003` (`#enterprise_bot` test-bot)
  - `f8be1023-63ce-44e8-9109-2bc2482ab9fd` (`#enterprise` Enterprise 2, inbox.eu)
- Widget construction (`create_captcha(e)`):
```js
$captcha.classList.add('g-recaptcha')
$captcha.dataset.sitekey = e.sitekey
$captcha.dataset.callback = e.callback
```
  - Widget mode: default checkbox (no `data-size="invisible"`, no `size`, no `hl`, no `rqdata`, no `theme` set anywhere in file — verified by grep for `hl|rqdata|size|invisible|theme`: only `font-size` in CSS and `sitekey/callback` matches).
  - `hl`: ABSENT
  - `rqdata`: ABSENT
  - Hash routing: `DOMContentLoaded` reads `window.location.hash`; if hash set, only entries with matching `e.hash` render. So `#enterprise` renders only the single active enterprise widget.
- Blockers hit: NONE for static fetch (WebFetch OK, curl OK, exa rendered title + `hCaptcha Enterprise` label).

## 2. https://help.royalmail.com/resource/1736330734000/RMG_HCaptcha_Next

### 2.1 Static HTML
- Status: `HTTP/1.1 200 OK` (`X-SFDC-Edge-Cache: none`, `Server: sfdcedge`)
- Headers:
```
Content-Type: text/html;charset=UTF-8
Content-Length: 1010
Last-Modified: Wed, 8 Jan 2025 10:05:34 GMT
Cache-Control: public,max-age=3888000,immutable
X-FRAME-OPTIONS: SAMEORIGIN
Content-Security-Policy: frame-ancestors 'self'; upgrade-insecure-requests
Cross-Origin-Resource-Policy: cross-origin
Referrer-Policy: strict-origin-when-cross-origin
```
- Body: `Content-Length: 1010 bytes`; file with headers on disk `2083 bytes`.
- Full raw body:
```html
<html>
  <head>
    <title>hCaptcha Demo</title>
    <script src="https://js.hcaptcha.com/1/api.js" async defer></script>
  </head>
  <body>
    <form action="" method="POST" onsubmit="return validateForm()">
        <div class="h-captcha" data-size="compact" data-sitekey="0a3e79fc-62f5-4bd8-ba87-710a4b81ac4b"></div>
        <br />
        <button class="slds-button slds-button_brand" style="padding: 12px 24px; cursor:pointer; border-radius: 100px; background-color: #da202a; border: none; color: white; font-size: 16px; line-height: 22px;" type="submit">Next</button>
    </form>
    <script type="text/javascript">
        function validateForm(){
            if(hcaptcha.getResponse().length == 0){
              alert('Please click the hCaptcha checkbox');
              parent.postMessage("captcha failed", location.origin);
              return false;
            }
            parent.postMessage("captcha success", location.origin);
            return true;
        }
    </script>
  </body>
</html>
```

### 2.2 Widget params
- `data-sitekey="0a3e79fc-62f5-4bd8-ba87-710a4b81ac4b"` — STATIC inline in HTML (not JS-injected; no JS builds the div).
- `data-size="compact"` — compact checkbox widget (NOT `invisible`, NOT `normal`).
- Widget mode: visible checkbox (`compact`).
- `hl`: ABSENT (no `hl`, no `data-hl`).
- `rqdata`: ABSENT (no `rqdata`, no `data-rqdata`).
- `class="h-captcha"` (native hCaptcha class, vs nopecha demo's `g-recaptcha` class).
- API loader: `<script src="https://js.hcaptcha.com/1/api.js" async defer></script>` static tag.

### 2.3 validateForm / postMessage flow
- Form: `<form action="" method="POST" onsubmit="return validateForm()">` with submit button `Next` (Salesforce Lightning `slds-button_brand` styling).
- `validateForm()`:
  - `if (hcaptcha.getResponse().length == 0)` → `alert('Please click the hCaptcha checkbox')`, `parent.postMessage("captcha failed", location.origin)`, `return false` (blocks submit).
  - else → `parent.postMessage("captcha success", location.origin)`, `return true`.
- No token is posted to a backend in this static file; result is communicated to parent frame via `postMessage` (`"captcha success"` / `"captcha failed"`, targetOrigin `location.origin`).
- Intended embedding: iframe (consistent with `X-FRAME-OPTIONS: SAMEORIGIN` + `frame-ancestors 'self'` — must be framed same-origin / Salesforce Experience Cloud).

### 2.4 Blockers hit
- `WebFetch` direct GET: `Transport error` (failed).
- `exa_web_fetch_exa`: succeeded minimally, rendered only `hCaptcha Demo / Next` (JS/widget not executed, no sitekey extracted by reader).
- `curl.exe` with desktop UA (`Mozilla/5.0 ... Chrome/126.0`): `200 OK`, full HTML retrieved. No auth, no JS-challenge hit for this static resource.
- No solving or token submission attempted.

## Summary Table
| Target | HTTP | Size | Sitekey | Size param | Mode | hl | rqdata | Sitekey delivery |
|---|---|---|---|---|---|---|---|---|
| nopecha.com/demo/hcaptcha (+#enterprise) | 200 | 575 B HTML; 4228 B hcaptcha.js?v=2 | `58366d97-3e8c-4b57-a679-4a41c8423be3` active (#enterprise) | none | checkbox default | absent | absent | JS-injected (`create_captcha`, `dataset.sitekey`) |
| help.royalmail.com/.../RMG_HCaptcha_Next | 200 | 1010 B body | `0a3e79fc-62f5-4bd8-ba87-710a4b81ac4b` | `compact` | checkbox compact | absent | absent | static inline `data-sitekey` |

## Re-probe 2026-10-08 (WebFetch + exa, facts only, no solving/submission)
- `GET https://nopecha.com/demo/hcaptcha` → `200 OK` via WebFetch. HTML: `<title>NopeCHA - hCaptcha Demo</title>`, `<div id="parent"></div>`, scripts `/js/layout.js`, `/js/demo/hcaptcha.js?v=2`, `https://js.hcaptcha.com/1/api.js async defer`. No `data-sitekey`, no `hl`/`rqdata` in static HTML.
- `GET https://nopecha.com/js/demo/hcaptcha.js?v=2` → `200 OK` via WebFetch (full text retrieved). Active entry unchanged: `hash '#enterprise'`, `label 'hCaptcha Enterprise'`, `sitekey '58366d97-3e8c-4b57-a679-4a41c8423be3'`, `callback 'on_token_3'`. Widget mode: default checkbox via `$captcha.classList.add('g-recaptcha')` + `dataset.sitekey/callback` only — no `size/hl/rqdata/theme/invisible`. Hash routing: `DOMContentLoaded` filters `CAPTCHAS` by `window.location.hash`.
- `GET https://help.royalmail.com/resource/1736330734000/RMG_HCaptcha_Next` → `200 OK` via WebFetch (full HTML retrieved, no transport error this run). `data-sitekey="0a3e79fc-62f5-4bd8-ba87-710a4b81ac4b"`, `data-size="compact"`, `class="h-captcha"`, loader `https://js.hcaptcha.com/1/api.js async defer`. `hl`/`rqdata` absent. `validateForm()`: `hcaptcha.getResponse().length==0` → `alert + parent.postMessage("captcha failed", location.origin) + return false`; else `parent.postMessage("captcha success", location.origin) + return true`. Form `<form action="" method="POST" onsubmit="return validateForm()">` + `Next` button.
- exa fetch: both URLs `200`, rendered only `hCaptcha Enterprise / More info` and `hCaptcha Demo / Next` (JS not executed, no extra params).
- Blockers this run: NONE (both WebFetch + exa succeeded; no auth/JS-challenge hit).
