//! hCaptcha task plumbing without any model weights.
//!
//! Covers prompt normalization, per-label thresholds, fail-closed index
//! selection, the multi-crumb / two-pass solve loop, invisible-mode trigger
//! detection, and an opt-in dump collector. ONNX inference lives behind
//! the `vision-onnx` feature so the default build stays light.
//!
//! All helpers here are offline and deterministic: no network, no clock.
//! Callers pass timestamps / JSON bodies in so unit tests never touch I/O.

pub mod taxonomy;

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use sha2::{Digest, Sha256};
use thiserror::Error;

pub use taxonomy::{canonical_label, normalize_prompt, threshold_for};

/// Challenge family recognized by the engine.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize, Default,
)]
#[serde(rename_all = "snake_case")]
pub enum ChallengeKind {
    /// No challenge detected.
    #[default]
    None,
    /// hCaptcha checkbox / image grid widget.
    HcaptchaWidget,
    /// Reserved for other providers.
    Other,
}

/// Static hCaptcha widget configuration.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, Default)]
pub struct HcaptchaConfig {
    /// hCaptcha sitekey from the widget element.
    pub sitekey: String,
    /// Host the challenge runs on.
    pub host: String,
    /// Interface language code.
    pub hl: String,
    /// Optional request data token (`rqdata` passthrough).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rqdata: Option<String>,
    /// Optional widget or page URL.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
}

impl HcaptchaConfig {
    /// Build a config from its parts.
    pub fn new(sitekey: impl Into<String>, host: impl Into<String>, hl: impl Into<String>) -> Self {
        Self {
            sitekey: sitekey.into(),
            host: host.into(),
            hl: hl.into(),
            rqdata: None,
            url: None,
        }
    }

    /// Attach an `rqdata` token (passthrough: sent verbatim on getcaptcha /
    /// checkcaptcha; never logged, never altered).
    pub fn with_rqdata(mut self, rqdata: impl Into<String>) -> Self {
        let v = rqdata.into();
        self.rqdata = if v.is_empty() { None } else { Some(v) };
        self
    }

    /// Attach the widget/page URL (sent as `host` context where required).
    pub fn with_url(mut self, url: impl Into<String>) -> Self {
        let v = url.into();
        self.url = if v.is_empty() { None } else { Some(v) };
        self
    }

    /// Force the interface language to English (`en`) for stable prompt
    /// normalization. hCaptcha localizes prompts per `hl`; the taxonomy is
    /// trained on English, so non-English prompts mis-normalize.
    pub fn force_english(mut self) -> Self {
        self.hl = "en".to_string();
        self
    }

    /// Effective `hl` to send: English unless already an English variant.
    /// Empty or non-English values collapse to `"en"` (fail-stable).
    pub fn effective_hl(&self) -> &str {
        let h = self.hl.trim();
        if h.is_empty() {
            return "en";
        }
        let mut low = h.to_lowercase();
        low = low.replace('_', "-");
        if low == "en" || low.starts_with("en-") {
            return &self.hl;
        }
        "en"
    }

    /// Ordered `(key, value)` pairs for a getcaptcha-style request body.
    /// Includes `rqdata` only when present (passthrough) and always uses
    /// [`Self::effective_hl`] so prompts come back in English.
    pub fn query_pairs(&self) -> Vec<(String, String)> {
        let mut out = vec![
            ("sitekey".to_string(), self.sitekey.clone()),
            ("host".to_string(), self.host.clone()),
            ("hl".to_string(), self.effective_hl().to_string()),
        ];
        if let Some(r) = &self.rqdata {
            out.push(("rqdata".to_string(), r.clone()));
        }
        if let Some(u) = &self.url {
            out.push(("url".to_string(), u.clone()));
        }
        out
    }

    /// Rewrite a frame/widget URL's `hl` query param to the effective value.
    /// Pure string surgery (no URL crate): appends `hl=en` when absent,
    /// replaces the existing value otherwise so the challenge renders English.
    pub fn frame_url_with_hl(&self, base: &str) -> String {
        let want = self.effective_hl();
        if let Some(pos) = find_hl_param(base) {
            let (head, tail) = base.split_at(pos);
            let rest = &tail["hl=".len()..];
            let end = rest
                .find('&')
                .map(|i| i + "hl=".len())
                .unwrap_or(tail.len());
            let after = &tail[end..];
            format!("{head}hl={want}{after}")
        } else if base.contains('?') {
            format!("{base}&hl={want}")
        } else {
            format!("{base}?hl={want}")
        }
    }
}

