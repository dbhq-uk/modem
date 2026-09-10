// GENERATED FILE. Do not hand-edit - edit brand/tokens.json and/or brand/_gen/tokens.py, then run `python3 brand/_gen/tokens.py`.
// Source: brand/tokens.json

/// Near-black ground, RGB.
pub const GROUND: (u8, u8, u8) = (0x0A, 0x0A, 0x0A);

/// Which phosphor is the default - `modem-tui`'s `Theme::default()`.
pub const DEFAULT_PHOSPHOR: &str = "white";

/// Phosphor white, bright, RGB.
pub const WHITE_BRIGHT: (u8, u8, u8) = (0xE8, 0xE8, 0xD8);
/// Phosphor white, dim, RGB.
pub const WHITE_DIM: (u8, u8, u8) = (0x74, 0x74, 0x6C);

/// Phosphor green, bright, RGB.
pub const GREEN_BRIGHT: (u8, u8, u8) = (0x33, 0xFF, 0x33);
/// Phosphor green, dim, RGB.
pub const GREEN_DIM: (u8, u8, u8) = (0x19, 0x80, 0x19);

/// Phosphor amber, bright, RGB.
pub const AMBER_BRIGHT: (u8, u8, u8) = (0xFF, 0xB0, 0x00);
/// Phosphor amber, dim, RGB.
pub const AMBER_DIM: (u8, u8, u8) = (0x80, 0x58, 0x00);

/// Error red, RGB.
pub const ERROR: (u8, u8, u8) = (0xFF, 0x33, 0x33);

/// The terminal face, as a CSS font stack - informational here.
/// `ratatui` draws in the user's own terminal and cannot set a font;
/// the page consumes this value directly.
pub const TYPE_FACE: &str = "'IBM Plex Mono', ui-monospace, 'Cascadia Code', 'Cascadia Mono', 'JetBrains Mono', Consolas, 'Liberation Mono', Menlo, monospace";

/// Type scale, rem.
pub const TYPE_SCALE_SMALL_REM: f32 = 0.8;
pub const TYPE_SCALE_BASE_REM: f32 = 1.0;
pub const TYPE_SCALE_LARGE_REM: f32 = 1.25;
pub const TYPE_SCALE_XLARGE_REM: f32 = 2.0;

/// Spacing unit name - `ch`, a character's advance width - and the
/// scale expressed as a multiple of it, so both surfaces keep the
/// same rhythm on a character-cell grid.
pub const SPACING_UNIT: &str = "ch";
pub const LINE_HEIGHT_RATIO: f32 = 1.4;
pub const SPACE_XS: f32 = 1.0;
pub const SPACE_SM: f32 = 2.0;
pub const SPACE_MD: f32 = 4.0;
pub const SPACE_LG: f32 = 8.0;

/// The CRT treatment: scanlines, bloom, and phosphor decay - see this
/// generator's own doc for where the decay figure comes from.
pub const SCANLINE_OPACITY: f32 = 0.15;
pub const SCANLINE_PITCH_PX: f32 = 2.0;
pub const BLOOM_RADIUS_PX: f32 = 6.0;
pub const PHOSPHOR_DECAY_SECONDS: f32 = 2.56;
