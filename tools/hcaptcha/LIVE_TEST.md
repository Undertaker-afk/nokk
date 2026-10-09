# hCaptcha Solver — Live Test Plan (probe + plan only)

> Policy: probe reachability + plan only. Do NOT run full challenge solves
> back-to-back — hCaptcha rate-limits sitekeys aggressively. One manual run
> per case max, ≥60s apart, stop on first `NeedsHuman` / `Timeout`.

Probe date: 2026-10-08. Toolchain: cargo 1.99.0 / rustc 1.99.0 (win32).
Solver state: see §0.

## 0. Current solver state (read-only, not committed)

`git status --short --branch` (via `AppData\Local\Programs\Git\bin\git.exe`):

```
## main...origin/main
 M Cargo.lock
 M Cargo.toml
 M crates/cdp/src/server.rs
 M crates/core/src/lib.rs
 M crates/dom/src/dom_runtime.js
 M crates/net/src/blocklist.rs
 M crates/net/src/lib.rs
 M crates/stealth/src/lib.rs
?? crates/nokk-captcha/
?? docs/hcaptcha.md
?? tools/hcaptcha/
```

`git diff --stat`: 8 files, 619 insertions, 75 deletions (core `lib.rs` +534:
per-context `Accept-Language` pooling, geo cache, `press_seed`,
`dispatch_drag`; stealth locale coherence; net `accept_language`; blocklist
allowlist `hcaptcha.com`; `Cargo.toml` adds `crates/nokk-captcha`).

Untracked solver plumbing (new, default light, no weights):

- `crates/nokk-captcha/`: `lib.rs` (prompt normalize, `decide_indices`
  fail-closed, `HcaptchaTask` 3x3 tiles, `maybe_dump` gated on
  `NOKK_HCAPTCHA_DUMP_DIR`, `threshold_for`), `taxonomy.rs` (30 labels:
  10 vehicles + 20 animals, alias map, strict 0.80 default), `models/`
  (manifest `hcaptcha-v1`, ONNX behind `vision-onnx` feature).
- `docs/hcaptcha.md`: enterprise = pointer-trail + press-timing + locale
  coherence + exit-IP reputation; residential proxy + `--geoip-timezone`.
- `tools/hcaptcha/`: `taxonomy.yaml`, `train.py`, `collector.md`, `fixtures/`.

## 1. Build checks (recorded)

| Crate | Cmd | Result |
|---|---|---|
| `nokk-captcha` | `cargo check -p nokk-captcha` | **PASS** (exit 0, ~0.15s) |
| `nokk` (core) | `cargo check -p nokk` | **FAIL — environment, not code** |

Failure chain for `cargo check -p nokk`:

1. `btls-sys` build script: `can't run git: program not found` / `"git" "init" failed`
   → `git.exe` not on default `PATH` (found at
   `%LocalAppData%\Programs\Git\bin\git.exe`). Fixed by prepending it.
2. After fix: `cmake-0.1.58: failed to execute command: program not found —
   is cmake not installed?` (Visual Studio 17 2022 generator for BoringSSL).
   → Install CMake + VS C++ workload + libclang per `docs/BUILD.md`, then retry.

`nokk-captcha` passes because it has no native deps; `nokk` needs the full
BoringSSL build.

## 2. Network probes (no browser, `Invoke-WebRequest`, 20s timeout)

| URL | Result |
|---|---|
| `https://js.hcaptcha.com/1/api.js` | **200**, `application/javascript`, 362824 bytes, head `/* { "version": "1", "hash": "MEQC…" } */` → reachable |
| `https://nopecha.com/demo/hcaptcha` | **200**, `text/html`, 1651 bytes shell only — loads `/js/demo/hcaptcha.js?v=2` + `api.js` async |
| `https://nopecha.com/js/demo/hcaptcha.js?v=2` | **200**, 5103 bytes. **Target sitekey present: `58366d97-3e8c-4b57-a679-4a41c8423be3` = TRUE**. 8 UUID-like hits: `b4c45857-…` (easy), `2c823188-…` (moderate), `ab803303-…` (hard), `f5561ba9-…` (enterprise Discord, commented), `20000000-ffff-…-000000000002` (enterprise human test), `30000000-ffff-…-000000000003` (enterprise bot test), **`58366d97-…` (active enterprise, familytreenow)**, `f8be1023-…` (enterprise 2, inbox.eu, commented) |
| `https://help.royalmail.com/resource/1736330734000/RMG_HCaptcha_Next` | **200**, `text/html;charset=UTF-8`, 1010 bytes Content-Length. **Not** JS-shell-only — static fetch returns full iframe HTML: `<div class="h-captcha" data-size="compact" data-sitekey="0a3e79fc-62f5-4bd8-ba87-710a4b81ac4b">` + `<button …>Next</button>` + `validateForm()` posting `"captcha success"/"captcha failed"` to parent. Parent `help.royalmail.com/` itself is Salesforce-rendered — live `sitekey` + flow state must still come from live DOM via `nokk --load` (iframe may rotate resource version `1736330734000` / sitekey). |

