//! The terminal frontend, built on ratatui.
//!
//! Renders the same three synchronized columns as the gpui frontend over the same
//! `core` geometry. The two graphical columns become half-block characters: two
//! byte rows per text row, `fg` the upper byte and `bg` the lower one.

// Scaffolding, removed once `run` is implemented. This frontend is built bottom
// up -- blitter, then state, then keymap, then the loop that drives them -- so
// each piece is transiently unused by the piece that will consume it, and
// `warnings = "deny"` would block every intermediate commit. When the allow comes
// off, the lint proves nothing here is genuinely unused.
#![allow(dead_code)]

pub(crate) mod blit;

use std::io;
use std::path::PathBuf;

/// Run the terminal UI against `file`.
///
/// # Errors
///
/// Returns the underlying `io::Error` if the file cannot be opened or mapped, or
/// if the terminal cannot be driven.
pub fn run(_file: Option<PathBuf>) -> io::Result<()> {
    Ok(())
}
