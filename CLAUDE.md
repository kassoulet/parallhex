# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project

`parallhex` — a native binary/hex explorer: one wide window showing the same byte window
through three synchronized columns (whole-file overview, per-byte pixel maps, class-colored
hex+ASCII). Files are `mmap`ed and rendered virtualized, so multi-gigabyte files open
instantly. `SPECS.md` is the design document and is kept in sync as features land — read it
for intended behavior, and update it in the same commit as a behavior change.

Note the naming skew: the directory is `entropymap`, the crate/binary and config directory
are `parallhex`.

## Commands

```sh
cargo run                     # launch with no file
cargo run -- path/to/file.bin # open a file on startup (`--` needed for dash-prefixed names)
cargo test                    # all unit tests
cargo test parse_hex_with_prefix   # one test by name
cargo fmt                     # required: `cargo fmt --check` gates commits
cargo clippy --all-targets    # required: pedantic, warnings = deny
```

Tests are inline `#[cfg(test)] mod tests` blocks in each module; there is no `tests/`
directory. Commits are gated by [`prek`](https://prek.j178.dev) (`prek.toml`) running
`cargo test --all-targets`, `cargo fmt --check` and `cargo clippy --all-targets` — run those
three before committing rather than discovering failures in the hook.

## Lints

`Cargo.toml` sets `[lints.rust] warnings = "deny"` and `[lints.clippy] pedantic = "deny"`.
A curated set of pedantic allows (packed RGB literals, the f32/f64/usize pixel-math casts,
float equality against exact constants) lives in the `#![allow(...)]` at the top of
`src/main.rs`, **not** in `Cargo.toml` — Cargo's `--allow`/`--deny` emission order isn't
controllable and last-flag-wins, so crate attributes are the only reliable place. Add new
overrides there, and prefer a local `#[allow]` with a reason over widening the crate list.

## Architecture

Six modules, deliberately layered so the pixel/geometry math is testable without a window:

- **`src/main.rs`** — CLI parsing (`parse_args` → `Cli`), the `actions!` list of every
  keyboard action, `bind_keys`, window creation, and `restored_bounds` (persisted geometry is
  only reused when it still intersects a connected display).
- **`src/app.rs`** (~3k lines) — `ParallHexApp`: the single view entity holding *all* state,
  plus its `Render` impl building the whole UI tree each frame (top bar, three columns +
  drag dividers, status bar, jump overlay). Also `JumpField`, a separate entity implementing
  `EntityInputHandler` with a hand-written `Element` (`JumpFieldElement`) — gpui 0.2 has no
  built-in text input, so the caret/selection/IME plumbing is ours.
- **`src/panes.rs`** — pure painting and geometry: `paint_hex` / `paint_pixels` /
  `paint_overview` / `paint_strip`, `RowGeo`, `hex_offset_at`, `build_*_image`, zoom
  constants and clamps.
- **`src/color.rs`** — `class_color` (binvis byte classes), `entropy_color` gradient, the
  `Colormap` enum (`color_for` is the per-byte mapping the pixels column honors; note the
  overview and strip thumbnails hardcode `Colormap::Greyscale` and ignore the selection),
  `printable`, `human_size`.
- **`src/entropy.rs`** — Shannon entropy; `block_entropies` computes one value per
  `entropy_window`-sized block in parallel with rayon. Per-byte entropy is *not* stored:
  `panes::entropy_at` interpolates between neighbouring blocks on demand.
- **`src/config.rs`** — hand-rolled `key = value` preferences file in the platform config
  dir (no serde dependency). Parsing is deliberately tolerant: unknown keys, malformed lines
  and non-finite floats are skipped, and values are clamped on load in `ParallHexApp::new`.

### A canvas has no intrinsic size

`Canvas::request_layout` refines `Style::default()` and has no children to measure, so a
canvas with no explicit size lays out **zero-height** — it silently paints nothing (or, worse,
one row, since the paint functions derive their visible-row count from `bounds.size.height`).
`.size_full()` must sit on the **canvas**, not on the parent div:

```rust
.child(canvas(prepaint, paint).size_full())   // right
.child(canvas(prepaint, paint)).size_full()   // wrong: sizes the div, canvas stays 0-tall
```

The second form compiles, looks correct, and cost this project a working view for a while.
`view_height == 0` in the status bar's row range is the tell.

### The gpui canvas pattern (most important thing to know)

`canvas()` paint closures cannot borrow the view. So `app.rs` **clones cheap snapshots**
(`Arc<Mmap>`, `Arc<Vec<f32>>` entropies, zoom, scroll, selection) into the closure and calls
the matching `panes::paint_*` free function. Everything in `panes.rs` is therefore a pure
function of its arguments — which is why it has real unit tests.

Hit-testing works off **last-frame bounds**: each canvas's *prepaint* callback does
`entity.update(...)` to store its `Bounds<Pixels>` (`hex_bounds`, `pixels_bounds`,
`overview_bounds`, `strip_bounds`, plus the slider bounds) on the app, and the mouse handlers
convert window coordinates using those stored bounds. If a pane renders but doesn't respond
to clicks, a missing or stale bounds write in prepaint is the first suspect.

### Shared scroll contract

`scroll_rows: f32` (in *rows*, not pixels) is the one scroll position for all three columns —
that's what keeps them synchronized. The **hex column is the master**: its prepaint closure
records `view_height`, resolves the one-shot requests `scroll_reset` and `scroll_to_offset`,
clamps `scroll_rows` to the content height, and recomputes `view_frac` / `view_frac_h` for
the overview and strip viewport bands. Other panes only ever write `scroll_rows`; they never
clamp it themselves.

Row layout is `row = offset / bytes_per_row`, identical in every pane. Horizontal hex
geometry has a single source of truth in `RowGeo`, built from the *measured* monospace glyph
width — paint and hit-testing both go through it, so changing cell spacing means changing
`RowGeo` only.

### Persistence

`Render` compares `current_config()` against `saved_cfg` and writes at most every 2 seconds;
`cx.on_release` and the Quit action also flush. Window geometry is captured every frame by
`capture_window_geometry`, which skips maximized/fullscreen frames so the saved bounds stay
the *un-maximize* size (gpui's `WindowBounds::Maximized` treats them that way).

## Making common changes

- **New keyboard action** — three places: the `actions!` list in `main.rs`, a
  `KeyBinding::new("secondary-…", …)` in `bind_keys`, and an `.on_action(cx.listener(Self::…))`
  on the root div in `Render`. Miss the last one and the key silently does nothing.
- **New persisted preference** — `config::Config` field + `Default` + a `parse` match arm +
  a `serialize` line + `current_config()` + the clamp in `ParallHexApp::new` + the
  `SPECS.md` preferences list. `parse_round_trip` in `config.rs` covers the round trip.
- **Layout/painting change** — put the math in `panes.rs` as a free function and unit-test
  it; keep `app.rs` limited to wiring state into it.

## Gotchas

- Accelerators are bound with `secondary-…`, which gpui resolves to Cmd on macOS and Ctrl
  everywhere else. Do **not** write `cmd-…`: gpui parses that as the literal platform
  modifier, i.e. Super on Linux/Windows, which is not what the UI labels promise. Labels that
  name the modifier go through `JUMP_BUTTON_LABEL`-style `cfg!(target_os = "macos")` consts.
- Mouse hit-testing must invert the paint formula exactly: rows are painted at
  `(row - scroll_rows) * row_h`, so the inverse adds the fractional scroll *before* flooring
  (`panes::hex_offset_at`, `panes::pixels_offset_at`). Flooring first is off by one row for
  any fractional scroll — which is the normal state after a wheel scroll or drag-pan.
- **The app draws its own window chrome on Linux.** Compositors need not implement
  `xdg-decoration`, and GNOME's Mutter doesn't. With no decoration object to negotiate with,
  gpui's `request_decorations` records the mode it was *asked* for and tells nobody — so
  asking for `Server` (the default) leaves `window_decorations()` reporting `Server` while
  nothing draws a titlebar, giving a window that can't be moved or closed. `main.rs`'s
  `DECORATIONS` asks for `Client` on Linux so the state is honest, and `render` keys the
  titlebar drag region, the window buttons and the eight resize handles off
  `Decorations::Client`. Note `start_window_move` uses the last **mouse-press** serial, so it
  must be called from `on_mouse_down` — an `on_click` handler is too late.
- gpui `Pixels` values are converted with `.to_f64() as f32` throughout; that's intentional
  and covered by the crate-level cast allows.
