//! `parallhex` — a binary/hex explorer showing one byte window through three
//! synchronized columns.
//!
//! The crate is a library with thin binaries on top, so more than one frontend
//! can share the geometry, colour and pixel maths. See `docs/ARCHITECTURE.md`.
//!
//! Visibility policy: the public surface is deliberately tiny — only what the
//! binaries in `src/bin/` reach for. Everything else is `pub(crate)`. This is
//! not just tidiness: `Cargo.toml` sets `clippy::pedantic = "deny"`, and several
//! pedantic lints (`must_use_candidate`, `missing_errors_doc`,
//! `missing_panics_doc`, `module_name_repetitions`) fire only on publicly
//! reachable items. Widening the surface would enable them wholesale.

// Curated `clippy::pedantic` allows (see Cargo.toml `[lints.clippy]`):
// - Packed RGB/RGBA color literals (`rgb(0x7aa2f7)`) are the idiomatic form
//   for a hex viewer's palette; forcing digit separators hurts readability.
// - UI/scroll math converts between f32, f64 and usize (pixel sizes, row
//   offsets, viewport heights); the values are bounded and the casts are
//   deliberate, so the flagged precision loss/truncation cannot occur.
// - Float equality is used only against exactly-representable constants
//   (0.0, 1.0) and in test assertions.
//
// These live here rather than in Cargo.toml because Cargo's `--allow`/`--deny`
// emission order is not controllable and last-flag-wins, so crate attributes are
// the only reliable place.
#![allow(
    clippy::unreadable_literal,
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    clippy::float_cmp
)]
// With no frontend enabled, nothing consumes `core`, so every item in it is
// legitimately dead and `warnings = "deny"` would fail the build. That
// configuration exists only to prove core is toolkit-free (`cargo build
// --no-default-features`, and the CI check that gpui stays out of the tree); any
// real build selects a frontend and keeps the lint fully active.
#![cfg_attr(not(feature = "gpui-frontend"), allow(dead_code))]

pub mod core;

#[cfg(feature = "gpui-frontend")]
pub mod gui;