/// Byte offset of the `hl=` query param value start's key in `url`, if any.
/// Matches `?hl=` or `&hl=` only (not `xhl=`).
fn find_hl_param(url: &str) -> Option<usize> {
    let b = url.as_bytes();
    let mut i = 0;
    while i + 4 <= b.len() {
        if b[i] == b'h'
            && b[i + 1] == b'l'
            && b[i + 2] == b'='
            && (i == 0 || b[i - 1] == b'?' || b[i - 1] == b'&')
        {
            // Require a query context: there must be a '?' at or before i.
            if url[..i].contains('?') || (i > 0 && (b[i - 1] == b'?' || b[i - 1] == b'&')) {
                return Some(i);
            }
        }
        i += 1;
    }
    None
}

/// One tile of the 3x3 hCaptcha grid.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct TileUrl {
    /// Tile position 0..9, row-major.
    pub index: u8,
    /// Tile image URL.
    pub url: String,
}

/// One normalized hCaptcha selection task.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct HcaptchaTask {
    /// Raw prompt text from the widget.
    pub prompt_raw: String,
    /// Normalized canonical label.
    pub prompt_norm: String,
    /// Grid tiles, usually 9 entries.
    pub tiles: Vec<TileUrl>,
    /// Request flavor, e.g. `image_label_binary`.
    #[serde(default)]
    pub request_type: String,
}

impl HcaptchaTask {
    /// Build a task and normalize the prompt in one step.
    pub fn new(
        prompt_raw: impl Into<String>,
        tile_urls: Vec<String>,
        request_type: impl Into<String>,
    ) -> Self {
        let prompt_raw = prompt_raw.into();
        let prompt_norm = normalize_prompt(&prompt_raw);
        let tiles = tile_urls
            .into_iter()
            .enumerate()
            .map(|(i, url)| TileUrl {
                index: i as u8,
                url,
            })
            .collect();
        Self {
            prompt_raw,
            prompt_norm,
            tiles,
            request_type: request_type.into(),
        }
    }

    /// Number of tiles in this task.
    pub fn len(&self) -> usize {
        self.tiles.len()
    }

    /// True when the task carries no tiles.
    pub fn is_empty(&self) -> bool {
        self.tiles.is_empty()
    }

    /// Tile URLs in index order (convenience for change detection).
    pub fn tile_urls(&self) -> Vec<String> {
        let mut v = self.tiles.clone();
        v.sort_by_key(|t| t.index);
        v.into_iter().map(|t| t.url).collect()
    }
}

/// Request flavors the default solver handles. Only the 3x3 image-label grid
/// (`image_label_binary`) has a (opt-in ONNX) vision backend; `area_select`,
/// slider/crop/drag variants have none in the default build and must resolve
/// to `NeedsHuman` upstream — never a coordinate guess (a teleport fails the
/// enterprise score and burns the sitekey's rate limit).
pub const SUPPORTED_REQUEST_TYPES: &[&str] = &["image_label_binary"];

/// True when `request_type` names a flavor the solver can attempt.
/// Empty (pre-task / checkbox-only) counts as supported: there is no puzzle
/// to mis-solve yet.
pub fn is_supported_request_type(request_type: &str) -> bool {
    request_type.is_empty() || SUPPORTED_REQUEST_TYPES.contains(&request_type)
}

/// Errors from captcha plumbing (parsing, IO).
#[derive(Debug, Error)]
pub enum CaptchaError {
    /// Dump directory IO failed.
    #[error("dump failed: {0}")]
    Dump(String),
    /// Task description is invalid.
    #[error("invalid task: {0}")]
    InvalidTask(String),
}

/// SHA-256 hex digest of arbitrary bytes.
pub fn sha256_hex(bytes: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(bytes);
    hex::encode(h.finalize())
}

