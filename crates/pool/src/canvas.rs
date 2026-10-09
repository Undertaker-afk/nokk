//! Optional real 2D canvas rasterization — the `render` feature.
//!
//! With `--features render`, the JS canvas backs its pixel operations with a real
//! [`tiny_skia`] rasterizer instead of the default JS synthesis, so `getImageData`
//! and `toDataURL` return genuine pixels of what was drawn (see docs/rendering.md).
//!
//! One [`Pixmap`] per canvas id, kept **thread-local** — nokk runs one V8 isolate
//! per worker thread, so a per-thread store needs no locking and can't leak across
//! contexts on other threads. The JS layer calls the `__pt_canvas*` natives that
//! wrap these. Covered here: fills, real glyph text (`fill_text`/`measure_text`
//! via a bundled font), vector paths (`fill_path`/`stroke_path` — the JS side
//! tessellates curves/arcs to a move/line/close verb stream), linear/radial
//! gradients (`fill_path_grad`), and image data put/get. Only `drawImage` still
//! falls back to the JS deterministic stamp; WebGL is a separate phase.

use std::cell::RefCell;
use std::collections::HashMap;

use ab_glyph::{Font, FontVec, PxScale};
use tiny_skia::{
    Color, FillRule, GradientStop, LinearGradient, Paint, PathBuilder, Pixmap, Point,
    RadialGradient, Rect, Shader, SpreadMode, Stroke, Transform,
};

/// Bundled Liberation Sans (OFL, Arial-metric): last-resort fallback when the
/// system has no fonts at all (e.g. a bare container).
const FONT_BYTES: &[u8] = include_bytes!("../fonts/LiberationSans-Regular.ttf");

/// Font directories, in fontconfig order.
const FONT_DIRS: &[&str] = &[
    "/usr/share/fonts",
    "/usr/local/share/fonts",
    "/usr/X11R6/lib/X11/fonts",
];

/// Which file Chrome 151 picks for which family name on this machine.
/// fontconfig substitutes metric-compatible fonts, so `16px Arial` and
/// `16px "Liberation Sans"` measure the same. An unlisted family is skipped;
/// if none matches, Chrome falls back to Liberation Serif.
const FAMILIES: &[(&str, &[&str])] = &[
    (
        "sans-serif",
        &[
            "LiberationSans-Regular.ttf",
            "Arimo-Regular.ttf",
            "DejaVuSans.ttf",
        ],
    ),
    (
        "arial",
        &["LiberationSans-Regular.ttf", "Arimo-Regular.ttf"],
    ),
    (
        "helvetica",
        &["LiberationSans-Regular.ttf", "Arimo-Regular.ttf"],
    ),
    ("liberation sans", &["LiberationSans-Regular.ttf"]),
    // Chrome resolves generic `serif` to Liberation Serif here, not DejaVu:
    // "mmmmmmmmmmlli" at 72px is 620.05 vs DejaVu's 751.82, which any
    // text-measuring page sees.
    (
        "serif",
        &[
            "LiberationSerif-Regular.ttf",
            "Tinos-Regular.ttf",
            "DejaVuSerif.ttf",
        ],
    ),
    (
        "times new roman",
        &["LiberationSerif-Regular.ttf", "Tinos-Regular.ttf"],
    ),
    (
        "times",
        &["LiberationSerif-Regular.ttf", "Tinos-Regular.ttf"],
    ),
    ("liberation serif", &["LiberationSerif-Regular.ttf"]),
    (
        "monospace",
        &[
            "NotoSansMono-Regular.ttf",
            "LiberationMono-Regular.ttf",
            "DejaVuSansMono.ttf",
        ],
    ),
    (
        "courier new",
        &["LiberationMono-Regular.ttf", "Cousine-Regular.ttf"],
    ),
    (
        "courier",
        &["LiberationMono-Regular.ttf", "Cousine-Regular.ttf"],
    ),
    ("liberation mono", &["LiberationMono-Regular.ttf"]),
    ("dejavu sans", &["DejaVuSans.ttf"]),
    ("dejavu serif", &["DejaVuSerif.ttf"]),
    ("dejavu sans mono", &["DejaVuSansMono.ttf"]),
    ("noto sans mono", &["NotoSansMono-Regular.ttf"]),
    // Metric-compatible names: no such files, but fontconfig substitutes
    // Liberation, so the browser reports the family as present.
    ("arimo", &["LiberationSans-Regular.ttf"]),
    ("tinos", &["LiberationSerif-Regular.ttf"]),
    ("cousine", &["LiberationMono-Regular.ttf"]),
    // `system-ui` is the desktop font; Chrome gets Cantarell here.
    (
        "system-ui",
        &[
            "Cantarell-Regular.otf",
            "NotoSans-Regular.ttf",
            "DejaVuSans.ttf",
        ],
    ),
    ("cantarell", &["Cantarell-Regular.otf"]),
];

/// Whether a font with this name exists, as `local()` in `@font-face` looks it
/// up. Pages enumerate installed fonts this way: `new FontFace(…, 'local("X")')`
/// resolves for a present name and rejects otherwise. fontconfig substitution
/// does not apply: `Arial` is not found here, `Liberation Sans` is.
pub fn has_local_font(name: &str) -> bool {
    let key = name.trim().to_lowercase();
    if key.is_empty() {
        return false;
    }
    local_index().contains(&key)
}

/// Names `local()` matches: full face name (name ID 4) and PostScript name
/// (ID 6), not the family. In Chrome `local("Cantarell")` fails while
/// `local("Cantarell Regular")` and `local("Cantarell-Regular")` succeed.
fn local_index() -> &'static std::collections::HashSet<String> {
    static INDEX: std::sync::OnceLock<std::collections::HashSet<String>> =
        std::sync::OnceLock::new();
    INDEX.get_or_init(|| {
        let mut out = std::collections::HashSet::new();
        for dir in FONT_DIRS {
            let mut stack = vec![std::path::PathBuf::from(dir)];
            while let Some(d) = stack.pop() {
                let Ok(entries) = std::fs::read_dir(&d) else {
                    continue;
                };
                for e in entries.flatten() {
                    let path = e.path();
                    if path.is_dir() {
                        stack.push(path);
                        continue;
                    }
                    let ext = path
                        .extension()
                        .and_then(|x| x.to_str())
                        .unwrap_or_default()
                        .to_ascii_lowercase();
                    if ext != "ttf" && ext != "otf" && ext != "ttc" {
                        continue;
                    }
                    let Ok(bytes) = std::fs::read(&path) else {
                        continue;
                    };
                    let Ok(face) = ttf_parser::Face::parse(&bytes, 0) else {
                        continue;
                    };
                    for name in face.names() {
                        if name.name_id != 4 && name.name_id != 6 {
                            continue;
                        }
                        if let Some(text) = name.to_string() {
                            out.insert(text.trim().to_lowercase());
                        }
                    }
                }
            }
        }
        out
    })
}

/// Browser default font, used when no listed family is found.
const FALLBACK_FAMILY: &str = "times new roman";

thread_local! {
    static CANVASES: RefCell<HashMap<u32, Pixmap>> = RefCell::new(HashMap::new());
    /// Parsed font files by file name. Parsing is costly and fingerprinting pages
    /// probe hundreds of families.
    static LOADED: RefCell<HashMap<String, Option<&'static FontVec>>> =
        RefCell::new(HashMap::new());
    /// Fallback font per char not covered by the named families.
    static FALLBACK: RefCell<HashMap<char, Option<&'static FontVec>>> =
        RefCell::new(HashMap::new());
    /// Shapers by font address.
    static SHAPERS: RefCell<HashMap<usize, Option<&'static rustybuzz::Face<'static>>>> =
        RefCell::new(HashMap::new());
    /// Decoded images, by address. A page draws the same picture many times —
    /// the challenge's beacon PNG lands on a canvas on every round — so the
    /// decode happens once and the pixels stay.
    static IMAGES: RefCell<HashMap<String, (u32, u32, Vec<u8>)>> = RefCell::new(HashMap::new());
}

/// Family -> file index built from the fonts' own `name` tables. A fixed
/// table missed most installed families, so measurement-based font
/// enumeration found far fewer fonts than in Chrome.
fn font_index() -> &'static std::collections::HashMap<String, (std::path::PathBuf, bool)> {
    static INDEX: std::sync::OnceLock<
        std::collections::HashMap<String, (std::path::PathBuf, bool)>,
    > = std::sync::OnceLock::new();
    INDEX.get_or_init(|| {
        let mut out = std::collections::HashMap::new();
        for dir in FONT_DIRS {
            let mut stack = vec![std::path::PathBuf::from(dir)];
            while let Some(d) = stack.pop() {
                let Ok(entries) = std::fs::read_dir(&d) else {
                    continue;
                };
                for e in entries.flatten() {
                    let path = e.path();
                    if path.is_dir() {
                        stack.push(path);
                        continue;
                    }
                    let ext = path
                        .extension()
                        .and_then(|x| x.to_str())
                        .unwrap_or_default()
                        .to_ascii_lowercase();
                    if ext != "ttf" && ext != "otf" && ext != "ttc" {
                        continue;
                    }
                    let Ok(bytes) = std::fs::read(&path) else {
                        continue;
                    };
                    let Ok(face) = ttf_parser::Face::parse(&bytes, 0) else {
                        continue;
                    };
                    // Prefer the regular face, but a family with only an italic face (e.g.
                    // Z003) still exists for the browser.
                    let plain = !face.is_bold() && !face.is_italic();
                    for name in face.names() {
                        // 1 = family, 16 = typographic family.
                        if name.name_id != 1 && name.name_id != 16 {
                            continue;
                        }
                        let Some(text) = name.to_string() else {
                            continue;
                        };
                        let key = text.to_lowercase();
                        match out.entry(key) {
                            std::collections::hash_map::Entry::Vacant(v) => {
                                v.insert((path.clone(), plain));
                            }
                            std::collections::hash_map::Entry::Occupied(mut o) => {
                                if plain && !o.get().1 {
                                    o.insert((path.clone(), true));
                                }
                            }
                        }
                    }
                }
            }
        }
        out
    })
}