## 3. Cases (run at most once each, manual)

Common harness:

```powershell
nokk --load <URL> --solve-challenge 30 --dump-requests
# with residential exit IP:
nokk --load <URL> --solve-challenge 30 `
  --proxy http://user:pass@residential.example:8080 --geoip-timezone
# probe live DOM without solving:
nokk --load <URL> --eval "document.documentElement.outerHTML.slice(0,4000)"
```

### A. Checkbox Enterprise (nopecha active key)

- URL: `https://nopecha.com/demo/hcaptcha#enterprise`
- Sitekey: `58366d97-3e8c-4b57-a679-4a41c8423be3`, callback `on_token_3`.
- Steps: `--load` + `--solve-challenge 30`. Expect: `press_widget_control`
  fires once (arc 18–25 pts + pause + held press), `challenge_state().kind ==
  hcaptcha-widget` → `TokenIssued`, `#token_3 = success`.
- Success: §5.

### B. Invisible `hcaptcha.execute()`

- URL: same demo, use test keys `20000000-…-000000000002` (human) /
  `30000000-…-000000000003` (bot) in a local repro page calling
  `hcaptcha.execute()` with no checkbox.
- Steps: `--load` + `--solve-challenge 30`. Expect: no pressable control;
  passive path only. Human key → `TokenIssued`; bot key → `Timeout` (do not
  retry — that is the correct negative).
- Success: §5. Records that solver does not fake an `execute()` token.

### C. Passive score (no puzzle)

- URL: case A with fresh residential session, no prior press.
- Steps: load, **do not** press (use `--load` alone, then
  `--eval "__pt_widgetToken()"`). Enterprise may issue token on reputation
  alone.
- Expect: `TokenIssued` with `presses == 0` on good IP; `NeedsHuman`/puzzle
  on datacenter IP. This isolates the `--geoip-timezone` + proxy effect.

### D. Image binary 3×3 (`image_label_binary`)

- URL: trigger puzzle (press checkbox on hard key `ab803303-…` or force
  challenge on enterprise key until grid appears).
- Steps: capture `HcaptchaTask { prompt_raw, prompt_norm, tiles[9],
  request_type="image_label_binary" }`; `normalize_prompt` → canonical
  (`taxonomy.yaml` / `taxonomy.rs`); `decide_indices_for_label(scores, label)`
  with per-label threshold (0.55 car/dog/cat … 0.80 unknown).
- Expect: solver either clicks high-confidence tiles or returns empty
  (fail-closed). `NOKK_HCAPTCHA_DUMP_DIR` set → dump record written, dedup by
  sha256. Vision model is opt-in (`vision-onnx`); default build must **not**
  guess.
- Success: §5 + dump file `<sha256>.json` when enabled.

### E. `area_select` / drag — expect `NeedsHuman`

- URL: any hCaptcha `area_select` (slider/crop) variant.
- Steps: `--load --solve-challenge 30`. Engine has `dispatch_drag`
  (trail-in, press, ~15ms stepped arc, release, micro-correction) but captcha
  layer has no area-select solver.
- Expect: `ChallengeStatus::NeedsHuman` after press + 2s puzzle check
  (`recaptcha_wants_a-person` equivalent path, `MAX_PRESSES=3`). **Do not**
  hand-roll coordinates — any teleport fails the enterprise score.

### F. RoyalMail Next-button flow

