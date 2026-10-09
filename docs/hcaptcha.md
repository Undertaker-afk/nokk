# hCaptcha Enterprise

hCaptcha's Enterprise score reads the whole interaction, not just the token:
pointer trail shape, press timing, locale coherence, and exit-IP reputation.

## What the engine does

- Checkbox press: arc approach (18-25 points), pause, held press, drift after.
  The seed is per-session — the same target never repeats the same trail.
  Tile clicks reuse the same `human_press` path (see `crates/core/src/lib.rs`),
  so grid solves carry a trail, not a teleport.
- Slider / crop drags: `dispatch_drag` (trail in, press, ~15 ms stepped moves
  along a slight arc, release, micro-correction). A teleport fails the score.
- Typing sends a real `KeyboardEvent.code` per char (`KeyA`, `Digit1`,
  `Space`, …), never an empty code.
- `--geoip-timezone` derives **both** `navigator.languages` and the wire
  `Accept-Language` from the same exit-IP lookup, atomically per context.
- Mac preset reports `devicePixelRatio` 2; `deviceMemory` comes from the
  preset, never the host.

## Solver quality (`nokk-captcha`)

All helpers are offline/deterministic (no network); see
`crates/nokk-captcha/src/lib.rs` + `taxonomy.rs`.

- **Taxonomy top-50** (`taxonomy::LABELS`): 10 vehicles, 20 animals,
  20 street/scene (`traffic_light`, `pedestrian`, `bridge`, `building`,
  `tree`, `mountain`, `water`, `road`, `stop_sign`, `crosswalk`, … plus
  `taxi`, `bicycle_rack`, `motorbus_stop`). Aliases cover plurals and
  US/UK variants (`lorries→truck`, `aeroplane→airplane`,
  `traffic lights→traffic_light`, `bus stop→motorbus_stop`,
  `people→pedestrian`). `normalize_prompt` lowercases, strips instruction
  verbs/articles/punctuation, maps to `snake_case`, with whole-string and
  last-word plural back-off. Unknown prompts stay unknown — never guessed.
- **Per-label calibrated thresholds** (`threshold_for`): 0.55 for
  high-frequency (`car/dog/cat`), 0.60 for common vehicles + street
  (`truck/bus/pedestrian/traffic_light`), 0.65 medium, 0.70 confusable,
  0.75 rarest scene props, **0.80 strict default**. Selection
  (`decide_indices`) is fail-closed: non-finite/out-of-range thresholds,
  `NaN` scores, or sub-threshold rows return empty — never a guess.
- **2-pass Verify re-read**: hCaptcha swaps solved tiles for fresh ones
  after the first Verify. Pass 1 *always* re-reads the grid
  (`needs_reread_after_verify(1, _)`); pass 2 re-reads only when tile URLs
  moved (`tiles_changed`). Pass 3+ never re-reads — the loop stops
  (`MAX_VERIFY_PASSES = 2`).
- **Crumb loop** (`CrumbLoop`, `parse_pass_flag`): up to `MAX_CRUMBS = 3`
  getcaptcha/checkcaptcha rounds until `pass:true`. Parses
  `{"pass":…}`, `{"result":{"pass":…}}`, `{"data":{"pass":…}}`, plus bare
  `pass:true/false` fragments; missing flag → `None` (fail closed).
- **`rqdata` passthrough** (`HcaptchaConfig::with_rqdata`, `query_pairs`):
  the token is sent verbatim when present, omitted when absent — never
  logged or mutated.
- **English `hl` forcing** (`effective_hl`, `force_english`,
  `frame_url_with_hl`): taxonomy is English-trained, so non-English `hl`
  collapses to `en`; frame URLs are rewritten (`hl=en`) so prompts come
  back normalizable.
- **Invisible mode** (`is_invisible_anchor_src`,
  `page_requests_invisible`, `execute_trigger_js`, plus
  `__pt_gateInfo.hcaptcha_invisible` / `__pt_hcaptchaMode()` in
  `dom_runtime.js`): `size=invisible` anchors and `hcaptcha.execute(`
  call sites are detected; the driver triggers `hcaptcha.execute()`
  instead of pressing a checkbox. Token detection still reads
  `h-captcha-response` via `__pt_widgetToken`.
