//! The raw design tokens, pulled in verbatim from `brand/tokens.rs`.
//!
//! `brand/tokens.json` is the single hand-edited source; `brand/tokens.rs`
//! and `brand/tokens.css` are both generated from it by
//! `brand/_gen/tokens.py` and must never be hand-edited - edit the JSON
//! or the generator, then regenerate:
//!
//! ```bash
//! python3 brand/_gen/tokens.py
//! ```
//!
//! This file is the one hand-written line in the chain: a fixed
//! `include!` that never changes when a token value does. [`crate::theme`]
//! is what turns the colour tuples below into `ratatui::style::Color` -
//! see that module's own doc for the history of the colours it replaced.
//! The type, spacing and CRT constants are not yet consumed by anything
//! in this crate (the page's CSS is where they matter today), but they
//! are pulled in here too, `pub`, so the workspace's tests can prove this
//! crate agrees with `brand/tokens.json` on every token this design
//! system defines, not only the ones already wired up.
//!
//! `pub` all the way from here to the crate root on purpose: an unused
//! item nested in a private module would be flagged dead code, but these
//! are generated constants a future consumer (this crate's own later
//! tasks, or another crate entirely) may read directly, exactly as
//! `brand/tokens.rs` intends.

include!(concat!(env!("CARGO_MANIFEST_DIR"), "/../brand/tokens.rs"));
