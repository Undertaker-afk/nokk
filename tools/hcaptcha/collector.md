# hCaptcha Tile Collector (opt-in only)

Collects anonymized tiles to train `train.py`. Disabled by default.
No PII: prompts + tile pixels + model scores only. No URLs, IPs, tokens.

## Enable

```bash
set NOKK_HCAPTCHA_DUMP_DIR=C:\temp\hcaptcha-dump  # opt-in; unset = no dump
```

## Dump layout

- `$DUMP/<prompt_norm>/<tile_sha256>.png` — 128px tile, dedup by SHA-256.
- `$DUMP/manifest.jsonl` — one JSON row per tile, see `dump.schema.json`.

## Rules

1. Skip if `NOKK_HCAPTCHA_DUMP_DIR` is unset or empty.
2. Dedup by `tile_sha256`; never overwrite an existing PNG.
3. `prompt_norm`: lowercase, trim, map alias via `taxonomy.yaml`.
4. Never log page URL, user agent, IP, sitekey, or raw HTML.
5. Best-effort I/O: collection must never break solving.

## Fields

`prompt_raw`, `prompt_norm`, `tile_sha256`, `tile_png`,
`pred`, `threshold`, `cleared`, `elapsed_ms`.

## Example row

```json
{"prompt_raw":"Select all buses","prompt_norm":"bus","tile_sha256":"…","tile_png":"bus/….png","pred":"bus","threshold":0.6,"cleared":true,"elapsed_ms":42}
```