/// Find a font file by name in the system font directories.
fn font_path(file: &str) -> Option<std::path::PathBuf> {
    for dir in FONT_DIRS {
        let mut stack = vec![std::path::PathBuf::from(dir)];
        while let Some(d) = stack.pop() {
            let Ok(entries) = std::fs::read_dir(&d) else {
                continue;
            };
            for e in entries.flatten() {
                let path = e.path();
                if path.is_dir() {
                    stack.push(path);
                } else if path.file_name().and_then(|n| n.to_str()) == Some(file) {
                    return Some(path);
                }
            }
        }
    }
    None
}

/// Load a font by file name, once per thread. Leaked on purpose: the set is
/// finite and lives for the whole process.
fn load(file: &str) -> Option<&'static FontVec> {
    LOADED.with(|m| {
        if let Some(hit) = m.borrow().get(file) {
            return *hit;
        }
        let got = font_path(file)
            .and_then(|p| std::fs::read(p).ok())
            .and_then(|bytes| FontVec::try_from_vec(bytes).ok())
            .map(|f| &*Box::leak(Box::new(f)));
        m.borrow_mut().insert(file.to_string(), got);
        got
    })
}

/// Resolve a family list like the browser: first family present in the
/// system, else the default font.
fn face(file: &str, bold: bool, italic: bool) -> Option<&'static FontVec> {
    if !bold && !italic {
        return load(file);
    }
    // Face file naming differs: Liberation uses `-Regular`/`-Bold`/`-Italic`,
    // DejaVu appends `-Bold`/`-Oblique` to the bare name.
    let (stem, ext) = file.rsplit_once('.')?;
    let suffixes: &[&str] = match (bold, italic) {
        (true, true) => &["BoldItalic", "BoldOblique"],
        (true, false) => &["Bold"],
        _ => &["Italic", "Oblique"],
    };
    for suffix in suffixes {
        let name = if let Some(base) = stem.strip_suffix("-Regular") {
            format!("{base}-{suffix}.{ext}")
        } else {
            format!("{stem}-{suffix}.{ext}")
        };
        if let Some(f) = load(&name) {
            return Some(f);
        }
    }
    load(file)
}

/// All families of the list in order, for per-glyph fallback.
///
/// The browser takes each char from the first family that has it, not from the
/// first family found: e.g. "Noto Color Emoji" has no Latin, so Latin text in
/// it measures with the fallback. Otherwise font enumeration by measurement
/// reports fonts as present that cannot be.
fn resolve_chain(families: &str, bold: bool, italic: bool) -> Vec<&'static FontVec> {
    let mut out: Vec<&'static FontVec> = Vec::new();
    let mut push = |f: &'static FontVec| {
        if !out.iter().any(|g| std::ptr::eq(*g, f)) {
            out.push(f);
        }
    };
    for raw in families.split(',') {
        let name = raw.trim().trim_matches(['"', '\'']).to_lowercase();
        if name.is_empty() {
            continue;
        }
        if let Some((_, files)) = FAMILIES.iter().find(|(f, _)| *f == name) {
            for file in *files {
                if let Some(f) = face(file, bold, italic) {
                    push(f);
                    break;
                }
            }
            continue;
        }
        if let Some((path, _)) = font_index().get(&name) {
            if let Some(file) = path.file_name().and_then(|n| n.to_str()) {
                if let Some(f) = face(file, bold, italic) {
                    push(f);
                }
            }
        }
    }
    if let Some(f) = FAMILIES
        .iter()
        .find(|(f, _)| *f == FALLBACK_FAMILY)
        .and_then(|(_, files)| files.iter().find_map(|f| face(f, bold, italic)))
        .or_else(bundled)
    {
        push(f);
    }
    out
}

/// Shaper for this font. Face parsing is costly and pages measure hundreds of
/// strings, so it lives for the whole process, like the font.
fn shaper(font: &'static FontVec) -> Option<&'static rustybuzz::Face<'static>> {
    SHAPERS.with(|m| {
        let key = font as *const FontVec as usize;
        if let Some(hit) = m.borrow().get(&key) {
            return *hit;
        }
        let got = rustybuzz::Face::from_slice(font.as_slice(), 0)
            .map(|f| &*Box::leak(Box::new(f)) as &'static rustybuzz::Face<'static>);
        m.borrow_mut().insert(key, got);
        got
    })
}

/// A shaped glyph: glyph id, font and its origin in pixels.
struct Shaped {
    font: &'static FontVec,
    id: ab_glyph::GlyphId,
    x: f64,
}

/// Convert font units to pixels like FreeType (and so Skia/Chrome): size in
/// 26.6, scale in 16.16, result a fraction over 65536.
///
/// Visible on 1000-upem fonts: `16px "Noto Sans Mono"` gives
/// 9.600021362304688 per glyph in Chrome, not 9.6. 2048-upem fonts divide
/// exactly.
fn ft_px(units: i32, upem: i64, size_px: f32) -> f64 {
    if upem <= 0 {
        return 0.0;
    }
    // FT_DivFix: 26.6 size raised to 16.16, divided by upem, rounded to nearest.
    let size26_6 = (size_px as f64 * 64.0).round() as i64;
    let x_scale = ((size26_6 << 16) + upem / 2) / upem;
    // Signed FT_MulFix: round half away from zero.
    let a = (units as i64) * 1024;
    let prod = a * x_scale;
    let fixed = if prod >= 0 {
        (prod + 0x8000) >> 16
    } else {
        -((-prod + 0x8000) >> 16)
    };
    fixed as f64 / 65536.0
}

/// Shape a run of the string with one font.
fn shape_run(
    out: &mut Vec<Shaped>,
    caret: &mut f64,
    font: &'static FontVec,
    text: &str,
    size_px: f32,
) {
    let upem = font.units_per_em().unwrap_or(1000.0) as i64;
    if let Some(face) = shaper(font) {
        let mut buf = rustybuzz::UnicodeBuffer::new();
        buf.push_str(text);
        buf.guess_segment_properties();
        let laid = rustybuzz::shape(face, &[], buf);
        let (infos, pos) = (laid.glyph_infos(), laid.glyph_positions());
        for (info, p) in infos.iter().zip(pos.iter()) {
            let id = ab_glyph::GlyphId(info.glyph_id as u16);
            out.push(Shaped {
                font,
                id,
                x: *caret + ft_px(p.x_offset, upem, size_px),
            });
            // Color fonts store glyphs as bitmaps; the advance comes from the chosen
            // strike, not hmtx (emoji at 16px: 19.96, not 19.92).
            match raster_advance(face, ttf_parser::GlyphId(info.glyph_id as u16), size_px) {
                Some(w) => *caret += w,
                None => *caret += ft_px(p.x_advance, upem, size_px),
            }
        }
        return;
    }
    for ch in text.chars() {
        let id = font.glyph_id(ch);
        out.push(Shaped {
            font,
            id,
            x: *caret,
        });
        *caret += ft_px(font.h_advance_unscaled(id) as i32, upem, size_px);
    }
}

/// Advance of a bitmap glyph: strike chosen by size, width from its ppem.
/// None for outline fonts.
fn raster_advance(
    face: &rustybuzz::Face<'static>,
    id: ttf_parser::GlyphId,
    size_px: f32,
) -> Option<f64> {
    let img = raster_image(face, id, size_px)?;
    // Keep the width as a fraction over 65536, truncated: emoji at 16px gives
    // 19.963302612304688, not 19.96330275229358.
    let exact = img.width as f64 * size_px as f64 / img.pixels_per_em as f64;
    Some((exact * 65536.0).floor() / 65536.0)
}

/// Bitmap of the glyph from the chosen strike; color fonts have no outlines.
fn raster_image<'a>(
    face: &'a rustybuzz::Face<'static>,
    id: ttf_parser::GlyphId,
    size_px: f32,
) -> Option<ttf_parser::RasterGlyphImage<'a>> {
    if face.glyph_bounding_box(id).is_some() {
        return None;
    }
    let img = face.glyph_raster_image(id, size_px.max(1.0) as u16)?;
    if img.pixels_per_em == 0 || img.width == 0 {
        return None;
    }
    Some(img)
}

