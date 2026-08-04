# Architecture

How ParallHex is put together, and the handful of invariants that are easy to
break by accident. For what the app *does*, see [../README.md](../README.md).

## Module layout

Deliberately layered so the pixel and geometry maths is testable without opening
a window.

| Module | Responsibility |
|---|---|
| `src/main.rs` | CLI parsing (`parse_args` → `Cli`), the `actions!` list of every keyboard action, `key_bindings`, window creation, and `restored_bounds`. |
| `src/app.rs` | `ParallHexApp`: the single view entity holding *all* state, its event handlers, the async work, and a thin `Render` impl that delegates tree building to `app::ui`. |
| `src/app/ui.rs` | View construction: top bar, status bar, the three columns and their canvases, the jump dialog, shared chrome helpers. |
| `src/jump.rs` | `JumpField`, the jump dialog's text field. gpui 0.2 has no built-in text input, so the caret/selection/IME plumbing is ours: an `EntityInputHandler` plus a hand-written `Element`. |
| `src/panes.rs` | Pure painting and geometry — `paint_hex`, `paint_zoom`, `paint_overview`, `paint_strip`, `RowGeo`, `ByteSource`, the `build_*` thumbnail generators, hit-testing, zoom constants and clamps. |
| `src/color.rs` | `class_color`, `entropy_color`, the `Colormap` enum, `printable`, `human_size`. |
| `src/entropy.rs` | Shannon entropy. `block_entropies` computes one value per window-sized block, in parallel via rayon. |
| `src/config.rs` | Hand-rolled `key = value` preferences file, no serde dependency. |

Per-byte entropy is never stored — only one value per block. `panes::entropy_at`
interpolates between neighbouring blocks on demand.

## The canvas pattern

This is the most important thing to know before touching the rendering code.

gpui `canvas()` paint closures **cannot borrow the view**. So `ui.rs` clones
cheap snapshots into the closure — an `Arc<Mmap>`, the `Arc<Vec<f32>>` entropy
cache, the zoom, the anchor, the selection — and calls a matching free function
in `panes.rs`. Everything in `panes.rs` is therefore a pure function of its
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

All three columns share **one byte anchor**, `scroll_offset`. They cannot share
a row number, because each column derives its own bytes-per-row from its own
measured width, so their rows do not line up.

- The anchor is the byte at each panel's vertical **centre**, not its top row
  (`panes::first_row_centred`). The columns show wildly different amounts of
  data — a zoom column showing ~16k bytes against a hex column showing ~840 — so
  anchoring at the top makes them drift apart downwards. Centring puts the same
  byte on the same line in all three. It saturates at 0 near the start of the
  file so the first rows stay reachable.
- **The hex column is the scroll reference.** It owns the visible height used to
  clamp the anchor, to size a page, and to centre a jump target. The anchor is
  clamped to `panes::max_anchor`, the start of the row holding the last byte *in
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
a window that can't be moved or closed. `main.rs`'s `DECORATIONS` therefore asks
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

- **New keyboard action** — three places: the `actions!` list in `main.rs`, a
  `KeyBinding::new("secondary-…", …)` in `key_bindings`, and an
  `.on_action(cx.listener(Self::…))` on the root div in `Render`. Miss the last
  one and the key silently does nothing; miss the binding and only the mouse can
  reach it (which is deliberate for `ResetSettings` and `ClearSelection`).
- **New persisted preference** — a `config::Config` field, its `Default`, a
  `parse` match arm, a `serialize` line, `current_config()`, the clamp in
  `ParallHexApp::new`, and the table in the README. `parse_round_trip` in
  `config.rs` covers the round trip.
- **Layout or painting change** — put the maths in `panes.rs` as a free function
  and unit-test it; keep `ui.rs` limited to wiring state into it.

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
