//! Proves `brand/tokens.json`, `brand/tokens.css` and `brand/tokens.rs`
//! actually agree, rather than merely looking like they do.
//!
//! # Why this cannot just diff the two generated files against each other
//!
//! `brand/_gen/tokens.py` writes both `tokens.css` and `tokens.rs` from
//! the same in-memory dictionary in the same run. A generator that had a
//! bug reading `tokens.json` - or one that stopped reading it altogether
//! and started emitting a stale cached value - could still produce a
//! `tokens.css` and a `tokens.rs` that agree with each other perfectly,
//! because they would agree with whatever the bug produced, together.
//! Comparing the two generated files to each other and nothing else is
//! exactly the self-consistency trap this project's plan warns about: it
//! passes with flying colours and proves nothing.
//!
//! So every value checked here is read from **three** independent
//! places, not two:
//!
//! 1. `brand/tokens.json`, parsed with `serde_json` - a general-purpose
//!    parser that has never heard of `brand/_gen/tokens.py` and owes it
//!    nothing.
//! 2. `brand/tokens.css`, read as plain text and scanned by hand in this
//!    file for the `--modem-*` custom properties the generator's own
//!    docstring says it writes.
//! 3. `brand/tokens.rs`, also read as plain text and scanned the same
//!    way for the `pub const` items the generator's docstring says it
//!    writes.
//!
//! Steps 2 and 3 deliberately do not `include!` `brand/tokens.rs` or
//! import `modem_tui::tokens` to compare compiled values - that would
//! only prove the Rust compiler agrees with itself about a file it
//! already parsed once. Reading the bytes back out as a string and
//! finding the substring is slower to write and is the actual test.
//!
//! A second, separate test (`theme_rs_colour_constants_match_tokens_json_exactly`)
//! does use the compiled `modem_tui::theme` API on purpose - that one
//! exists to prove `theme.rs` was rewired to the generated tokens rather
//! than merely gaining an unused import next to its old hand-pinned
//! values, which is a different claim from "the two generated files
//! agree with the JSON" and needs the real, compiled constants to check.

use std::fs;
use std::path::PathBuf;

use ratatui::style::Color;

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..")
}

fn read(rel: &str) -> String {
    let path = workspace_root().join(rel);
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("reading {path:?}: {e}"))
}

fn tokens_json() -> serde_json::Value {
    serde_json::from_str(&read("brand/tokens.json")).expect("brand/tokens.json must be valid JSON")
}

/// Slices `haystack` from just after `needle` to the next `;`, trimmed -
/// the shape both a CSS custom property declaration and a Rust `const`
/// statement share.
fn slice_after_to_semicolon<'a>(haystack: &'a str, needle: &str) -> &'a str {
    let start = haystack
        .find(needle)
        .unwrap_or_else(|| panic!("{needle:?} not found in:\n{haystack}"))
        + needle.len();
    let rest = &haystack[start..];
    let end = rest
        .find(';')
        .unwrap_or_else(|| panic!("no terminating ';' after {needle:?}"));
    rest[..end].trim()
}

/// The value of a `--modem-{name}` custom property in generated CSS text.
fn css_var<'a>(css: &'a str, name: &str) -> &'a str {
    slice_after_to_semicolon(css, &format!("--modem-{name}:"))
}

/// The value of a `pub const {name}` item in generated Rust text - the
/// part after `=`, not after the type, so `pub const X: f32 = 2.0;`
/// yields `2.0` rather than `f32 = 2.0`.
fn rust_const<'a>(rust: &'a str, name: &str) -> &'a str {
    let decl = slice_after_to_semicolon(rust, &format!("pub const {name}:"));
    // decl is now e.g. "(u8, u8, u8) = (0x0A, 0x0A, 0x0A)" or "f32 = 2.0"
    // or "&str = \"white\"" - split on the first '=' to drop the type.
    let eq = decl
        .find('=')
        .unwrap_or_else(|| panic!("no '=' in const {name} declaration: {decl:?}"));
    decl[eq + 1..].trim()
}

/// Parses a `#RRGGBB` string, from JSON or CSS, case-insensitively.
fn parse_hex(s: &str) -> (u8, u8, u8) {
    let h = s.trim().trim_start_matches('#');
    assert_eq!(h.len(), 6, "not a 6-digit hex colour: {s:?}");
    let byte = |i: usize| u8::from_str_radix(&h[i..i + 2], 16).unwrap();
    (byte(0), byte(2), byte(4))
}