/// Shape a string like the browser: word by word, each char from the first
/// font in the chain that has it, with the font's ligatures and kerning.
///
/// Blink shapes per word (word cache), so a pair across a space is not kerned.
/// Liberation Serif kerns "space + W" by 37 units; without this rule
/// "To Wave" came out 0.03px narrower than Chrome.
fn shape(chain: &[&'static FontVec], text: &str, size_px: f32) -> (Vec<Shaped>, f64) {
    let mut out: Vec<Shaped> = Vec::new();
    let mut caret = 0.0f64;
    let mut run = String::new();
    let mut run_font: Option<&'static FontVec> = None;
    for ch in text.chars() {
        // ZWJ and marks do not pick a font themselves: they keep a multi-codepoint
        // emoji in one font so it shapes into a single glyph, as in the browser.
        let cf = if clings_to_previous(ch) {
            run_font.unwrap_or_else(|| face_for(chain, ch))
        } else {
            face_for(chain, ch)
        };
        let breaks = ch == ' ' || run_font.is_some_and(|f| !std::ptr::eq(f, cf));
        if breaks && !run.is_empty() {
            shape_run(
                &mut out,
                &mut caret,
                run_font.unwrap_or(chain[0]),
                &run,
                size_px,
            );
            run.clear();
        }
        run.push(ch);
        run_font = Some(cf);
        if ch == ' ' {
            shape_run(&mut out, &mut caret, cf, &run, size_px);
            run.clear();
            run_font = None;
        }
    }
    if !run.is_empty() {
        shape_run(
            &mut out,
            &mut caret,
            run_font.unwrap_or(chain[0]),
            &run,
            size_px,
        );
    }
    (out, caret)
}

/// Blink-style layout (see `skia::text`): same words and font chain as
/// `shape`, but Skia advances (size to 1/100, truncated to 26.6) and HarfBuzz
/// kerning in 16.16. `fonts` holds font bytes per glyph index.
fn shape_blink(
    chain: &[&'static FontVec],
    text: &str,
    eff: f32,
) -> (Vec<crate::skia::text::ShapedGlyph>, f32, Vec<&'static [u8]>) {
    use crate::skia::text::{em_mult, to_hb_position, ShapedGlyph};
    let mut out: Vec<ShapedGlyph> = Vec::new();
    let mut fonts: Vec<&'static [u8]> = Vec::new();
    let mut font_index = |f: &'static FontVec| -> usize {
        let b = f.as_slice();
        if let Some(i) = fonts
            .iter()
            .position(|x| std::ptr::eq(x.as_ptr(), b.as_ptr()))
        {
            i
        } else {
            fonts.push(b);
            fonts.len() - 1
        }
    };
    // Position in 16.16 (Blink's InlineLayoutUnit).
    let mut total: i64 = 0;
    let mut run_it = |out: &mut Vec<ShapedGlyph>, font: &'static FontVec, run: &str| {
        let fi = font_index(font);
        let Some(face) = shaper(font) else {
            return;
        };
        let Ok(fref) = skrifa::FontRef::new(font.as_slice()) else {
            return;
        };
        use skrifa::MetadataProvider;
        let metrics = fref.glyph_metrics(
            skrifa::prelude::Size::new(eff),
            skrifa::prelude::LocationRef::default(),
        );
        let upem = face.units_per_em() as i64;
        if upem <= 0 {
            return;
        }
        let x_scale = to_hb_position(eff) as i64;
        let x_mult = (if x_scale < 0 {
            -((-x_scale) << 16)
        } else {
            x_scale << 16
        }) / upem;
        let mut buf = rustybuzz::UnicodeBuffer::new();
        buf.push_str(run);
        buf.guess_segment_properties();
        let laid = rustybuzz::shape(face, &[], buf);
        let (infos, pos) = (laid.glyph_infos(), laid.glyph_positions());
        for (info, p) in infos.iter().zip(pos.iter()) {
            let gid = info.glyph_id as u16;
            let tid = ttf_parser::GlyphId(gid);
            // Skia advance; bitmap glyphs use the chosen strike.
            let hb_adv = match raster_advance(face, tid, eff) {
                Some(w) => (w * 65536.0).floor() as i32,
                None => to_hb_position(
                    metrics
                        .advance_width(skrifa::GlyphId::from(gid))
                        .unwrap_or(0.0),
                ),
            };
            let hmtx = face.glyph_hor_advance(tid).unwrap_or(0) as i32;
            let kern = p.x_advance - hmtx;
            let x_advance = hb_adv.wrapping_add(if kern != 0 { em_mult(kern, x_mult) } else { 0 });
            let x_offset = if p.x_offset != 0 {
                em_mult(p.x_offset, x_mult)
            } else {
                0
            };
            out.push(ShapedGlyph {
                gid,
                font: fi,
                x: (total + x_offset as i64) as f32 / 65536.0,
            });
            total += x_advance as i64;
        }
    };
    let mut run = String::new();
    let mut run_font: Option<&'static FontVec> = None;
    for ch in text.chars() {
        let cf = if clings_to_previous(ch) {
            run_font.unwrap_or_else(|| face_for(chain, ch))
        } else {
            face_for(chain, ch)
        };
        let breaks = ch == ' ' || run_font.is_some_and(|f| !std::ptr::eq(f, cf));
        if breaks && !run.is_empty() {
            run_it(&mut out, run_font.unwrap_or(chain[0]), &run);
            run.clear();
        }
        run.push(ch);
        run_font = Some(cf);
        if ch == ' ' {
            run_it(&mut out, cf, &run);
            run.clear();
            run_font = None;
        }
    }
    if !run.is_empty() {
        run_it(&mut out, run_font.unwrap_or(chain[0]), &run);
    }
    (out, total as f32 / 65536.0, fonts)
}

/// Font used to draw this char: the first in the chain that has it.
fn face_for(chain: &[&'static FontVec], ch: char) -> &'static FontVec {
    for f in chain {
        if f.glyph_id(ch).0 != 0 {
            return f;
        }
    }
    // Char missing from every named family: the browser searches all installed
    // fonts (e.g. finds the color emoji font), otherwise the width differs.
    system_face(ch).unwrap_or(chain[0])
}

/// Families used for chars missing from the named ones, in this machine's
/// fontconfig order: color emoji first, then regular fonts.
const FALLBACK_FAMILIES: &[&str] = &[
    "NotoColorEmoji.ttf",
    "DejaVuSans.ttf",
    "NotoSansSymbols2-Regular.ttf",
    "NotoSansMath-Regular.ttf",
    "LiberationSans-Regular.ttf",
    "NotoSansMono-Regular.ttf",
];

/// System font that has this char. Cached: pages ask for the same chars many
/// times.
fn system_face(ch: char) -> Option<&'static FontVec> {
    FALLBACK.with(|m| {
        if let Some(hit) = m.borrow().get(&ch) {
            return *hit;
        }
        let mut found = None;
        for file in FALLBACK_FAMILIES {
            if let Some(f) = load(file) {
                if f.glyph_id(ch).0 != 0 {
                    found = Some(f);
                    break;
                }
            }
        }
        if found.is_none() {
            // Nothing in the list matched: scan all system fonts in a stable order.
            let mut names: Vec<_> = font_index().values().map(|(p, _)| p.clone()).collect();
            names.sort();
            names.dedup();
            for path in names {
                let Some(file) = path.file_name().and_then(|n| n.to_str()) else {
                    continue;
                };
                if let Some(f) = load(file) {
                    if f.glyph_id(ch).0 != 0 {
                        found = Some(f);
                        break;
                    }
                }
            }
        }
        m.borrow_mut().insert(ch, found);
        found
    })
}

/// Chars that stay with their neighbour's font: ZWJ, variation selectors,
/// skin tone modifiers, combining marks.
fn clings_to_previous(ch: char) -> bool {
    matches!(ch as u32,
        0x200D | 0xFE0E | 0xFE0F | 0x1F3FB..=0x1F3FF | 0x20E3
        | 0x0300..=0x036F | 0x1AB0..=0x1AFF | 0x20D0..=0x20F0 | 0xE0020..=0xE007F)
}

fn resolve(families: &str, bold: bool, italic: bool) -> Option<&'static FontVec> {
    for raw in families.split(',') {
        let name = raw.trim().trim_matches(['"', '\'']).to_lowercase();
        if name.is_empty() {
            continue;
        }
        if let Some((_, files)) = FAMILIES.iter().find(|(f, _)| *f == name) {
            for file in *files {
                if let Some(f) = face(file, bold, italic) {
                    return Some(f);
                }
            }
            continue;
        }
        // Not substituted: look the family up as is.
        if let Some((path, _)) = font_index().get(&name) {
            if let Some(file) = path.file_name().and_then(|n| n.to_str()) {
                if let Some(f) = face(file, bold, italic) {
                    return Some(f);
                }
            }
        }
    }
    FAMILIES
        .iter()
        .find(|(f, _)| *f == FALLBACK_FAMILY)
        .and_then(|(_, files)| files.iter().find_map(|f| face(f, bold, italic)))
        .or_else(bundled)
}

/// Built-in font: a system without fonts must still draw something.
fn bundled() -> Option<&'static FontVec> {
    LOADED.with(|m| {
        if let Some(hit) = m.borrow().get("\u{0}bundled") {
            return *hit;
        }
        let got = FontVec::try_from_vec(FONT_BYTES.to_vec())
            .ok()
            .map(|f| &*Box::leak(Box::new(f)));
        m.borrow_mut().insert("\u{0}bundled".to_string(), got);
        got
    })
}

/// Scale at which `ab_glyph` draws a font of size `size_px`. `PxScale` is line
/// height, not em, so convert; otherwise every width is short by
/// height/em (2288/2048 = 1.1172 for Liberation Sans).
fn px_scale<F: Font>(font: &F, size_px: f32) -> PxScale {
    let upem = font.units_per_em().unwrap_or(1000.0);
    PxScale::from(size_px * font.height_unscaled() / upem)
}