- Resource: `https://help.royalmail.com/resource/1736330734000/RMG_HCaptcha_Next`
  (compact, sitekey `0a3e79fc-62f5-4bd8-ba87-710a4b81ac4b` at probe time).
- Steps:
  1. `nokk --load https://help.royalmail.com/ --eval "document.body.innerHTML.slice(0,4000)"` — find iframe src (version may differ).
  2. `nokk --load <iframe-url> --solve-challenge 30 [--proxy … --geoip-timezone]` — press checkbox, then `--click "button[type=submit]"` (the Next button).
  3. Assert parent receives `"captcha success"` (validateForm passes,
     `h-captcha-response` non-empty).
- Expect: `TokenIssued` then Next navigates. Static-fetch sitekey is stale
  signal only — live DOM is ground truth.

### G. Proxy requirement (residential + `--geoip-timezone`)

- Run case A twice: (1) direct datacenter IP, default `en-US`;
  (2) `--proxy http://user:pass@residential:<port> --geoip-timezone`.
- Expect: (1) higher puzzle rate / lower passive score; (2) timezone from
  exit IP, `navigator.languages` + wire `Accept-Language` derived atomically
  from same geo lookup (pooled per `key\0lang:` / `session:name\0lang:`),
  lookup via throwaway proxy client (cookies never pollute site jar).
- Without `--proxy`, `--geoip-timezone` is a no-op (by design).
- Blocklist note: `crates/net/src/blocklist.rs` allowlists `hcaptcha.com`
  (covers `js.hcaptcha.com`, `frames.hcaptcha.com`) — do not enable strict
  tracker blocking against these hosts during tests.

## 4. Must-NOT-do

- No loops over sitekeys, no parallel solves, no `vision-onnx` brute-force
  at low threshold. Fail-closed (`decide_indices` empty on low confidence /
  NaN / out-of-range threshold) is correct behavior.
- No `siteverify` secret in repo. Verify server-side only with throwaway
  test secret, never log tokens.

## 5. Success criteria

1. `solve_challenge` returns `TokenIssued` (or `Cleared` for interstitial),
   `presses <= 3`, `h-captcha-response` / `__pt_widgetToken()` non-empty.
2. Server-side `POST https://api.hcaptcha.com/siteverify` with
   `secret + response + sitekey` → `{ "success": true }` (run once per case,
   outside the repo).
3. Both fail correctly: unknown label → threshold 0.80 + empty indices;
   `area_select`/drag → `NeedsHuman`; bot test key → `Timeout`.

## 6. Predicted top-3 failure modes (no solves run)

1. **Blocklist / tracker stripping breaks the widget** — over-aggressive
   blocking of `hcaptcha.com` frames or `api.js` leaves `challenge_state`
   stuck at `hcaptcha && !token` → `Timeout`. Mitigation: keep
   `ALLOWLIST hcaptcha.com` (already in `blocklist.rs`), use
   `--dump-requests` to confirm `api.js` + `/captcha/*` loaded.
2. **Passive enterprise score fails on datacenter IP / locale mismatch** —
   exit IP ASN = hosting provider + `en-US` + wrong timezone reads as bot
   even with a perfect pointer trail (`press_seed` arc is necessary but not
   sufficient). Mitigation: residential proxy in target market +
   `--geoip-timezone` (atomic JS + wire locale); expect puzzle or outright
   score-fail without it.
3. **Drag / `area_select` unsupported → `NeedsHuman` by design** —
   `dispatch_drag` exists in core but the captcha layer only handles
   `image_label_binary` 3×3 + checkbox/invisible; crop/slider puzzles have no
   vision backend in default build. Attempting coordinate guesses would also
   burn the sitekey's rate limit. Correct outcome is `NeedsHuman`, not a
   forced click.

## 7. Quick re-probe commands (safe, no solve)

```powershell
(Invoke-WebRequest https://js.hcaptcha.com/1/api.js -UseBasicParsing).StatusCode
(Invoke-WebRequest https://nopecha.com/js/demo/hcaptcha.js?v=2 -UseBasicParsing).Content `
  | Select-String -Pattern '[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}' -AllMatches `
  | ForEach-Object { $_.Matches.Value } | Sort-Object -Unique
(Invoke-WebRequest https://help.royalmail.com/resource/1736330734000/RMG_HCaptcha_Next -UseBasicParsing).Content
```