/// Parses a generated Rust `(0xRR, 0xGG, 0xBB)` tuple literal.
fn parse_rust_rgb_tuple(s: &str) -> (u8, u8, u8) {
    let inner = s.trim().trim_start_matches('(').trim_end_matches(')');
    let mut parts = inner.split(',').map(str::trim);
    let byte = |p: &str| {
        u8::from_str_radix(p.trim_start_matches("0x").trim_start_matches("0X"), 16)
            .unwrap_or_else(|e| panic!("bad byte {p:?} in tuple {s:?}: {e}"))
    };
    let r = byte(parts.next().unwrap());
    let g = byte(parts.next().unwrap());
    let b = byte(parts.next().unwrap());
    (r, g, b)
}

/// Parses the leading numeric run of a string, ignoring any trailing
/// unit suffix (`px`, `rem`, `ch`, `s`) a CSS value carries and a Rust
/// `f32` literal never does.
fn parse_leading_number(s: &str) -> f64 {
    let s = s.trim();
    let end = s
        .find(|c: char| !(c.is_ascii_digit() || c == '.' || c == '-'))
        .unwrap_or(s.len());
    s[..end]
        .parse()
        .unwrap_or_else(|e| panic!("not a number: {s:?}: {e}"))
}

/// Strips the surrounding quotes from a generated `&str` literal, in
/// either CSS-text-with-no-quotes form (already bare) or Rust's
/// `"quoted"` form.
fn unquote(s: &str) -> &str {
    s.trim().trim_matches('"')
}

struct Fixture {
    json: serde_json::Value,
    css: String,
    rust: String,
}

fn load() -> Fixture {
    Fixture {
        json: tokens_json(),
        css: read("brand/tokens.css"),
        rust: read("brand/tokens.rs"),
    }
}

fn json_hex<'a>(v: &'a serde_json::Value, path: &[&str]) -> &'a str {
    let mut cur = v;
    for p in path {
        cur = &cur[*p];
    }
    cur.as_str()
        .unwrap_or_else(|| panic!("{path:?} is not a JSON string in tokens.json"))
}

fn json_num(v: &serde_json::Value, path: &[&str]) -> f64 {
    let mut cur = v;
    for p in path {
        cur = &cur[*p];
    }
    cur.as_f64()
        .unwrap_or_else(|| panic!("{path:?} is not a JSON number in tokens.json"))
}

// --- Colour: ground, the three phosphors bright and dim, error --------

#[test]
fn ground_agrees_across_json_css_and_rust() {
    let f = load();
    let json = parse_hex(json_hex(&f.json, &["colour", "ground"]));
    let css = parse_hex(css_var(&f.css, "ground"));
    let rust = parse_rust_rgb_tuple(rust_const(&f.rust, "GROUND"));
    assert_eq!(json, css, "json vs css for ground");
    assert_eq!(json, rust, "json vs rust for ground");
}

#[test]
fn error_red_agrees_across_json_css_and_rust() {
    let f = load();
    let json = parse_hex(json_hex(&f.json, &["colour", "error"]));
    let css = parse_hex(css_var(&f.css, "error"));
    let rust = parse_rust_rgb_tuple(rust_const(&f.rust, "ERROR"));
    assert_eq!(json, css, "json vs css for error");
    assert_eq!(json, rust, "json vs rust for error");
}

#[test]
fn every_phosphor_bright_and_dim_colour_agrees_across_json_css_and_rust() {
    let f = load();
    for name in ["white", "green", "amber"] {
        for shade in ["bright", "dim"] {
            let json = parse_hex(json_hex(&f.json, &["colour", "phosphor", name, shade]));
            let css = parse_hex(css_var(&f.css, &format!("{name}-{shade}")));
            let rust_const_name = format!("{}_{}", name.to_uppercase(), shade.to_uppercase());
            let rust = parse_rust_rgb_tuple(rust_const(&f.rust, &rust_const_name));
            assert_eq!(json, css, "json vs css for {name} {shade}");
            assert_eq!(json, rust, "json vs rust for {name} {shade}");
        }
    }
}

#[test]
fn the_default_phosphor_name_agrees_across_json_and_rust() {
    let f = load();
    let json = f.json["colour"]["phosphor"]["default"].as_str().unwrap();
    let rust = unquote(rust_const(&f.rust, "DEFAULT_PHOSPHOR"));
    assert_eq!(json, rust);
}