/// Line metrics as returned by `measureText`.
#[derive(Debug, Clone, Copy, Default)]
pub struct TextMetrics {
    pub width: f64,
    pub left: f64,
    pub right: f64,
    pub ascent: f64,
    pub descent: f64,
    pub font_ascent: f64,
    pub font_descent: f64,
    /// Line height for `line-height: normal`: ascent + descent + line gap
    /// (1.15 em for Liberation Sans).
    pub line: f64,
}

/// Cap per-side pixels so a hostile page can't request an absurd allocation.
const MAX_DIM: u32 = 8192;

/// Create (or reset) a canvas surface of `w`×`h`.
pub fn create(id: u32, w: u32, h: u32) {
    let (w, h) = (w.clamp(1, MAX_DIM), h.clamp(1, MAX_DIM));
    if let Some(pm) = Pixmap::new(w, h) {
        CANVASES.with(|c| {
            c.borrow_mut().insert(id, pm);
        });
    }
}

/// Remember an image's bytes under its address, decoded to RGBA.
///
/// Without this `drawImage` had nothing to draw: an image reaches JS as lossy
/// text, so its pixels never left Rust. A page that draws a picture and reads
/// the canvas back — which is what a challenge does with the beacon it sends —
/// saw a synthesized pattern where the browser shows the picture.
pub fn remember_image(url: &str, bytes: &[u8]) -> Option<(u32, u32)> {
    let decoded = decode_rgba(bytes)?;
    let (w, h) = (decoded.0, decoded.1);
    IMAGES.with(|m| {
        let mut m = m.borrow_mut();
        // A page can load a great many pictures; keep the map from growing
        // without bound by dropping the oldest arrival when it gets large.
        if m.len() >= 256 {
            if let Some(k) = m.keys().next().cloned() {
                m.remove(&k);
            }
        }
        m.insert(url.to_string(), decoded);
    });
    Some((w, h))
}

/// PNG to RGBA. Everything else is left to the caller's fallback: the formats a
/// challenge uses for a data-bearing picture are lossless, and a lossy one
/// would not carry data anyway.
fn decode_rgba(bytes: &[u8]) -> Option<(u32, u32, Vec<u8>)> {
    let decoder = png::Decoder::new(bytes);
    let mut reader = decoder.read_info().ok()?;
    let mut buf = vec![0; reader.output_buffer_size()];
    let info = reader.next_frame(&mut buf).ok()?;
    let (w, h) = (info.width, info.height);
    if w == 0 || h == 0 || w > MAX_DIM || h > MAX_DIM {
        return None;
    }
    let px = &buf[..info.buffer_size()];
    let rgba = match (info.color_type, info.bit_depth) {
        (png::ColorType::Rgba, png::BitDepth::Eight) => px.to_vec(),
        (png::ColorType::Rgb, png::BitDepth::Eight) => px
            .chunks_exact(3)
            .flat_map(|c| [c[0], c[1], c[2], 255])
            .collect(),
        (png::ColorType::Grayscale, png::BitDepth::Eight) => {
            px.iter().flat_map(|&g| [g, g, g, 255]).collect()
        }
        (png::ColorType::GrayscaleAlpha, png::BitDepth::Eight) => px
            .chunks_exact(2)
            .flat_map(|c| [c[0], c[0], c[0], c[1]])
            .collect(),
        _ => return None,
    };
    Some((w, h, rgba))
}

/// Draw a remembered image onto a canvas, scaled to `dw`x`dh` at `dx`,`dy`.
///
/// Nearest-neighbour and source-over, which is what a picture drawn at its own
/// size needs; a challenge reading the pixels back gets the picture it sent.
pub fn draw_image(id: u32, url: &str, dx: f32, dy: f32, dw: f32, dh: f32) -> bool {
    IMAGES.with(|m| {
        let m = m.borrow();
        let Some((sw, sh, src)) = m.get(url) else {
            return false;
        };
        let (sw, sh) = (*sw as i64, *sh as i64);
        let dw = if dw > 0.0 { dw.round() as i64 } else { sw };
        let dh = if dh > 0.0 { dh.round() as i64 } else { sh };
        if dw <= 0 || dh <= 0 {
            return false;
        }
        CANVASES.with(|c| {
            let mut c = c.borrow_mut();
            let Some(pm) = c.get_mut(&id) else {
                return false;
            };
            let (cw, ch) = (pm.width() as i64, pm.height() as i64);
            let dst = pm.pixels_mut();
            let (ox, oy) = (dx.round() as i64, dy.round() as i64);
            for ty in 0..dh {
                let py = oy + ty;
                if py < 0 || py >= ch {
                    continue;
                }
                let sy = (ty * sh / dh).clamp(0, sh - 1);
                for tx in 0..dw {
                    let px = ox + tx;
                    if px < 0 || px >= cw {
                        continue;
                    }
                    let sx = (tx * sw / dw).clamp(0, sw - 1);
                    let si = ((sy * sw + sx) * 4) as usize;
                    let (r, g, b, a) = (src[si], src[si + 1], src[si + 2], src[si + 3]);
                    let di = (py * cw + px) as usize;
                    dst[di] = tiny_skia::PremultipliedColorU8::from_rgba(
                        mul(r, a),
                        mul(g, a),
                        mul(b, a),
                        a,
                    )
                    .unwrap_or_else(|| {
                        tiny_skia::PremultipliedColorU8::from_rgba(0, 0, 0, 0).unwrap()
                    });
                }
            }
            true
        })
    })
}

/// `drawImage(sourceCanvas, …)`: draw one canvas onto another. Chrome accepts
/// any pixel source (canvas element, `OffscreenCanvas`, `ImageBitmap`);
/// Cloudflare's collector draws its `OffscreenCanvas` this way.
///
/// The source rect is in source coordinates: 9-arg `drawImage` takes a
/// sub-rectangle (sprite sheets).
#[allow(clippy::too_many_arguments)]
pub fn blit(
    dst_id: u32,
    src_id: u32,
    sx: f32,
    sy: f32,
    sw_in: f32,
    sh_in: f32,
    dx: f32,
    dy: f32,
    dw: f32,
    dh: f32,
) -> bool {
    if dst_id == src_id {
        return false;
    }
    // Snapshot the source pixels first: two borrows of one map at once are not
    // possible, and a copy is simpler than splitting the storage.
    let src = CANVASES.with(|c| {
        c.borrow()
            .get(&src_id)
            .map(|pm| (pm.width() as i64, pm.height() as i64, pm.data().to_vec()))
    });
    let Some((full_w, full_h, src)) = src else {
        return false;
    };
    if full_w <= 0 || full_h <= 0 {
        return false;
    }
    // Source rect, defaulting to the whole image.
    let (ox_s, oy_s) = (sx.round() as i64, sy.round() as i64);
    let sw = if sw_in > 0.0 {
        sw_in.round() as i64
    } else {
        full_w
    };
    let sh = if sh_in > 0.0 {
        sh_in.round() as i64
    } else {
        full_h
    };
    if sw <= 0 || sh <= 0 {
        return false;
    }
    let dw = if dw > 0.0 { dw.round() as i64 } else { sw };
    let dh = if dh > 0.0 { dh.round() as i64 } else { sh };
    if dw <= 0 || dh <= 0 {
        return false;
    }
    CANVASES.with(|c| {
        let mut c = c.borrow_mut();
        let Some(pm) = c.get_mut(&dst_id) else {
            return false;
        };
        let (cw, ch) = (pm.width() as i64, pm.height() as i64);
        let dst = pm.pixels_mut();
        let (ox, oy) = (dx.round() as i64, dy.round() as i64);
        for ty in 0..dh {
            let py = oy + ty;
            if py < 0 || py >= ch {
                continue;
            }
            let sy = oy_s + (ty * sh / dh).clamp(0, sh - 1);
            for tx in 0..dw {
                let px = ox + tx;
                if px < 0 || px >= cw {
                    continue;
                }
                let sx = ox_s + (tx * sw / dw).clamp(0, sw - 1);
                if sx < 0 || sx >= full_w || sy < 0 || sy >= full_h {
                    continue;
                }
                let si = ((sy * full_w + sx) * 4) as usize;
                // Source and destination are both premultiplied, so composite source-over
                // directly.
                let (sr, sg, sb, sa) = (src[si], src[si + 1], src[si + 2], src[si + 3]);
                let di = (py * cw + px) as usize;
                let old = dst[di];
                let inv = 255 - sa as u16;
                let over = |s: u8, d: u8| -> u8 {
                    (s as u16 + (d as u16 * inv + 127) / 255).min(255) as u8
                };
                dst[di] = tiny_skia::PremultipliedColorU8::from_rgba(
                    over(sr, old.red()),
                    over(sg, old.green()),
                    over(sb, old.blue()),
                    over(sa, old.alpha()),
                )
                .unwrap_or(old);
            }
        }
        true
    })
}

/// Straight alpha to premultiplied, the form tiny-skia stores.
fn mul(c: u8, a: u8) -> u8 {
    ((u16::from(c) * u16::from(a) + 127) / 255) as u8
}

/// Drop a canvas surface (the JS wrapper was garbage-collected).
pub fn destroy(id: u32) {
    CANVASES.with(|c| {
        c.borrow_mut().remove(&id);
    });
}

