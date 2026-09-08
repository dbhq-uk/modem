#!/usr/bin/env python3
"""Generates brand/tokens.css and brand/tokens.rs from brand/tokens.json.

Edit tokens.json, then regenerate - never hand-edit tokens.css or
tokens.rs. Same rule brand/_gen/icon.py already follows for the icon
assets, applied here to the tokens the terminal (`modem-tui`) and the
page (`web/`, plan 2's task 3) both render from.

    python3 brand/_gen/tokens.py

## Why this script reads tokens.json rather than declaring the values

`icon.py` hard-codes its two colours as Python literals, because there
is nothing else that has to stay in step with them. Tokens are
different: the whole point of this system is one hand-edited source and
two generated outputs, and spec 3's BBS is a third surface that will
want the same tokens without ever touching this file. So `tokens.json`
is the thing a human edits, and this script's only job is to walk it and
print it twice, in two syntaxes. Re-declaring any value here as a fresh
Python literal would be exactly the drift this system exists to
prevent, just moved one file to the left - which is why every value
below is read out of `tokens.json`, never typed a second time.

## Where the phosphor decay figure comes from

`crt.phosphor_decay_seconds` (2.56) is not a fresh guess: it is
`modem-tui`'s own waterfall trail persistence, restated in seconds so a
CSS animation can use it too. `modem-tui/src/waterfall.rs` decays a
column over `DECAY_COLUMNS` = 40 scrolled columns, and at 8 kHz each
column is one `fft_size_for(8000)` = 512-sample block, i.e. 0.064s -
40 * 0.064 = 2.56s. `waterfall.rs` still owns its own literal for now
(wiring it to this token is a follow-up, not this task); the value here
is set to already match it, so that follow-up changes nothing when it
happens. If `waterfall.rs`'s constants ever move, update this comment
and the value together, or the TUI and the page will disagree on the
single strongest CRT cue there is - see the design spec's "Brand"
section for why that matters.
"""

import json
from pathlib import Path

HERE = Path(__file__).resolve().parent
BRAND = HERE.parent
TOKENS_JSON = BRAND / "tokens.json"

BANNER = (
    "GENERATED FILE. Do not hand-edit - edit brand/tokens.json and/or "
    "brand/_gen/tokens.py, then run `python3 brand/_gen/tokens.py`."
)

PHOSPHOR_ORDER = ("white", "green", "amber")


def load_tokens() -> dict:
    return json.loads(TOKENS_JSON.read_text())


def f32(value) -> str:
    """A JSON number, formatted so it is always a valid Rust `f32`
    literal - `2` alone is not (Rust needs a decimal point or a type
    suffix), so every numeric token destined for a Rust `f32` const is
    routed through this rather than interpolated as-is.
    """
    return repr(float(value))


def css(tokens: dict) -> str:
    colour = tokens["colour"]
    phosphor = colour["phosphor"]
    type_ = tokens["type"]
    scale = type_["scale"]
    spacing = tokens["spacing"]
    sp_scale = spacing["scale"]
    unit = spacing["unit"]
    crt = tokens["crt"]

    lines = [
        "/*",
        f" * {BANNER}",
        " * Source: brand/tokens.json",
        " */",
        ":root {",
        f"  --modem-ground: {colour['ground']};",
        "",
    ]
    for name in PHOSPHOR_ORDER:
        p = phosphor[name]
        lines.append(f"  --modem-{name}-bright: {p['bright']};")
        lines.append(f"  --modem-{name}-dim: {p['dim']};")
    lines.append(f"  --modem-error: {colour['error']};")
    lines.append("")
    lines.append(f"  --modem-type-face: {type_['face']};")
    lines.append(f"  --modem-type-scale-small: {scale['small_rem']}rem;")
    lines.append(f"  --modem-type-scale-base: {scale['base_rem']}rem;")
    lines.append(f"  --modem-type-scale-large: {scale['large_rem']}rem;")
    lines.append(f"  --modem-type-scale-xlarge: {scale['xlarge_rem']}rem;")
    lines.append("")
    lines.append(f"  --modem-spacing-unit: 1{unit};")
    lines.append(f"  --modem-line-height-ratio: {spacing['line_height_ratio']};")
    for key in ("xs", "sm", "md", "lg"):
        lines.append(f"  --modem-space-{key}: {sp_scale[key]}{unit};")
    lines.append("")
    lines.append(f"  --modem-scanline-opacity: {crt['scanline_opacity']};")
    lines.append(f"  --modem-scanline-pitch: {crt['scanline_pitch_px']}px;")
    lines.append(f"  --modem-bloom-radius: {crt['bloom_radius_px']}px;")
    lines.append(f"  --modem-phosphor-decay: {crt['phosphor_decay_seconds']}s;")
    lines.append("}")
    lines.append("")
    return "\n".join(lines)