// --- Type: the terminal face and the scale -----------------------------

#[test]
fn the_type_face_agrees_across_json_css_and_rust() {
    let f = load();
    let json = f.json["type"]["face"].as_str().unwrap();
    let css = css_var(&f.css, "type-face");
    let rust = unquote(rust_const(&f.rust, "TYPE_FACE"));
    assert_eq!(json, css, "json vs css for the type face");
    assert_eq!(json, rust, "json vs rust for the type face");
}

#[test]
fn the_type_scale_agrees_across_json_css_and_rust() {
    let f = load();
    for (step, rust_name) in [
        ("small", "TYPE_SCALE_SMALL_REM"),
        ("base", "TYPE_SCALE_BASE_REM"),
        ("large", "TYPE_SCALE_LARGE_REM"),
        ("xlarge", "TYPE_SCALE_XLARGE_REM"),
    ] {
        let json = json_num(&f.json, &["type", "scale", &format!("{step}_rem")]);
        let css = parse_leading_number(css_var(&f.css, &format!("type-scale-{step}")));
        let rust = parse_leading_number(rust_const(&f.rust, rust_name));
        assert_eq!(json, css, "json vs css for type scale {step}");
        assert_eq!(json, rust, "json vs rust for type scale {step}");
    }
}

// --- Spacing: the character-cell unit and its scale --------------------

#[test]
fn the_spacing_unit_agrees_across_json_css_and_rust() {
    let f = load();
    let json = f.json["spacing"]["unit"].as_str().unwrap();
    // The CSS variable is "1ch" (a usable length), not the bare unit
    // name, so it is checked as "1" + the unit rather than byte-for-byte.
    let css = css_var(&f.css, "spacing-unit");
    assert_eq!(css, format!("1{json}"), "css spacing unit vs json unit");
    let rust = unquote(rust_const(&f.rust, "SPACING_UNIT"));
    assert_eq!(json, rust, "json vs rust for the spacing unit name");
}

#[test]
fn the_line_height_ratio_agrees_across_json_css_and_rust() {
    let f = load();
    let json = json_num(&f.json, &["spacing", "line_height_ratio"]);
    let css = parse_leading_number(css_var(&f.css, "line-height-ratio"));
    let rust = parse_leading_number(rust_const(&f.rust, "LINE_HEIGHT_RATIO"));
    assert_eq!(json, css);
    assert_eq!(json, rust);
}

#[test]
fn the_spacing_scale_agrees_across_json_css_and_rust() {
    let f = load();
    for (key, rust_name) in [
        ("xs", "SPACE_XS"),
        ("sm", "SPACE_SM"),
        ("md", "SPACE_MD"),
        ("lg", "SPACE_LG"),
    ] {
        let json = json_num(&f.json, &["spacing", "scale", key]);
        let css = parse_leading_number(css_var(&f.css, &format!("space-{key}")));
        let rust = parse_leading_number(rust_const(&f.rust, rust_name));
        assert_eq!(json, css, "json vs css for spacing scale {key}");
        assert_eq!(json, rust, "json vs rust for spacing scale {key}");
    }
}

// --- The CRT treatment: scanlines, bloom, and phosphor decay -----------

#[test]
fn the_crt_treatment_agrees_across_json_css_and_rust() {
    let f = load();
    for (key, rust_name) in [
        ("scanline_opacity", "SCANLINE_OPACITY"),
        ("scanline_pitch_px", "SCANLINE_PITCH_PX"),
        ("bloom_radius_px", "BLOOM_RADIUS_PX"),
        ("phosphor_decay_seconds", "PHOSPHOR_DECAY_SECONDS"),
    ] {
        let json = json_num(&f.json, &["crt", key]);
        let css_key = key
            .replace("_px", "")
            .replace("_seconds", "")
            .replace('_', "-");
        let css = parse_leading_number(css_var(&f.css, &css_key));
        let rust = parse_leading_number(rust_const(&f.rust, rust_name));
        assert_eq!(json, css, "json vs css for crt.{key}");
        assert_eq!(json, rust, "json vs rust for crt.{key}");
    }
}