/// Select tile indices whose score clears `threshold`.
///
/// Fail-closed: low confidence returns an empty vec, never a guess.
/// An all-false row also returns empty here; any fallback click lives
/// in a higher layer, not in this function.
pub fn decide_indices(scores: &[f32], threshold: f32) -> Vec<u8> {
    if !threshold.is_finite() || threshold < 0.0 || threshold > 1.0 {
        return Vec::new();
    }
    scores
        .iter()
        .enumerate()
        .filter_map(|(i, s)| {
            if s.is_finite() && *s >= threshold && i <= u8::MAX as usize {
                Some(i as u8)
            } else {
                None
            }
        })
        .collect()
}

/// Same as [`decide_indices`] with the per-label table threshold.
pub fn decide_indices_for_label(scores: &[f32], label: &str) -> Vec<u8> {
    decide_indices(scores, threshold_for(label))
}

impl HcaptchaTask {
    /// True when this task's flavor has a solver in the default build.
    /// `area_select` / slider / crop / drag flavors return false: the caller
    /// must surface `NeedsHuman` instead of guessing coordinates.
    pub fn is_supported(&self) -> bool {
        is_supported_request_type(&self.request_type)
    }

    /// Fail-closed selection for a full task: unsupported flavors return
    /// empty unconditionally (no scores are even consulted), otherwise the
    /// per-label threshold applies via [`decide_indices_for_label`].
    pub fn decide_for_task(&self, scores: &[f32]) -> Vec<u8> {
        if !self.is_supported() {
            return Vec::new();
        }
        decide_indices_for_label(scores, &self.prompt_norm)
    }
}

// ---------------------------------------------------------------------------
// Solve loop: crumbs, two-pass Verify re-read, stale cleanup, invisible mode.
// ---------------------------------------------------------------------------

/// Max getcaptcha/checkcaptcha crumbs per challenge: hCaptcha usually passes
/// on crumb 1–2 and rarely needs a third; more is a loop, not a user.
pub const MAX_CRUMBS: u32 = 3;

/// Max Verify passes per challenge: after the first Verify hCaptcha swaps
/// out solved tiles for fresh ones, so the solver must re-read and solve
/// once more. A third pass means the loop is stuck.
pub const MAX_VERIFY_PASSES: u32 = 2;

/// A solved token older than this is stale and must not be reused: the
/// widget rotated (or the page navigated) underneath the solve.
pub const STALE_SOLVE_MS: u64 = 120_000;

/// Parse a checkcaptcha-style body for the `pass` flag.
///
/// Accepts `{"pass":true}`, `{"pass":false}`, nested
/// `{"result":{"pass":true}}`, and bare `pass:true` fragments. Returns
/// `None` when no flag is present so callers can fail closed.
pub fn parse_pass_flag(body: &str) -> Option<bool> {
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(body) {
        if let Some(b) = v.get("pass").and_then(|x| x.as_bool()) {
            return Some(b);
        }
        if let Some(b) = v
            .get("result")
            .and_then(|r| r.get("pass"))
            .and_then(|x| x.as_bool())
        {
            return Some(b);
        }
        // Some backends nest one deeper under `data`.
        if let Some(b) = v
            .get("data")
            .and_then(|d| d.get("pass"))
            .and_then(|x| x.as_bool())
        {
            return Some(b);
        }
    }
    // Fallback for fragmented/log-line bodies.
    let low = body.to_lowercase();
    if low.contains("\"pass\":true") || low.contains("\"pass\": true") || low.contains("pass:true")
    {
        return Some(true);
    }
    if low.contains("\"pass\":false")
        || low.contains("\"pass\": false")
        || low.contains("pass:false")
    {
        return Some(false);
    }
    None
}

/// Offline crumb-loop state: attempt up to [`MAX_CRUMBS`] crumbs until the
/// backend answers `pass:true`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CrumbLoop {
    attempts: u32,
    max: u32,
}

impl Default for CrumbLoop {
    fn default() -> Self {
        Self::new()
    }
}

impl CrumbLoop {
    /// New loop with the default budget ([`MAX_CRUMBS`]).
    pub fn new() -> Self {
        Self {
            attempts: 0,
            max: MAX_CRUMBS,
        }
    }

    /// New loop with an explicit budget (clamped to >= 1).
    pub fn with_max(max: u32) -> Self {
        Self {
            attempts: 0,
            max: max.max(1),
        }
    }

