//! Toolkit-neutral core: colour, entropy, pixel maths and persisted settings.
//!
//! Nothing in here may reference a UI toolkit. Both frontends depend on it, and
//! the terminal one must stay buildable on hosts that lack gpui's link-time
//! libraries (freetype, xcb, xkbcommon).

pub(crate) mod color;
pub(crate) mod config;
pub(crate) mod entropy;
