# Architecture

How ParallHex is put together, and the handful of invariants that are easy to
break by accident. For what the app *does*, see [../README.md](../README.md).

## Module layout

A library with two thin binaries on top. The layering has one governing rule:

> **`core` may not name a UI toolkit.** Both frontends depend on it, and the
> terminal one must build on hosts that lack gpui's link-time libraries
> (`libfreetype`, `libxcb`, `libxkbcommon`). CI asserts gpui cannot reach a
> TUI-only dependency tree.

```
src/
  lib.rs        crate root: the curated #![allow(...)] set, feature gates
  core/         toolkit-neutral -- no gpui, no ratatui
    cli.rs      Cli, parse_args(args, bin_name)
    color.rs    Rgb, class_color, entropy_color, fg_for_bg, Colormap
    config.rs   hand-rolled `key = value` preferences, no serde
    entropy.rs  Shannon entropy; block_entropies runs under rayon
    geom.rs     RowGeo, bytes-per-row, the anchor maths, entropy_at,
                ByteSource, Nav/nav_next, hit-testing, selection_text
    thumb.rs    build_overview_rgba, build_zoom_rgba -> plain Vec<u8>
  gui/          #[cfg(feature = "gpui-frontend")]
    mod.rs      actions!, key_bindings, DECORATIONS, restored_bounds, run()
    app.rs      ParallHexApp: all state, handlers, async work, Render
    app/ui.rs   view construction: bars, columns, dialog, chrome
    jump.rs     JumpField -- gpui 0.2 has no text input, so the
                caret/selection/IME plumbing is ours
    paint.rs    everything that names a gpui type
  tui/          #[cfg(feature = "tui-frontend")]
    mod.rs      terminal lifecycle, panic hook, event loop, run()
    app.rs      TuiApp, Focus, Action, apply() -- the state machine
    blit.rs     half-block blitter, base64, OSC 52
    input.rs    key_to_action -- the keymap, as a pure function
    render.rs   the three columns
src/bin/parallhex-gpui.rs   shim
src/bin/parallhex-tui.rs    shim
```

`mod gui`, not `mod gpui`, so the module cannot shadow the crate.

Two consequences of the rule worth knowing:

- **The public surface is deliberately tiny** — only `core::cli`, `gui::run` and
  `tui::run`. Several `clippy::pedantic` lints (`must_use_candidate`,
  `missing_errors_doc`, `missing_panics_doc`, `module_name_repetitions`) fire only
  on publicly reachable items, and `Cargo.toml` denies pedantic, so widening the
  surface enables them wholesale.
- **`lib.rs` allows `dead_code` when the gpui frontend is off.** `core` carries
  geometry only one frontend uses — the scrollbar maths and the zoom column's
  redistribution are gpui-only — so a TUI-only build legitimately leaves part of
  it unused. The default build keeps the lint active over all of `core`.

Per-byte entropy is never stored — only one value per block. `core::geom::entropy_at`
interpolates between neighbouring blocks on demand.

## What the two frontends share

Everything about what a byte *means*: its colour, its entropy, which row and
column it occupies, what an arrow key does to the cursor, and how a copied range
is rendered. The frontends differ only in how they put pixels or cells on screen.

Two pieces of that sharing are worth singling out, because they were the design's
real bet:

- **`RowGeo` takes its gutter and glyph width as parameters.** At
  `gutter = 0.0, char_w = 1.0` the arithmetic yields `cell_w = 3.0` (`"HH "`) and
  `hex_start = 10.0` — exactly a terminal's 8-digit address plus two spaces. So
  cell positions, 8-byte group gaps and the digits-only colouring are shared
  verbatim between a pixel canvas and a character grid.
- **The terminal reuses the RGBA thumbnail generators.** `build_overview_rgba` is
  asked for `h = rows * 2`, and `build_zoom_rgba` with `block = 1.0` already emits
  one pixel per byte. `tui::blit` then renders two pixel rows per text row as `▀`,
  foreground the upper pixel and background the lower. Alpha 0 becomes
  `Color::Reset`, which is how `Colormap::None` mutes a panel rather than blanking
  it — the same meaning transparent pixels carry on the gpui side.

## The canvas pattern

This is the most important thing to know before touching the rendering code.

gpui `canvas()` paint closures **cannot borrow the view**. So `gui::app::ui` clones
cheap snapshots into the closure — an `Arc<Mmap>`, the `Arc<Vec<f32>>` entropy
cache, the zoom, the anchor, the selection — and calls a matching free function
in `core::geom`. Everything in `core::geom` is therefore a pure function of its
arguments, which is exactly why it has real unit tests.

