# Performance

This documents the current render cost, where the time goes, and the
optimizations that have landed. It is a living report: keep it in sync with the
code as further changes land.

## Where the time goes

The status bar shows two live timers taken in the `Render` impl:

- **`build`** — wall-clock inside `render` (app.rs) while the element tree is
  constructed. This is a few milliseconds and *not* where the cost is.
- **`frame`** — from the end of `render` until gpui's next frame callback
  (`Window::on_next_frame`). This spans the rest of the CPU paint, the GPU
  submit/present and the refresh wait. gpui 0.2 exposes no post-present hook, so
  this is the closest we can get to a paint+GPU number.

The remaining per-frame work is:

- **`panes::paint_zoom`** — one texture upload per frame (when the visible
  region changed) plus a handful of overlay quads (selection, hover, the hex
  column's mark band). The per-byte quads are gone (see landed item 1).
- **`panes::paint_hex`** — merged background/selection runs (one quad per
  *run* of identical (color, selection) state rather than per byte), the text
  runs, and the hover outline. Each byte's color is computed once and reused
  for both cells and both glyphs.

App-side, `load_file` now computes the whole-file entropy pass on the
background executor; the UI thread only memory-maps the file and paints the
bytes immediately.

## Landed optimizations

### 1. Zoom column renders a visible-region texture ✅

`panes::paint_zoom` no longer emits one quad per byte (the old worst case was
~540k quads/frame at `pixel_zoom = 1`). Instead `measure_zoom` (the zoom
canvas's prepaint) builds the visible bytes into a single RGBA buffer via
`panes::build_zoom_rgba` / `build_zoom_image` — a rayon-parallel fill of one
`block × block`-ish pixel square per byte, quantized to the integer pixel grid —
and caches the resulting `RenderImage` on the app. The cache key
(`ZoomImageKey`) covers row length, visible start, texture size, colormap and
entropy window, so scrolling/zooming re-uploads a texture instead of repainting
hundreds of thousands of quads. Selection and hover stay overlay quads, and the
texture is invalidated when entropies land or a new file loads. The pixel math
is unit-tested (`build_zoom_rgba`), like the overview's.

### 2. RLE-merged background quads in the hex column ✅

`paint_hex` walks each row and extends a single quad while the byte color and
selection state are unchanged, emitting one quad per run rather than one per
byte (`panes::paint_cell_runs`). Binary data is repetitive (zero / `0xFF` runs,
printable blocks), so 2·bpr quads per row typically collapse to a handful. Hex
runs split at every 8-byte group boundary so a merged quad never covers the
group gap (which shows the panel background). Each byte's color is computed
**once** (gated on the colormap, see item 3) and reused for the hex cell, the
ASCII cell and both glyphs — previously it was computed four times per byte.

### 3. Entropy lookups gated on the colormap ✅

`Colormap::uses_entropy()` returns true only for `Entropy`, and every per-byte
paint path routes its lookup through `panes::entropy_for`, so the interpolating
`entropy_at` call is skipped for `none`, `value` and `class` (the default hex
colormap no longer pays ~40k+/frame of skipped work).

### 4. Load-time work moved off the UI thread ✅

`recompute_entropies_async` spawns a background task holding an `Arc<Mmap>`
snapshot (`cx.spawn` + the background executor), shows a "computing entropy…"
status on load, and applies the result — invalidating the overview, strip and
zoom texture — when it lands. A generation counter (`entropy_gen`) drops stale
results if the entropy window changed mid-compute. The entropy-window slider and
"Reset all settings" use the same path. The UI never blocks on the whole-file
entropy pass for multi-gigabyte files.

### 5. Hygiene ✅

- One `Font` per frame (the hex column previously built two identical ones).
- `hex_char_w` is measured once per frame in `render` and shared by the hex
  canvas prepaint, the paint closure and hit-testing — previously reshaped 64
  glyphs on every canvas paint *and* on every mouse move.
- `build_row_text_into` / `build_row_runs` reuse caller-owned buffers across
  the rows of a frame instead of allocating per row.
- Optional later: set `TextRun.background_color` and drop the hex-cell bg quads
  entirely (run-boundary caveats apply; the RLE approach above landed first).

## Validation

After each landed step, compare the status-bar `frame` timer at
`pixel_zoom = 1` on a large binary; the primary target was the zoom column's
quad count dropping by three to four orders of magnitude, now replaced by a
single texture upload when the visible region changes.