/// `fillRect(x, y, w, h)` with a straight-alpha RGBA color.
pub fn fill_rect(id: u32, x: f32, y: f32, w: f32, h: f32, rgba: [u8; 4], sh: &[f32], mode: u32) {
    CANVASES.with(|c| {
        if let Some(pm) = c.borrow_mut().get_mut(&id) {
            if let Some(rect) = Rect::from_xywh(x, y, w, h) {
                paint_shadow(pm, sh, |sp, col| {
                    let mut p2 = Paint::default();
                    p2.set_color_rgba8(col[0], col[1], col[2], col[3]);
                    p2.anti_alias = true;
                    sp.fill_rect(rect, &p2, Transform::identity(), None);
                });
            }
            let mut paint = Paint::default();
            paint.set_color_rgba8(rgba[0], rgba[1], rgba[2], rgba[3]);
            paint.anti_alias = true;
            paint.blend_mode = blend_mode(mode);
            if let Some(rect) = Rect::from_xywh(x, y, w, h) {
                pm.fill_rect(rect, &paint, Transform::identity(), None);
            }
        }
    });
}

/// `clearRect(x, y, w, h)` — set the region back to transparent.
pub fn clear_rect(id: u32, x: f32, y: f32, w: f32, h: f32) {
    CANVASES.with(|c| {
        if let Some(pm) = c.borrow_mut().get_mut(&id) {
            let mut paint = Paint::default();
            paint.set_color_rgba8(0, 0, 0, 0);
            paint.blend_mode = tiny_skia::BlendMode::Source; // overwrite, don't blend
            if let Some(rect) = Rect::from_xywh(x, y, w, h) {
                pm.fill_rect(rect, &paint, Transform::identity(), None);
            }
        }
    });
}

/// Build a [`tiny_skia::Path`] from a flat verb stream: `0,x,y` = moveTo,
/// `1,x,y` = lineTo, `4` = close. Curves and arcs are tessellated to line
/// segments on the JS side, so this stays a simple, robust decoder.
fn path_from_verbs(verbs: &[f32]) -> Option<tiny_skia::Path> {
    let mut pb = PathBuilder::new();
    let mut i = 0;
    while i < verbs.len() {
        match verbs[i].round() as i32 {
            0 if i + 2 < verbs.len() => {
                pb.move_to(verbs[i + 1], verbs[i + 2]);
                i += 3;
            }
            1 if i + 2 < verbs.len() => {
                pb.line_to(verbs[i + 1], verbs[i + 2]);
                i += 3;
            }
            4 => {
                pb.close();
                i += 1;
            }
            _ => break, // unknown/truncated verb — stop rather than misread
        }
    }
    pb.finish()
}

/// `fill()` a tessellated path with a straight-alpha RGBA color. `even_odd`
/// selects the fill rule (canvas `'evenodd'` vs default nonzero winding).
/// Canvas shadow as Chrome draws it: the shape filled with the shadow color,
/// Gaussian-blurred and offset, then the shape on top. Without it the canvas
/// fingerprint loses most of its ink (the blur covers the canvas with faint
/// alpha).
///
/// Sigma is half of `shadowBlur`, as in Chrome.
fn gaussian_blur(data: &mut [u8], w: usize, h: usize, sigma: f32) {
    if sigma <= 0.0 || w == 0 || h == 0 {
        return;
    }
    // A real kernel, not three box passes: the box approximation gave a shadow
    // twice as wide as Chrome's.
    let radius = ((sigma * 3.0).ceil() as usize).min(128);
    let mut kernel = Vec::with_capacity(radius * 2 + 1);
    let denom = 2.0 * sigma * sigma;
    let mut total = 0.0f32;
    for i in 0..=(radius * 2) {
        let x = i as f32 - radius as f32;
        let v = (-(x * x) / denom).exp();
        kernel.push(v);
        total += v;
    }
    for v in kernel.iter_mut() {
        *v /= total;
    }
    let at = |i: isize, lo: isize, hi: isize| i.clamp(lo, hi) as usize;
    // Keep the intermediate pass in floats: rounding to bytes between passes
    // widens the tail.
    let mut tmp = vec![0.0f32; data.len()];
    for y in 0..h {
        let row = y * w * 4;
        for x in 0..w {
            let mut acc = [0.0f32; 4];
            for (k, wgt) in kernel.iter().enumerate() {
                let sx = at(x as isize + k as isize - radius as isize, 0, w as isize - 1);
                let si = row + sx * 4;
                for ch in 0..4 {
                    acc[ch] += f32::from(data[si + ch]) * wgt;
                }
            }
            let di = row + x * 4;
            for ch in 0..4 {
                tmp[di + ch] = acc[ch];
            }
        }
    }
    for x in 0..w {
        for y in 0..h {
            let mut acc = [0.0f32; 4];
            for (k, wgt) in kernel.iter().enumerate() {
                let sy = at(y as isize + k as isize - radius as isize, 0, h as isize - 1);
                let si = (sy * w + x) * 4;
                for ch in 0..4 {
                    acc[ch] += tmp[si + ch] * wgt;
                }
            }
            let di = (y * w + x) * 4;
            for ch in 0..4 {
                data[di + ch] = acc[ch].round().clamp(0.0, 255.0) as u8;
            }
        }
    }
}

/// `[blur, dx, dy, r, g, b, a]`; None when there is no shadow.
fn shadow_of(sh: &[f32]) -> Option<(f32, f32, f32, [u8; 4])> {
    if sh.len() < 7 {
        return None;
    }
    let a = sh[6].round().clamp(0.0, 255.0) as u8;
    if a == 0 {
        return None;
    }
    let (blur, dx, dy) = (sh[0].max(0.0), sh[1], sh[2]);
    if blur <= 0.0 && dx == 0.0 && dy == 0.0 {
        return None;
    }
    Some((
        blur,
        dx,
        dy,
        [
            sh[3].round().clamp(0.0, 255.0) as u8,
            sh[4].round().clamp(0.0, 255.0) as u8,
            sh[5].round().clamp(0.0, 255.0) as u8,
            a,
        ],
    ))
}

/// Draw a shape's shadow: `draw` paints the shape in the shadow color on a
/// blank canvas of the same size, then blur and offset.
fn paint_shadow<F>(pm: &mut tiny_skia::Pixmap, sh: &[f32], draw: F)
where
    F: FnOnce(&mut tiny_skia::Pixmap, [u8; 4]),
{
    let Some((blur, dx, dy, color)) = shadow_of(sh) else {
        return;
    };
    let (w, h) = (pm.width(), pm.height());
    let Some(mut scratch) = tiny_skia::Pixmap::new(w, h) else {
        return;
    };
    draw(&mut scratch, color);
    if blur > 0.0 {
        // Sigma is half the declared blur, per the canvas spec.
        gaussian_blur(scratch.data_mut(), w as usize, h as usize, blur / 2.0);
    }
    let paint = tiny_skia::PixmapPaint::default();
    pm.draw_pixmap(
        dx.round() as i32,
        dy.round() as i32,
        scratch.as_ref(),
        &paint,
        Transform::identity(),
        None,
    );
}

/// `globalCompositeOperation` as a number. Pages draw overlapping circles in
/// different modes and read the overlap color.
fn blend_mode(i: u32) -> tiny_skia::BlendMode {
    use tiny_skia::BlendMode as B;
    match i {
        1 => B::SourceIn,
        2 => B::SourceOut,
        3 => B::SourceAtop,
        4 => B::DestinationOver,
        5 => B::DestinationIn,
        6 => B::DestinationOut,
        7 => B::DestinationAtop,
        8 => B::Plus,
        9 => B::Source,
        10 => B::Xor,
        11 => B::Multiply,
        12 => B::Screen,
        13 => B::Overlay,
        14 => B::Darken,
        15 => B::Lighten,
        16 => B::ColorDodge,
        17 => B::ColorBurn,
        18 => B::HardLight,
        19 => B::SoftLight,
        20 => B::Difference,
        21 => B::Exclusion,
        22 => B::Hue,
        23 => B::Saturation,
        24 => B::Color,
        25 => B::Luminosity,
        _ => B::SourceOver,
    }
}

pub fn fill_path(id: u32, verbs: &[f32], even_odd: bool, rgba: [u8; 4], sh: &[f32], mode: u32) {
    let Some(path) = path_from_verbs(verbs) else {
        return;
    };
    CANVASES.with(|c| {
        if let Some(pm) = c.borrow_mut().get_mut(&id) {
            let rule0 = if even_odd {
                FillRule::EvenOdd
            } else {
                FillRule::Winding
            };
            paint_shadow(pm, sh, |sp, col| {
                let mut p2 = Paint::default();
                p2.set_color_rgba8(col[0], col[1], col[2], col[3]);
                p2.anti_alias = true;
                sp.fill_path(&path, &p2, rule0, Transform::identity(), None);
            });
            let mut paint = Paint::default();
            paint.set_color_rgba8(rgba[0], rgba[1], rgba[2], rgba[3]);
            paint.anti_alias = true;
            paint.blend_mode = blend_mode(mode);
            let rule = if even_odd {
                FillRule::EvenOdd
            } else {
                FillRule::Winding
            };
            pm.fill_path(&path, &paint, rule, Transform::identity(), None);
        }
    });
}