    /// Crumbs consumed so far.
    pub fn attempts(&self) -> u32 {
        self.attempts
    }

    /// Remaining crumbs including the current one.
    pub fn remaining(&self) -> u32 {
        self.max.saturating_sub(self.attempts)
    }

    /// True when another crumb may be issued (budget left and no pass yet).
    pub fn should_continue(&self, passed: bool) -> bool {
        !passed && self.attempts < self.max
    }

    /// Record one finished crumb. Returns whether to issue another
    /// (`should_continue(passed)` after incrementing).
    pub fn record(&mut self, passed: bool) -> bool {
        self.attempts = self.attempts.saturating_add(1);
        self.should_continue(passed)
    }
}

/// True when two tile-URL snapshots differ (order-sensitive): hCaptcha swaps
/// solved tiles for fresh ones after each Verify, so equality means the
/// re-read raced the swap and must be retried.
pub fn tiles_changed(before: &[String], after: &[String]) -> bool {
    before != after
}

/// True when the solver must re-read the grid after a Verify click:
/// always on pass 1 (the swap), and whenever the URLs moved on later passes.
/// Pass numbering is 1-based; passes beyond [`MAX_VERIFY_PASSES`] never
/// re-read — the loop must stop instead.
pub fn needs_reread_after_verify(pass: u32, urls_changed: bool) -> bool {
    if pass == 0 || pass > MAX_VERIFY_PASSES {
        return false;
    }
    if pass == 1 {
        return true;
    }
    urls_changed
}

/// Freshness guard for a finished solve: drops tokens older than
/// [`STALE_SOLVE_MS`] instead of submitting them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SolveFreshness {
    solved_at_ms: u64,
    max_age_ms: u64,
}

impl SolveFreshness {
    /// Mark a solve finished at `solved_at_ms` (epoch ms, caller clock).
    pub fn new(solved_at_ms: u64) -> Self {
        Self {
            solved_at_ms,
            max_age_ms: STALE_SOLVE_MS,
        }
    }

    /// True when `now_ms` is past the staleness horizon (or clock-skewed
    /// backwards — fail closed and treat as stale).
    pub fn is_stale(&self, now_ms: u64) -> bool {
        if now_ms < self.solved_at_ms {
            return true;
        }
        now_ms.saturating_sub(self.solved_at_ms) > self.max_age_ms
    }
}

/// JS that clears stale solve state after a finished/failed attempt:
/// drops any latched press target so the next press re-resolves its element.
pub fn stale_solve_cleanup_js() -> String {
    "typeof __pt_setPressTarget==='function'&&__pt_setPressTarget(null)".to_string()
}

// --- invisible mode ---------------------------------------------------------

/// True when an hCaptcha anchor/frame `src` requests invisible mode
/// (`size=invisible` / `data-size=invisible`).
pub fn is_invisible_anchor_src(src: &str) -> bool {
    let low = src.to_lowercase().replace("%3d", "=").replace("%3D", "=");
    low.contains("size=invisible") || low.contains("data-size=invisible")
}

/// True when page HTML/JS shows an invisible hCaptcha integration:
/// an invisible anchor src or an explicit `hcaptcha.execute(` call.
pub fn page_requests_invisible(html_or_js: &str) -> bool {
    if is_invisible_anchor_src(html_or_js) {
        return true;
    }
    html_or_js.contains("hcaptcha.execute")
        || html_or_js.contains("hcaptcha?.execute")
        || html_or_js.contains("window.hcaptcha") && html_or_js.contains(".execute(")
}

/// JS snippet that triggers an invisible challenge for `sitekey`.
/// The caller evaluates it in the page; a `false` return means no
/// `hcaptcha` bridge was present to trigger.
pub fn execute_trigger_js(sitekey: &str) -> String {
    // JSON-encode the sitekey so quotes cannot break out of the string.
    let key = serde_json::to_string(sitekey).unwrap_or_else(|_| "\"\"".into());
    format!(
        "(() => {{ try {{ const h = window.hcaptcha; if (!h || typeof h.execute !== 'function') return false; \
         const id = (() => {{ try {{ for (const el of document.querySelectorAll('.h-captcha')) {{ \
         const k = el.getAttribute('data-sitekey'); if (!k || k === {key}) return el; }} }} catch (e) {{}} return null; }})(); \
         if (id) {{ h.execute(id); }} else {{ h.execute(); }} return true; }} catch (e) {{ return false; }} }})()"
    )
}