The four values a painter needs to colour a byte — the data, the entropy cache,
the entropy window and the colormap — travel together as one `ByteSource`. It
also owns the per-byte lookup (`color_at`, and `color_of` for the thumbnails,
which colour a sampled *average* while reading entropy at a cell's midpoint), so
no call site rebuilds it.

### A canvas has no intrinsic size

`Canvas::request_layout` refines `Style::default()` and has no children to
measure, so a canvas with no explicit size lays out **zero-height** and silently
paints nothing — or, worse, exactly one row, since the paint functions derive
their visible-row count from `bounds.size.height`. `.size_full()` must sit on
the **canvas**, not on the parent div:

```rust
.child(canvas(prepaint, paint).size_full())   // right
.child(canvas(prepaint, paint)).size_full()   // wrong: sizes the div; canvas stays 0-tall
```

The second form compiles and looks correct. It cost this project a working view
for a while. `view_height == 0` in the status bar's row range is the tell.
`ui::pane_canvas` now wraps this so call sites cannot get it wrong.

### Hit-testing runs off last-frame bounds

Each canvas's *prepaint* callback does `entity.update(...)` to store its
`Bounds<Pixels>` on the app (`hex_bounds`, `pixels_bounds`, `overview_bounds`,
`strip_bounds`, `scrollbar_bounds`, plus the slider bounds). Mouse handlers
convert window coordinates using those stored bounds. **If a pane renders but
doesn't respond to clicks, a missing or stale bounds write in prepaint is the
first suspect.**

Hit-testing must invert the paint formula exactly. Rows are painted from a
row-aligned start, so the inverse has to align the same way before flooring;
flooring first is off by one row.

## The shared scroll contract

All three columns share **one byte anchor** — `scroll_offset` in the gpui
frontend, `anchor` in the terminal one, both driving the same `core::geom`
helpers. They cannot share
a row number, because each column derives its own bytes-per-row from its own
measured width, so their rows do not line up.

- The anchor is the byte at each panel's vertical **centre**, not its top row
  (`core::geom::first_row_centred`). The columns show wildly different amounts of
  data — a zoom column showing ~16k bytes against a hex column showing ~840 — so
  anchoring at the top makes them drift apart downwards. Centring puts the same
  byte on the same line in all three. It saturates at 0 near the start of the
  file so the first rows stay reachable.
- **The hex column is the scroll reference.** It owns the visible height used to
  clamp the anchor, to size a page, and to centre a jump target. The anchor is
  clamped to `core::geom::max_anchor`, the start of the row holding the last byte *in
  the hex column's row length*. A column with longer rows runs out of file
  sooner; it simply paints what exists rather than scrolling independently.
- Scrolling is **vertical only** — every column's row length is chosen so its
  content fits the width.
- The wheel scrolls by whole rows *of the column under the pointer*, converted
  to bytes.
- Each column draws the *next* column's visible range as a band, so the overview
  shows where the zoom column is looking and the zoom column shows where hex is.

## The hex layout invariant

The hex column derives cell rectangles and hit-testing from `RowGeo`, and glyph
positions from character offsets in the row's text. **These two must resolve to
the same x for every byte**, or backgrounds drift away from the digits they
belong to. Two rules keep them in step:

- The row text is painted at `origin.x + ADDR_X` — the same gutter padding
  `RowGeo` builds into `hex_start`.
- `RowGeo` counts `(n-1)/8` group gaps in a row of `n` bytes, matching the text
  builder, which emits a space *between* groups only. Counting one too many put
  the ASCII block a full character right of its glyphs.

`hex_and_ascii_glyphs_sit_on_their_background_cells` asserts the identity for
every byte in a row and across group boundaries, for both the hex and ASCII
blocks. `RowGeo` is built from the *measured* monospace glyph width and is the
single source of truth for horizontal geometry — changing cell spacing means
changing `RowGeo` only.

## Caching and async work

Nothing that scales with file size runs on the UI thread.

- **Entropy** (`recompute_entropies_async`) runs on the background executor. An
  `entropy_gen` counter drops a stale result if the window changed mid-compute,
  and `entropy_computing` / `entropy_pending` coalesce requests so dragging the
  window slider cannot queue one whole-file pass per tick.
- **The overview thumbnail** and **the zoom column's visible-region texture**
  are each cached against a key (`OverviewKey`, `ZoomImageKey`) covering every
  input they are a function of. A key mismatch triggers one background rebuild;
  if the view moved during the build, the landing's stale key triggers another.
  Divider drags skip rebuilds and let the old texture scale until the drag ends.
- The zoom column uploads **one texture per changed visible region** rather than
  a quad per byte, which at `pixel_zoom = 1` was ~540k quads per frame.
- `paint_hex` merges cell backgrounds into runs of identical (colour, selection)
  state. Binary data is repetitive, so 2·bpr quads per row typically collapse to
  a handful. Runs split at every 8-byte boundary so a merged quad never covers
  the group gap that shows the panel background.
- Per-byte entropy lookups are gated on `Colormap::uses_entropy`, so the three
  colormaps that don't need entropy skip the interpolating lookup entirely.

The status bar's two timers are the feedback loop for all of this: `build` is
wall-clock inside `render` (element construction only), and `frame` runs from the
end of `render` to gpui's next frame callback, which covers the rest of the CPU
paint, the GPU submit/present and the refresh wait. gpui 0.2 exposes no
post-present hook, so `frame` is the closest available paint+GPU number.

## Persistence

`Render` compares `current_config()` against `saved_cfg` and writes at most every
2 seconds; `cx.on_release` and the Quit action also flush.

Window geometry is captured every frame by `capture_window_geometry`, which skips
maximized and fullscreen frames so the saved bounds stay the *un-maximize* size —
gpui's `WindowBounds::Maximized` treats the bounds it is given that way. Sizes are
rounded to whole pixels so an unchanged window doesn't keep rewriting the file.

## Platform notes

**The app draws its own window chrome on Linux.** Compositors need not implement
`xdg-decoration`, and GNOME's Mutter doesn't. With no decoration object to
negotiate with, gpui's `request_decorations` records the mode it was *asked* for
and tells nobody — so asking for `Server` (the default) leaves
`window_decorations()` reporting `Server` while nothing draws a titlebar, giving
a window that can't be moved or closed. `gui`'s `DECORATIONS` therefore asks
for `Client` on Linux so the state is honest, and `render` keys the titlebar drag
region, the window buttons and the eight resize handles off `Decorations::Client`.

`start_window_move` uses the last **mouse-press** serial, so it must be called
from `on_mouse_down` — an `on_click` handler is too late.

Accelerators are bound with `secondary-`, which gpui resolves to Cmd on macOS and
Ctrl elsewhere. Do **not** write `cmd-`: gpui parses that as the literal platform
modifier, i.e. Super on Linux/Windows, which is not what the UI labels promise.
Labels that name the modifier go through `cfg!(target_os = "macos")` consts such
as `JUMP_BUTTON_LABEL`.

## Making common changes

- **New keyboard action** — three places: the `actions!` list in `gui/mod.rs`, a
  `KeyBinding::new("secondary-…", …)` in `key_bindings`, and an
  `.on_action(cx.listener(Self::…))` on the root div in `Render`. Miss the last
  one and the key silently does nothing; miss the binding and only the mouse can
  reach it (which is deliberate for `ResetSettings` and `ClearSelection`).
- **New persisted preference** — a `core::config::Config` field, its `Default`, a
  `parse` match arm, a `serialize` line, `current_config()`, the clamp in
  `ParallHexApp::new` (and `TuiApp::new`), and the table in the README. `parse_round_trip` in
  `core/config.rs` covers the round trip.
- **New terminal key** — two places: a variant in `tui::app::Action` with its arm
  in `apply`, and the binding in `tui::input::key_to_action`. Both are pure, so
  add the test in the same commit; nothing needs a terminal.
- **Layout or painting change** — put the maths in `core::geom` as a free function
  and unit-test it; keep `gui::app::ui` and `tui::render` limited to wiring state
  into it. If the change is unit-aware, take the unit as a parameter (as `RowGeo`
  does with its gutter) rather than branching on the frontend.
- **Anything touching `core`** — check both frontends still build:
  `cargo test --all-targets` covers the default configuration, and
  `cargo test --no-default-features --features tui-frontend` the gpui-free one.

## Testing

The pure functions carry the coverage:

- The glyph/rect alignment identity above.
- `hex_bytes_per_row`, `zoom_bytes_per_row`: fit, snapping and minimum.
- Anchor → per-panel first row, including end-of-file clamping.
- Hit-testing: hex gaps and the gutter reject; the zoom column rejects positions
  past the last byte of a row and past end-of-file.
- Entropy: uniform data is 0, full-range is 8, block coverage, interpolation.
- Thumbnail pixel maths, including the `None` colormap leaving cells transparent
  and zero dimensions not panicking.
- Config round trip for every key, including each colormap value.
- CLI parsing, and that every keystroke string in `key_bindings` parses —
  `KeyBinding::new` panics on an unparseable keystroke, which would otherwise
  only surface as a crash on startup.