/// `fill()` from path ops and the canvas matrix, rasterized by Skia (see `crate::skia`).
pub fn fill_ops(
    id: u32,
    ops: &[f32],
    ctm: [f32; 6],
    even_odd: bool,
    rgba: [u8; 4],
    sh: &[f32],
    mode: u32,
) {
    CANVASES.with(|c| {
        if let Some(pm) = c.borrow_mut().get_mut(&id) {
            let (w, h) = (pm.width(), pm.height());
            let shadow = crate::skia::Shadow::parse(sh);
            crate::skia::fill_ops_paint(
                pm.data_mut(),
                w,
                h,
                ops,
                ctm,
                even_odd,
                &crate::skia::PaintKind::Solid(rgba),
                shadow,
                mode,
            );
        }
    });
}

/// `fill()` with a gradient; descriptor as in `fill_path_grad`.
pub fn fill_ops_grad(
    id: u32,
    ops: &[f32],
    ctm: [f32; 6],
    even_odd: bool,
    grad: &[f32],
    sh: &[f32],
    mode: u32,
) {
    let Some(desc) = crate::skia::gradient::GradientDesc::parse(grad) else {
        return;
    };
    CANVASES.with(|c| {
        if let Some(pm) = c.borrow_mut().get_mut(&id) {
            let (w, h) = (pm.width(), pm.height());
            let shadow = crate::skia::Shadow::parse(sh);
            crate::skia::fill_ops_paint(
                pm.data_mut(),
                w,
                h,
                ops,
                ctm,
                even_odd,
                &crate::skia::PaintKind::Gradient(desc),
                shadow,
                mode,
            );
        }
    });
}

/// `stroke()` from path ops (Skia hairline/stroker). Returns false when the
/// caller must use the old path.
#[allow(clippy::too_many_arguments)]
pub fn stroke_ops(
    id: u32,
    ops: &[f32],
    ctm: [f32; 6],
    line: &crate::skia::LineStyle,
    rgba: [u8; 4],
    grad: &[f32],
    sh: &[f32],
    mode: u32,
) -> bool {
    let paint = match crate::skia::gradient::GradientDesc::parse(grad) {
        Some(desc) if !grad.is_empty() => crate::skia::PaintKind::Gradient(desc),
        _ => crate::skia::PaintKind::Solid(rgba),
    };
    CANVASES.with(|c| {
        if let Some(pm) = c.borrow_mut().get_mut(&id) {
            let (w, h) = (pm.width(), pm.height());
            let shadow = crate::skia::Shadow::parse(sh);
            return crate::skia::stroke_ops(
                pm.data_mut(),
                w,
                h,
                ops,
                ctm,
                line,
                &paint,
                shadow,
                mode,
            );
        }
        true
    })
}

/// Decode a flat gradient descriptor into a tiny-skia [`Shader`]:
/// `[type, x0,y0, x1,y1, r0,r1, nstops, (pos,r,g,b,a)×nstops]` — `type` 0 linear,
/// 1 radial; colors are straight-alpha 0..255. Canvas's inner radius `r0` is
/// approximated away (mapped to the focal point), which is invisible for the
/// usual `r0 = 0` fingerprint gradients.
fn shader_from_grad(g: &[f32]) -> Option<Shader<'static>> {
    if g.len() < 8 {
        return None;
    }
    let ty = g[0].round() as i32;
    let (x0, y0, x1, y1, r1) = (g[1], g[2], g[3], g[4], g[6]);
    let n = g[7].max(0.0) as usize;
    let mut raw: Vec<(f32, Color)> = Vec::with_capacity(n);
    let mut idx = 8;
    for _ in 0..n {
        if idx + 5 > g.len() {
            break;
        }
        let pos = g[idx].clamp(0.0, 1.0);
        let color = Color::from_rgba8(
            g[idx + 1] as u8,
            g[idx + 2] as u8,
            g[idx + 3] as u8,
            g[idx + 4] as u8,
        );
        raw.push((pos, color));
        idx += 5;
    }
    if raw.is_empty() {
        return None;
    }
    if raw.len() == 1 {
        raw.push((1.0, raw[0].1)); // tiny-skia needs ≥2 stops; a lone stop → solid
    }
    let stops: Vec<GradientStop> = raw
        .into_iter()
        .map(|(p, c)| GradientStop::new(p, c))
        .collect();
    if ty == 1 {
        RadialGradient::new(
            Point::from_xy(x0, y0),
            Point::from_xy(x1, y1),
            r1.max(0.01),
            stops,
            SpreadMode::Pad,
            Transform::identity(),
        )
    } else {
        LinearGradient::new(
            Point::from_xy(x0, y0),
            Point::from_xy(x1, y1),
            stops,
            SpreadMode::Pad,
            Transform::identity(),
        )
    }
}

/// `fill()` a tessellated path with a linear/radial gradient (see
/// [`shader_from_grad`] for the descriptor layout).
pub fn fill_path_grad(id: u32, verbs: &[f32], even_odd: bool, grad: &[f32], sh: &[f32], mode: u32) {
    let Some(path) = path_from_verbs(verbs) else {
        return;
    };
    let Some(shader) = shader_from_grad(grad) else {
        return;
    };
    CANVASES.with(|c| {
        if let Some(pm) = c.borrow_mut().get_mut(&id) {
            // Gradient fill shadow is solid shadow color: the browser blurs the shape's
            // silhouette, not its paint.
            let rule0 = if even_odd {
                FillRule::EvenOdd
            } else {
                FillRule::Winding
            };
            paint_shadow(pm, sh, |sp, col| {
                let mut p2 = Paint::default();
                p2.set_color_rgba8(col[0], col[1], col[2], col[3]);
                p2.anti_alias = true;
                sp.fill_path(&path, &p2, rule0, Transform::identity(), None);
            });
            let paint = Paint {
                shader,
                anti_alias: true,
                blend_mode: blend_mode(mode),
                ..Paint::default()
            };
            let rule = if even_odd {
                FillRule::EvenOdd
            } else {
                FillRule::Winding
            };
            pm.fill_path(&path, &paint, rule, Transform::identity(), None);
        }
    });
}

/// `stroke()` a tessellated path with `line_width` and a straight-alpha color.
pub fn stroke_path(id: u32, verbs: &[f32], line_width: f32, rgba: [u8; 4], sh: &[f32], mode: u32) {
    let Some(path) = path_from_verbs(verbs) else {
        return;
    };
    CANVASES.with(|c| {
        if let Some(pm) = c.borrow_mut().get_mut(&id) {
            let stroke = Stroke {
                width: line_width.max(0.0),
                ..Stroke::default()
            };
            paint_shadow(pm, sh, |sp, col| {
                let mut p2 = Paint::default();
                p2.set_color_rgba8(col[0], col[1], col[2], col[3]);
                p2.anti_alias = true;
                sp.stroke_path(&path, &p2, &stroke, Transform::identity(), None);
            });
            let mut paint = Paint::default();
            paint.set_color_rgba8(rgba[0], rgba[1], rgba[2], rgba[3]);
            paint.anti_alias = true;
            paint.blend_mode = blend_mode(mode);
            pm.stroke_path(&path, &paint, &stroke, Transform::identity(), None);
        }
    });
}

/// Composite a coverage-weighted straight-alpha color over one premultiplied pixel.
fn blend_over(data: &mut [u8], i: usize, rgba: [u8; 4], coverage: f32) {
    let sa = (rgba[3] as f32 / 255.0) * coverage.clamp(0.0, 1.0); // src alpha 0..1
    if sa <= 0.0 {
        return;
    }
    // src premultiplied; dst is already premultiplied (tiny-skia).
    let sr = rgba[0] as f32 / 255.0 * sa;
    let sg = rgba[1] as f32 / 255.0 * sa;
    let sb = rgba[2] as f32 / 255.0 * sa;
    let inv = 1.0 - sa;
    let out = |src: f32, dst: u8| {
        ((src + (dst as f32 / 255.0) * inv) * 255.0)
            .round()
            .clamp(0.0, 255.0) as u8
    };
    data[i] = out(sr, data[i]);
    data[i + 1] = out(sg, data[i + 1]);
    data[i + 2] = out(sb, data[i + 2]);
    data[i + 3] = out(sa, data[i + 3]);
}

/// `fillText(text, x, y)` — rasterize real glyphs of the bundled font at `size_px`,
/// `y` being the alphabetic baseline (as canvas specifies), composited into the
/// surface. This is the fingerprint-critical op: real, deterministic text pixels
/// instead of a synthesized pattern.
pub fn fill_text(
    id: u32,
    text: &str,
    x: f32,
    y: f32,
    size_px: f32,
    rgba: [u8; 4],
    families: &str,
    bold: bool,
    italic: bool,
    sh: &[f32],
) {
    if size_px <= 0.0 || text.is_empty() {
        return;
    }
    let chain = resolve_chain(families, bold, italic);
    if chain.is_empty() {
        return;
    }
    CANVASES.with(|c| {
        let mut map = c.borrow_mut();
        let Some(pm) = map.get_mut(&id) else {
            return;
        };
        // Text shadow: the same glyphs in the shadow color, blurred and offset. The
        // challenge draws text with a shadow; without it most canvas coverage is lost.
        let (glyphs_of, _) = shape(&chain, text, size_px);
        let glyphs = |target: &mut [u8], tw: i32, th: i32, colour: [u8; 4]| {
            for g in &glyphs_of {
                let cf = g.font;
                let cscale = px_scale(cf, size_px);
                let glyph =
                    g.id.with_scale_and_position(cscale, ab_glyph::point(x + g.x as f32, y));
                if let Some(og) = cf.outline_glyph(glyph) {
                    let bb = og.px_bounds();
                    og.draw(|gx, gy, coverage| {
                        let px = bb.min.x as i32 + gx as i32;
                        let py = bb.min.y as i32 + gy as i32;
                        if px < 0 || py < 0 || px >= tw || py >= th {
                            return;
                        }
                        blend_over(target, ((py * tw + px) * 4) as usize, colour, coverage);
                    });
                }
            }
        };
        paint_shadow(pm, sh, |sp, col| {
            let (sw, shh) = (sp.width() as i32, sp.height() as i32);
            glyphs(sp.data_mut(), sw, shh, col);
        });
        let (pw, ph) = (pm.width() as i32, pm.height() as i32);
        let data = pm.data_mut();
        {
            glyphs(data, pw, ph, rgba);
        }
    });
}