def hex_to_rust_tuple(hexcolour: str) -> str:
    h = hexcolour.lstrip("#")
    r, g, b = h[0:2], h[2:4], h[4:6]
    return f"(0x{r.upper()}, 0x{g.upper()}, 0x{b.upper()})"


def rust(tokens: dict) -> str:
    colour = tokens["colour"]
    phosphor = colour["phosphor"]
    type_ = tokens["type"]
    scale = type_["scale"]
    spacing = tokens["spacing"]
    sp_scale = spacing["scale"]
    crt = tokens["crt"]

    lines = [
        f"// {BANNER}",
        "// Source: brand/tokens.json",
        "",
        "/// Near-black ground, RGB.",
        f"pub const GROUND: (u8, u8, u8) = {hex_to_rust_tuple(colour['ground'])};",
        "",
        "/// Which phosphor is the default - `modem-tui`'s `Theme::default()`.",
        f'pub const DEFAULT_PHOSPHOR: &str = "{phosphor["default"]}";',
        "",
    ]
    for name in PHOSPHOR_ORDER:
        p = phosphor[name]
        const = name.upper()
        lines.append(f"/// Phosphor {name}, bright, RGB.")
        lines.append(
            f"pub const {const}_BRIGHT: (u8, u8, u8) = {hex_to_rust_tuple(p['bright'])};"
        )
        lines.append(f"/// Phosphor {name}, dim, RGB.")
        lines.append(
            f"pub const {const}_DIM: (u8, u8, u8) = {hex_to_rust_tuple(p['dim'])};"
        )
        lines.append("")
    lines.append("/// Error red, RGB.")
    lines.append(f"pub const ERROR: (u8, u8, u8) = {hex_to_rust_tuple(colour['error'])};")
    lines.append("")
    lines.append("/// The terminal face, as a CSS font stack - informational here.")
    lines.append("/// `ratatui` draws in the user's own terminal and cannot set a font;")
    lines.append("/// the page consumes this value directly.")
    lines.append(f'pub const TYPE_FACE: &str = "{type_["face"]}";')
    lines.append("")
    lines.append("/// Type scale, rem.")
    lines.append(f"pub const TYPE_SCALE_SMALL_REM: f32 = {f32(scale['small_rem'])};")
    lines.append(f"pub const TYPE_SCALE_BASE_REM: f32 = {f32(scale['base_rem'])};")
    lines.append(f"pub const TYPE_SCALE_LARGE_REM: f32 = {f32(scale['large_rem'])};")
    lines.append(f"pub const TYPE_SCALE_XLARGE_REM: f32 = {f32(scale['xlarge_rem'])};")
    lines.append("")
    lines.append("/// Spacing unit name - `ch`, a character's advance width - and the")
    lines.append("/// scale expressed as a multiple of it, so both surfaces keep the")
    lines.append("/// same rhythm on a character-cell grid.")
    lines.append(f'pub const SPACING_UNIT: &str = "{spacing["unit"]}";')
    lines.append(f"pub const LINE_HEIGHT_RATIO: f32 = {f32(spacing['line_height_ratio'])};")
    lines.append(f"pub const SPACE_XS: f32 = {f32(sp_scale['xs'])};")
    lines.append(f"pub const SPACE_SM: f32 = {f32(sp_scale['sm'])};")
    lines.append(f"pub const SPACE_MD: f32 = {f32(sp_scale['md'])};")
    lines.append(f"pub const SPACE_LG: f32 = {f32(sp_scale['lg'])};")
    lines.append("")
    lines.append("/// The CRT treatment: scanlines, bloom, and phosphor decay - see this")
    lines.append("/// generator's own doc for where the decay figure comes from.")
    lines.append(f"pub const SCANLINE_OPACITY: f32 = {f32(crt['scanline_opacity'])};")
    lines.append(f"pub const SCANLINE_PITCH_PX: f32 = {f32(crt['scanline_pitch_px'])};")
    lines.append(f"pub const BLOOM_RADIUS_PX: f32 = {f32(crt['bloom_radius_px'])};")
    lines.append(
        f"pub const PHOSPHOR_DECAY_SECONDS: f32 = {f32(crt['phosphor_decay_seconds'])};"
    )
    lines.append("")
    return "\n".join(lines)


def main() -> None:
    tokens = load_tokens()

    css_path = BRAND / "tokens.css"
    css_path.write_text(css(tokens))
    print(f"wrote {css_path}")

    rust_path = BRAND / "tokens.rs"
    rust_path.write_text(rust(tokens))
    print(f"wrote {rust_path}")


if __name__ == "__main__":
    main()