/// One dump record written by [`maybe_dump`].
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct DumpRecord {
    /// Normalized prompt label.
    pub prompt: String,
    /// SHA-256 over prompt + tile URLs.
    pub sha256: String,
    /// Predicted tile indices.
    pub pred: Vec<u8>,
    /// Solver time in milliseconds.
    pub elapsed_ms: u64,
}

impl DumpRecord {
    /// Build a record from task parts.
    pub fn new(prompt: &str, tile_urls: &[String], pred: Vec<u8>, elapsed: Duration) -> Self {
        let mut key = prompt.as_bytes().to_vec();
        for u in tile_urls {
            key.extend_from_slice(&[0]);
            key.extend_from_slice(u.as_bytes());
        }
        Self {
            prompt: prompt.to_string(),
            sha256: sha256_hex(&key),
            pred,
            elapsed_ms: elapsed.as_millis() as u64,
        }
    }
}

/// Dump path for a record digest, if collection is enabled.
pub fn dump_path_for(dir: &Path, sha256: &str) -> PathBuf {
    dir.join(format!("{sha256}.json"))
}

/// Write a dump record when `NOKK_HCAPTCHA_DUMP_DIR` is set.
///
/// Dedupes by digest: an existing `<sha256>.json` file is left alone.
/// Returns `Ok(None)` when the env var is unset, `Ok(Some(path))` after
/// a write or a dedup hit.
pub fn maybe_dump(record: &DumpRecord) -> Result<Option<PathBuf>, CaptchaError> {
    let dir = match std::env::var("NOKK_HCAPTCHA_DUMP_DIR") {
        Ok(v) if !v.trim().is_empty() => PathBuf::from(v),
        _ => return Ok(None),
    };
    fs::create_dir_all(&dir).map_err(|e| CaptchaError::Dump(e.to_string()))?;
    let path = dump_path_for(&dir, &record.sha256);
    if path.exists() {
        return Ok(Some(path));
    }
    let body = serde_json::to_string(record).map_err(|e| CaptchaError::Dump(e.to_string()))?;
    fs::write(&path, body).map_err(|e| CaptchaError::Dump(e.to_string()))?;
    Ok(Some(path))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_maps_lorry_to_truck() {
        assert_eq!(normalize_prompt("Please select all lorries"), "truck");
    }

    #[test]
    fn decide_is_fail_closed() {
        assert!(decide_indices(&[0.1, 0.2], 0.6).is_empty());
        assert!(decide_indices(&[f32::NAN, 0.9], 0.6) == vec![1]);
        assert!(decide_indices(&[0.9], f32::NAN).is_empty());
    }

    #[test]
    fn task_build_normalizes_prompt() {
        let t = HcaptchaTask::new(
            "Click each image containing a bus",
            vec!["https://x/0".into()],
            "image_label_binary",
        );
        assert_eq!(t.prompt_norm, "bus");
        assert_eq!(t.tiles.len(), 1);
    }

    #[test]
    fn hl_forcing_is_english() {
        let c = HcaptchaConfig::new("k", "example.com", "de").force_english();
        assert_eq!(c.effective_hl(), "en");
        let c2 = HcaptchaConfig::new("k", "example.com", "");
        assert_eq!(c2.effective_hl(), "en");
        let c3 = HcaptchaConfig::new("k", "example.com", "en-US");
        assert_eq!(c3.effective_hl(), "en-US");
    }

    #[test]
    fn frame_url_forces_hl() {
        let c = HcaptchaConfig::new("k", "example.com", "fr");
        assert_eq!(
            c.frame_url_with_hl("https://hcaptcha.com/captcha/v1/frame?sitekey=k&hl=fr"),
            "https://hcaptcha.com/captcha/v1/frame?sitekey=k&hl=en"
        );
        assert_eq!(
            c.frame_url_with_hl("https://hcaptcha.com/captcha/v1/frame?sitekey=k"),
            "https://hcaptcha.com/captcha/v1/frame?sitekey=k&hl=en"
        );
    }

    #[test]
    fn rqdata_passes_through() {
        let c = HcaptchaConfig::new("k", "h", "en").with_rqdata("tok123");
        let pairs = c.query_pairs();
        assert!(pairs.iter().any(|(k, v)| k == "rqdata" && v == "tok123"));
        let c2 = HcaptchaConfig::new("k", "h", "en");
        assert!(c2.query_pairs().iter().all(|(k, _)| k != "rqdata"));
    }

    #[test]
    fn crumb_loop_runs_until_pass_or_budget() {
        let mut l = CrumbLoop::new();
        assert!(l.should_continue(false));
        assert!(l.record(false)); // crumb 1, no pass -> continue
        assert!(l.record(false)); // crumb 2 -> continue (1 left)
        assert!(!l.record(false)); // crumb 3 -> budget spent
        assert_eq!(l.attempts(), MAX_CRUMBS);
        let mut ok = CrumbLoop::new();
        assert!(!ok.record(true)); // pass on crumb 1 stops
    }

    #[test]
    fn pass_flag_parses() {
        assert_eq!(parse_pass_flag(r#"{"pass":true}"#), Some(true));
        assert_eq!(parse_pass_flag(r#"{"pass":false}"#), Some(false));
        assert_eq!(parse_pass_flag(r#"{"result":{"pass":true}}"#), Some(true));
        assert_eq!(parse_pass_flag(r#"{"c":"75a5"}"#), None);
    }

    #[test]
    fn two_pass_reread_logic() {
        // Pass 1 always re-reads: the widget swaps solved tiles.
        assert!(needs_reread_after_verify(1, false));
        assert!(needs_reread_after_verify(1, true));
        // Pass 2 re-reads only when URLs moved.
        assert!(needs_reread_after_verify(2, true));
        assert!(!needs_reread_after_verify(2, false));
        assert!(!needs_reread_after_verify(3, true));
        let a = vec!["u1".to_string(), "u2".to_string()];
        let b = vec!["u1".to_string(), "u3".to_string()];
        assert!(tiles_changed(&a, &b));
        assert!(!tiles_changed(&a, &a));
    }

    #[test]
    fn stale_guard_fails_closed() {
        let g = SolveFreshness::new(1_000);
        assert!(!g.is_stale(1_001));
        assert!(g.is_stale(1_000 + STALE_SOLVE_MS + 1));
        assert!(g.is_stale(999)); // clock went backwards
    }

    #[test]
    fn invisible_detection() {
        assert!(is_invisible_anchor_src(
            "https://hcaptcha.com/captcha/v1/anchor?sitekey=k&size=invisible"
        ));
        assert!(!is_invisible_anchor_src(
            "https://hcaptcha.com/captcha/v1/anchor?sitekey=k"
        ));
        assert!(page_requests_invisible("hcaptcha.execute({sitekey})"));
        assert!(!page_requests_invisible("<div class=h-captcha></div>"));
        let js = execute_trigger_js("site-k");
        assert!(js.contains("hcaptcha") && js.contains("execute"));
    }

    #[test]
    fn area_select_is_unsupported_fail_closed() {
        // Live-test case E: drag/slider/crop flavors have no vision backend in
        // the default build. They must read as unsupported so the driver
        // returns NeedsHuman instead of guessing coordinates.
        assert!(is_supported_request_type("image_label_binary"));
        assert!(is_supported_request_type("")); // pre-task, nothing to solve
        for flavor in [
            "area_select",
            "slider",
            "crop",
            "drag",
            "image_label_area_select",
        ] {
            assert!(
                !is_supported_request_type(flavor),
                "{flavor} must be unsupported"
            );
        }
        let binary = HcaptchaTask::new("car", vec!["https://x/0".into()], "image_label_binary");
        assert!(binary.is_supported());
        let drag = HcaptchaTask::new("drag the slider", vec!["https://x/0".into()], "area_select");
        assert!(!drag.is_supported());
        // Even perfect scores on an unsupported flavor select nothing.
        assert!(drag.decide_for_task(&[0.99; 9]).is_empty());
        // Supported flavor still thresholds normally.
        assert!(binary.decide_for_task(&[0.1; 9]).is_empty());
        assert_eq!(binary.decide_for_task(&[0.99; 9]).len(), 9);
    }
}