/// `fillText`/`strokeText` like Chrome; false means not supported yet
/// (stroke) and the caller uses the old path.
#[allow(clippy::too_many_arguments)]
pub fn text_ops(
    id: u32,
    text: &str,
    x: f32,
    y: f32,
    ctm: [f32; 6],
    size: f32,
    families: &str,
    bold: bool,
    italic: bool,
    stroke: bool,
    line: &crate::skia::LineStyle,
    rgba: [u8; 4],
    grad: &[f32],
    sh: &[f32],
    mode: u32,
    align: u32,
    baseline: u32,
) -> bool {
    let chain = resolve_chain(families, bold, italic);
    if chain.is_empty() || text.is_empty() {
        return true;
    }
    let eff = crate::skia::text::effective_size(size);
    if !(eff > 0.0) {
        return true;
    }
    let (glyphs, width, fonts) = shape_blink(&chain, text, eff);
    let paint = match crate::skia::gradient::GradientDesc::parse(grad) {
        Some(desc) if !grad.is_empty() => crate::skia::PaintKind::Gradient(desc),
        _ => crate::skia::PaintKind::Solid(rgba),
    };
    let mut ok = true;
    CANVASES.with(|c| {
        if let Some(pm) = c.borrow_mut().get_mut(&id) {
            let (w, h) = (pm.width(), pm.height());
            let shadow = crate::skia::Shadow::parse(sh);
            ok = crate::skia::draw_text(
                pm.data_mut(),
                w,
                h,
                &fonts,
                &glyphs,
                width,
                x,
                y,
                ctm,
                eff,
                align,
                baseline,
                if stroke { Some(line) } else { None },
                &paint,
                shadow,
                mode,
            );
        }
    });
    ok
}

/// `measureText(text).width` for the bundled font at `size_px`.
pub fn measure_text(
    text: &str,
    size_px: f32,
    families: &str,
    bold: bool,
    italic: bool,
) -> TextMetrics {
    if size_px <= 0.0 {
        return TextMetrics::default();
    }
    let chain = resolve_chain(families, bold, italic);
    if chain.is_empty() {
        return TextMetrics::default();
    }
    let font = chain[0];
    let upem = font.units_per_em().unwrap_or(1000.0);
    // Ink bounds: Chrome reports horizontal bounds fractional (from the outline)
    // and vertical bounds whole (from the raster), hence two sources.
    let (mut ink_l, mut ink_r) = (f64::MAX, f64::MIN);
    let (mut ink_t, mut ink_b) = (f64::MAX, f64::MIN);
    let (glyphs, _old_width) = shape(&chain, text, size_px);
    // Width from Blink layout (size to 1/100, Skia advances, HarfBuzz kerning).
    let eff = crate::skia::text::effective_size(size_px);
    let (bglyphs, bwidth, bfonts) = shape_blink(&chain, text, eff);
    let width = bwidth as f64;
    // Ink bounds of outline glyphs as in Skia generateMetrics: hinted outline,
    // roundOut bounds, offset to the glyph position. This gives Chrome's 0 descent
    // for "Hello" (hinting removes the "o" overshoot) and 9 ascent for 13.3px
    // Arial; unhinted ab_glyph gave 1 and 10.
    {
        use crate::skia::geometry::Matrix;
        let refs: Vec<Option<skrifa::FontRef>> = bfonts
            .iter()
            .map(|b| skrifa::FontRef::new(b).ok())
            .collect();
        let scalers: Vec<Option<crate::skia::text::Scaler>> = bfonts
            .iter()
            .zip(refs.iter())
            .map(|(b, f)| {
                f.as_ref()
                    .and_then(|f| crate::skia::text::Scaler::new(b, f, eff, &Matrix::IDENTITY))
            })
            .collect();
        for g in &bglyphs {
            let Some(Some(sc)) = scalers.get(g.font) else {
                continue;
            };
            let Some(path) = sc.path(g.gid) else { continue };
            if path.pts.is_empty() {
                continue;
            }
            let ir = path.bounds().round_out();
            if ir.width() <= 0 || ir.height() <= 0 {
                continue;
            }
            ink_l = ink_l.min(g.x as f64 + ir.left as f64);
            ink_r = ink_r.max(g.x as f64 + ir.right as f64);
            ink_t = ink_t.min(ir.top as f64);
            ink_b = ink_b.max(ir.bottom as f64);
        }
    }
    for g in &glyphs {
        // Glyph from the first family in the chain that has it; size computed from
        // that font's metrics.
        let cf = g.font;
        let cscale = px_scale(cf, size_px);
        // The ink box is rounded in the glyph's own space, then offset into place.
        // Chrome: "A" right edge 7 (whole), "AV" 12.928 (5.928 kerned origin of
        // "V" + its box 7). Rounding after the offset would make both whole.
        let glyph =
            g.id.with_scale_and_position(cscale, ab_glyph::point(0.0, 0.0));
        if cf.outline_glyph(glyph).is_some() {
            // Outline glyphs were handled above via Skia.
        } else if let Some(img) =
            shaper(cf).and_then(|f| raster_image(f, ttf_parser::GlyphId(g.id.0), size_px))
        {
            // Bitmap glyph: ink bounds are its edges scaled to the size and rounded
            // outward, like outline glyphs.
            let k = size_px as f64 / img.pixels_per_em as f64;
            ink_l = ink_l.min(g.x + (img.x as f64 * k).floor());
            ink_r = ink_r.max(g.x + ((img.x as f64 + img.width as f64) * k).ceil());
            // Bitmap `y` is from the baseline to its bottom, so the top is `y + height`.
            ink_t = ink_t.min(-(((img.y as f64 + img.height as f64) * k).ceil()));
            ink_b = ink_b.max((-(img.y as f64) * k).ceil());
        }
    }
    let none = ink_l > ink_r;
    let flat = ink_t > ink_b;
    TextMetrics {
        width,
        // Chrome truncates the left bound toward zero rather than rounding: ink
        // starting 0.8px right of origin gives 0, 1.5px gives -1.
        left: if none { 0.0 } else { (-ink_l).trunc() },
        right: if none { 0.0 } else { ink_r },
        ascent: if flat { 0.0 } else { -ink_t },
        descent: if flat { 0.0 } else { ink_b },
        font_ascent: (font.ascent_unscaled() / upem * size_px).round() as f64,
        font_descent: (-font.descent_unscaled() / upem * size_px).round() as f64,
        line: ((font.ascent_unscaled() - font.descent_unscaled() + font.line_gap_unscaled()) / upem
            * size_px) as f64,
    }
}

/// `putImageData(data, x, y)` — overwrite a `w`×`h` region with straight-alpha
/// RGBA (premultiplying into the surface). Replaces, does not blend, as the spec
/// requires. Pixels outside the surface are dropped.
pub fn put_image_data(id: u32, x: i32, y: i32, w: u32, h: u32, data: &[u8]) {
    CANVASES.with(|c| {
        let mut map = c.borrow_mut();
        let Some(pm) = map.get_mut(&id) else {
            return;
        };
        let (pw, ph) = (pm.width() as i32, pm.height() as i32);
        let out = pm.data_mut();
        for row in 0..h as i32 {
            for col in 0..w as i32 {
                let (dx, dy) = (x + col, y + row);
                if dx < 0 || dy < 0 || dx >= pw || dy >= ph {
                    continue;
                }
                let si = ((row * w as i32 + col) * 4) as usize;
                if si + 3 >= data.len() {
                    continue;
                }
                let a = data[si + 3] as u32;
                let prem = |v: u8| ((v as u32 * a + 127) / 255) as u8;
                let di = ((dy * pw + dx) * 4) as usize;
                out[di] = prem(data[si]);
                out[di + 1] = prem(data[si + 1]);
                out[di + 2] = prem(data[si + 2]);
                out[di + 3] = a as u8;
            }
        }
    });
}