- **Stale-solve cleanup** (`SolveFreshness`, `stale_solve_cleanup_js`):
  solves older than `STALE_SOLVE_MS` (120 s) are dropped, and the latched
  `__pt_setPressTarget` is cleared between attempts so the next press
  re-resolves its element.

## Proxies: use residential

A datacenter IP with the default `en-US` locale is high-risk: the score sees
an IP whose ASN is a hosting provider claiming a generic US user. Prefer a
residential proxy in the target market and pass `--geoip-timezone` so the
timezone + locale match the exit IP:

```bash
nokk --load https://gated.example/ --solve-challenge 30 \
  --proxy http://user:pass@residential.example:8080 --geoip-timezone
```

Without a proxy `--geoip-timezone` is a no-op.

## Image/label tasks

Checkbox-only flows clear via `solve_challenge` press + token poll. Once the
widget shows its task view (`.task-image` / `.challenge-view`), the engine
reports `needs-human` — the ONNX tile classifier (`vision-onnx`, manifest in
`crates/nokk-captcha/models/`) is opt-in and ships no weights. Tile
collection is opt-in via `NOKK_HCAPTCHA_DUMP_DIR` (see
`tools/hcaptcha/collector.md`); dumps dedupe by SHA-256 and never log URLs,
IPs, or tokens.

`area_select` / slider / crop / drag variants are **unsupported by design** in
the default build (`nokk-captcha::is_supported_request_type` returns false for
anything but `image_label_binary`; `HcaptchaTask::decide_for_task` selects
nothing for them even at perfect scores). The driver surfaces them as
`ChallengeStatus::NeedsHuman` after press + 2 s puzzle check — the same path
as the image grid — and never guesses coordinates. `dispatch_drag` exists for
a human operator's own use, not for engine guesses: any teleport fails the
enterprise score and burns the sitekey's rate limit.

## Second target: RoyalMail Next-button flow

First target is the nopecha demo enterprise key
(`58366d97-3e8c-4b57-a679-4a41c8423be3`); the second is RoyalMail's help
portal, which embeds the widget in a Salesforce resource iframe with a
compact layout and a `Next` button that posts the verdict to its parent:

- Resource (version in the path rotates — treat as stale signal, live DOM is
  ground truth):
  `https://help.royalmail.com/resource/1736330734000/RMG_HCaptcha_Next`
- Widget (at probe time): `<div class="h-captcha" data-size="compact"
  data-sitekey="0a3e79fc-62f5-4bd8-ba87-710a4b81ac4b">` + `<button
  …>Next</button>` + `validateForm()` posting `"captcha success"` /
  `"captcha failed"` to the parent; `h-captcha-response` must be non-empty
  for the pass.
- Steps:
  1. `nokk --load https://help.royalmail.com/ --eval
     "document.body.innerHTML.slice(0,4000)"` — find the live iframe src.
  2. `nokk --load <iframe-url> --solve-challenge 30 [--proxy … --geoip-timezone]`
     — press the checkbox, then `--click "button[type=submit]"` (Next).
  3. Assert the parent receives `"captcha success"`.
- Expect `TokenIssued` then Next navigates. Run at most once per case, ≥60 s
  apart; stop on first `NeedsHuman` / `Timeout` (see
  `tools/hcaptcha/LIVE_TEST.md` cases E–G).

Blocklist note: `crates/net/src/blocklist.rs` hard-allowlists `hcaptcha.com`
plus the explicit challenge hosts `js.hcaptcha.com` and
`newassets.hcaptcha.com` (suffix match also covers `frames.` / `api.` / `c.`
subdomains). Do not enable strict tracker blocking against these hosts
during tests — blocked `api.js` / `/captcha/*` shows up as `challenge_state`
stuck at `hcaptcha && !token` → `Timeout`. Confirm with `--dump-requests`.

## Remaining gaps

- No bundled weights: `hcaptcha-v1` manifest entry is a placeholder.
- Non-English prompts outside `hl=en` still normalize best-effort only.
- Proof-of-work / device-attestation variants (Enterprise) are out of scope.
