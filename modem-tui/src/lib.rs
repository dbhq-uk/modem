//! The comms-package terminal interface: a 1990s amber-phosphor frame
//! around a real [`modem_core::session::Session`], in two layouts.
//!
//! **Single pane** - one end of a call, connected to another machine over
//! a real [`modem_audio::Transport`]. Full-width waterfall, the overture
//! stage labels along its axis.
//!
//! **Split screen** - both ends on one machine, for demonstration. See
//! [`app`]'s own module doc for the three rules that govern it and where
//! each is enforced: both panes render the same spectrum, `[DEMO MODE]`
//! is read from `Transport::is_acoustic()` and nothing else, and split
//! screen refuses to lay out below 80 columns.
//!
//! # Layout
//!
//! - [`theme`] - the amber/green/white phosphor palette.
//! - [`draw`] - bounds-checked primitives every widget draws through.
//! - [`frame`] - the double-line chrome and the shared row/column layout.
//! - [`waterfall`] - the well-shaped hole Task 16's real FFT renderer
//!   drops into.
//! - [`spectrum`] - the plain-data argument that boundary is built on.
//! - [`pane`] - one end's protocol state plus its terminal emulation.
//! - [`directory`] - the hand-editable dialling directory (Task 18).
//! - [`tokens`] - the generated design tokens `theme` builds its palette
//!   from - see that module's own doc.
//! - [`app`] - ties it all together: layout choice, focus, key handling.

pub mod app;
pub mod directory;
pub mod draw;
pub mod frame;
pub mod pane;
pub mod spectrum;
pub mod theme;
pub mod tokens;
pub mod waterfall;

pub use app::{App, LayoutMode, RequestedLayout};
pub use directory::{Directory, Entry};
pub use pane::Pane;
pub use spectrum::Spectrum;
pub use theme::Theme;