/// Decay is called out on its own, per the design spec's "Brand"
/// section: it is the strongest CRT cue there is, and if the TUI and
/// the page disagree on it the two surfaces read as different products.
/// This pins the actual number, not just "the three sources agree with
/// each other" - 2.56s, derived in `brand/_gen/tokens.py`'s own doc from
/// `modem-tui/src/waterfall.rs`'s current `DECAY_COLUMNS` (40) and
/// `fft_size_for(8000)` (512 samples, 0.064s a column). A generator that
/// agreed with itself on the wrong number would pass every other test in
/// this file and still ship a mismatched trail.
#[test]
fn the_phosphor_decay_is_the_value_derived_from_waterfall_rs() {
    let f = load();
    let json = json_num(&f.json, &["crt", "phosphor_decay_seconds"]);
    assert_eq!(json, 2.56);
}

// --- theme.rs itself: compiled constants, not generated text -----------

fn colour_from_hex(hex: &str) -> Color {
    let (r, g, b) = parse_hex(hex);
    Color::Rgb(r, g, b)
}

/// The brief's second required test: `theme.rs`'s own colour constants,
/// as real compiled `ratatui::style::Color` values, match
/// `brand/tokens.json` exactly. This is what proves the generator
/// **replaced** the hand-pinned values Task 15 left behind rather than
/// merely sitting alongside them unused - a `theme.rs` that still hid
/// `Color::Rgb(0xFF, 0xB0, 0x00)` behind an unused `tokens` import would
/// fail every test above (which never look at `theme.rs`) but pass a
/// naive "does the crate build" check. This test looks at `theme.rs`
/// directly instead.
#[test]
fn theme_rs_colour_constants_match_tokens_json_exactly() {
    let f = load();
    let json = &f.json;

    assert_eq!(
        modem_tui::theme::GROUND,
        colour_from_hex(json_hex(json, &["colour", "ground"]))
    );
    assert_eq!(
        modem_tui::theme::WHITE,
        colour_from_hex(json_hex(json, &["colour", "phosphor", "white", "bright"]))
    );
    assert_eq!(
        modem_tui::theme::GREEN,
        colour_from_hex(json_hex(json, &["colour", "phosphor", "green", "bright"]))
    );
    assert_eq!(
        modem_tui::theme::AMBER,
        colour_from_hex(json_hex(json, &["colour", "phosphor", "amber", "bright"]))
    );
    assert_eq!(
        modem_tui::theme::error(),
        colour_from_hex(json_hex(json, &["colour", "error"]))
    );

    assert_eq!(
        modem_tui::Theme::White.dim(),
        colour_from_hex(json_hex(json, &["colour", "phosphor", "white", "dim"]))
    );
    assert_eq!(
        modem_tui::Theme::Green.dim(),
        colour_from_hex(json_hex(json, &["colour", "phosphor", "green", "dim"]))
    );
    assert_eq!(
        modem_tui::Theme::Amber.dim(),
        colour_from_hex(json_hex(json, &["colour", "phosphor", "amber", "dim"]))
    );

    assert_eq!(
        modem_tui::Theme::default().name(),
        json["colour"]["phosphor"]["default"].as_str().unwrap()
    );
}

/// Phosphor decay is authored once, in seconds, and the terminal derives
/// its own column count from it. The brief's own reason: decay is the
/// strongest CRT cue there is, and if the terminal and the page's canvas
/// disagree the two surfaces read as different products.
///
/// What this can and cannot prove, stated plainly: it cannot tell a
/// derivation from a literal that happens to equal the same value today,
/// because at runtime both are just `40.0`. What it does guarantee is
/// the thing that actually matters - **change the token and this fails
/// unless the renderer follows.** Verified by mutation: setting
/// `phosphor_decay_seconds` to 5.12 and regenerating fails this test
/// while `DECAY_COLUMNS` stays pinned.
#[test]
fn the_waterfalls_decay_is_derived_from_the_token_not_pinned_beside_it() {
    let json: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string("../brand/tokens.json").unwrap()).unwrap();
    let seconds = json["crt"]["phosphor_decay_seconds"].as_f64().unwrap() as f32;

    let expected = seconds / modem_tui::waterfall::COLUMN_SECONDS;
    assert!(
        (modem_tui::waterfall::DECAY_COLUMNS - expected).abs() < 1e-4,
        "DECAY_COLUMNS is {} but the token implies {expected}",
        modem_tui::waterfall::DECAY_COLUMNS
    );
}