/// Straight (un-premultiplied) RGBA for a `w`×`h` region at `(x, y)` — exactly what
/// canvas `getImageData` returns. Pixels outside the surface read as transparent.
pub fn get_image_data(id: u32, x: u32, y: u32, w: u32, h: u32) -> Vec<u8> {
    CANVASES.with(|c| {
        let map = c.borrow();
        let mut out = vec![0u8; (w as usize) * (h as usize) * 4];
        let Some(pm) = map.get(&id) else {
            return out;
        };
        let (pw, ph) = (pm.width(), pm.height());
        let data = pm.data(); // premultiplied RGBA8
        for row in 0..h {
            for col in 0..w {
                let (sx, sy) = (x + col, y + row);
                if sx >= pw || sy >= ph {
                    continue;
                }
                let si = ((sy * pw + sx) * 4) as usize;
                let di = ((row * w + col) * 4) as usize;
                // Premul -> straight, like Chrome's `readPixels(kUnpremul)`: float division,
                // round half to even.
                crate::skia::read_unpremul(&data[si..si + 4], &mut out[di..di + 4]);
            }
        }
        out
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fill_then_read_is_exact() {
        create(1, 4, 4);
        fill_rect(1, 0.0, 0.0, 4.0, 4.0, [255, 0, 0, 255], &[], 0);
        let px = get_image_data(1, 0, 0, 1, 1);
        assert_eq!(px, vec![255, 0, 0, 255], "opaque red fill reads back red");
        clear_rect(1, 0.0, 0.0, 4.0, 4.0);
        assert_eq!(
            get_image_data(1, 0, 0, 1, 1),
            vec![0, 0, 0, 0],
            "cleared → transparent"
        );
        destroy(1);
    }

    #[test]
    fn fill_text_draws_real_glyph_pixels() {
        create(2, 40, 40);
        // Baseline near the bottom so a 24px 'H' lands inside the surface.
        fill_text(
            2,
            "H",
            4.0,
            30.0,
            24.0,
            [0, 0, 0, 255],
            "sans-serif",
            false,
            false,
            &[],
        );
        let px = get_image_data(2, 0, 0, 40, 40);
        let opaque = px.chunks_exact(4).filter(|p| p[3] > 0).count();
        assert!(
            opaque > 20,
            "glyph 'H' must cover real pixels, got {opaque}"
        );
        destroy(2);
    }

    #[test]
    fn a_pair_of_letters_is_kerned_like_the_browser() {
        // "AV" is narrower than "A" + "V": the font kerns the pair.
        let av = measure_text("AV", 16.0, "Liberation Sans", false, false).width;
        let a = measure_text("A", 16.0, "Liberation Sans", false, false).width;
        let v = measure_text("V", 16.0, "Liberation Sans", false, false).width;
        assert!(av < a + v - 0.5, "pair not kerned: {av} vs {} apart", a + v);
        // Chrome here: 20.156 vs 21.344 unkerned.
        assert!(
            (av - 20.15625).abs() < 0.001,
            "\"AV\" width differs from Chrome: {av}"
        );
    }

    #[test]
    fn a_space_breaks_the_kerning_pair() {
        // Blink shapes per word: no kerning across a space, although Liberation
        // Serif kerns "space + W".
        let whole = measure_text("To Wave", 16.0, "Liberation Serif", false, false).width;
        let to = measure_text("To", 16.0, "Liberation Serif", false, false).width;
        let space = measure_text(" ", 16.0, "Liberation Serif", false, false).width;
        let wave = measure_text("Wave", 16.0, "Liberation Serif", false, false).width;
        assert!(
            (whole - (to + space + wave)).abs() < 1e-9,
            "words must add up without kerning: {whole} vs {}",
            to + space + wave
        );
    }

    #[test]
    fn a_ligature_narrows_the_string() {
        // DejaVu Sans "ffi" is one ligature of 1980 units vs 2011 for three glyphs;
        // the browser applies it.
        let ffi = measure_text("ffi", 16.0, "DejaVu Sans", false, false).width;
        let apart = measure_text("f", 16.0, "DejaVu Sans", false, false).width * 2.0
            + measure_text("i", 16.0, "DejaVu Sans", false, false).width;
        assert!(ffi < apart - 0.2, "ligature not applied: {ffi} vs {apart}");
    }

    #[test]
    fn lengths_scale_through_the_same_fixed_point_as_the_browser() {
        // 600 units at 1000 upem and 16px: Chrome gives 9.600021362304688, not 9.6
        // (size via 26.6, scale via 16.16). 2048-upem fonts divide exactly.
        assert_eq!(ft_px(600, 1000, 16.0), 9.600021362304688);
        assert_eq!(ft_px(600, 1000, 13.0), 7.8000030517578125);
        assert_eq!(ft_px(1366, 2048, 16.0), 10.671875);
        assert_eq!(ft_px(-143, 2048, 16.0), -1.1171875, "signed kerning");
        assert_eq!(ft_px(0, 1000, 16.0), 0.0);
    }

    #[test]
    fn the_ink_box_is_rounded_in_the_glyph_own_space() {
        // "A" has a whole right ink bound, "AV" a fractional one: the glyph box is
        // rounded in its own space, then offset. Chrome gives 7 and 12.928.
        let a = measure_text("A", 10.0, "Liberation Sans", false, false);
        let av = measure_text("AV", 10.0, "Liberation Sans", false, false);
        assert_eq!(a.right, 7.0, "right bound of a lone \"A\"");
        assert!(
            (av.right - 12.928).abs() < 0.001,
            "right bound of \"AV\": {}",
            av.right
        );
    }

    /// Emoji are measured with the color font: 19.963302612304688 at 16px in
    /// Chrome, and ZWJ sequences too, since they shape into one glyph. Values
    /// from Chrome 151 on this machine.
    #[test]
    fn an_emoji_is_measured_by_the_colour_font() {
        let one = measure_text("😀", 16.0, "sans-serif", false, false);
        assert_eq!(one.width, 19.963302612304688, "emoji width: {}", one.width);
        assert_eq!((one.ascent, one.descent), (15.0, 4.0), "ink box");
        assert_eq!(one.right, 20.0, "right bound");
        for seq in ["👩‍❤️‍💋‍👨", "👨‍👩‍👧‍👦", "👨‍👩‍👦", "🇺🇦", "👍🏽"]
        {
            let w = measure_text(seq, 16.0, "sans-serif", false, false).width;
            assert_eq!(w, one.width, "sequence {seq} must be one glyph");
        }
        // Size scales the width like the browser: a fraction over 65536.
        assert_eq!(
            measure_text("😀", 11.0, "sans-serif", false, false).width,
            13.724761962890625
        );
        assert_eq!(
            measure_text("😀", 32.0, "sans-serif", false, false).width,
            39.926605224609375
        );
    }

    #[test]
    fn fill_path_triangle_covers_interior() {
        create(3, 20, 20);
        // A filled triangle: (2,2) (18,2) (10,18).
        let verbs = [0.0, 2.0, 2.0, 1.0, 18.0, 2.0, 1.0, 10.0, 18.0, 4.0];
        fill_path(3, &verbs, false, [0, 0, 255, 255], &[], 0);
        // Center of mass ~ (10, 7) is inside; a far corner is outside.
        let inside = get_image_data(3, 10, 7, 1, 1);
        let corner = get_image_data(3, 0, 19, 1, 1);
        assert!(
            inside[3] > 0 && inside[2] > 100,
            "interior filled blue, got {inside:?}"
        );
        assert_eq!(corner[3], 0, "outside the triangle stays transparent");
        destroy(3);
    }

    #[test]
    fn linear_gradient_fill_varies_across_the_rect() {
        create(4, 20, 4);
        // Linear red→blue across x=0..20, filling the whole surface via a rect path.
        let grad = [
            0.0, 0.0, 0.0, 20.0, 0.0, 0.0, 0.0, 2.0, // type,x0,y0,x1,y1,r0,r1,nstops
            0.0, 255.0, 0.0, 0.0, 255.0, // stop 0 @0.0 = red
            1.0, 0.0, 0.0, 255.0, 255.0, // stop 1 @1.0 = blue
        ];
        let verbs = [
            0.0, 0.0, 0.0, 1.0, 20.0, 0.0, 1.0, 20.0, 4.0, 1.0, 0.0, 4.0, 4.0,
        ];
        fill_path_grad(4, &verbs, false, &grad, &[], 0);
        let left = get_image_data(4, 1, 2, 1, 1);
        let right = get_image_data(4, 18, 2, 1, 1);
        assert!(
            left[0] > 150 && left[2] < 100,
            "left edge is red-ish, got {left:?}"
        );
        assert!(
            right[2] > 150 && right[0] < 100,
            "right edge is blue-ish, got {right:?}"
        );
        destroy(4);
    }

    #[test]
    fn measure_text_is_positive_and_scales() {
        let w1 = measure_text("nokk", 16.0, "sans-serif", false, false).width;
        let w2 = measure_text("nokk", 32.0, "sans-serif", false, false).width;
        assert!(w1 > 0.0, "non-empty text has width");
        assert!(
            w2 > w1 * 1.9,
            "2x font size ~doubles advance ({w1} vs {w2})"
        );
        assert_eq!(
            measure_text("", 16.0, "sans-serif", false, false).width,
            0.0,
            "empty text has zero width"
        );
    }

    /// Each family is measured with its own file, so measurement-based font
    /// enumeration sees distinct widths for Arial, Times and Courier. Values
    /// checked against Chrome 151 on this machine.
    #[test]
    fn each_family_is_measured_with_its_own_file() {
        let w = |fam: &str| measure_text("mmmmmmmmmmlli", 16.0, fam, false, false).width;
        let (sans, serif, mono) = (w("Arial"), w("Times New Roman"), w("Courier New"));
        assert!(
            sans != serif && serif != mono && sans != mono,
            "three families measured the same: {sans} {serif} {mono}"
        );
        // Unknown name is skipped for the next family.
        assert_eq!(
            w("NoSuchFontXYZ, Arial"),
            sans,
            "fell through to the next family"
        );
        // Bold is another file, hence another width.
        let bold = measure_text("mmmmmmmmmmlli", 16.0, "Times New Roman", true, false).width;
        assert!(
            bold > serif,
            "bold is not wider than regular: {bold} vs {serif}"
        );
    }
}
