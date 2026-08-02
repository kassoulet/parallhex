# Architecture Specification: `parallhex` — Three-Column Binary Explorer

A native `binvis.io`-inspired binary viewer in Rust using `gpui`. The file is
memory-mapped and presented in **one wide window** as three columns that show
the same region of the file at three levels of detail: a whole-file
**overview**, a zoomable per-byte **zoom view**, and a **hex/ASCII** dump.

Each column derives its own row length from its own width, so nothing ever
scrolls horizontally, and each column can be colored independently.

## 1. Dependencies

```toml
[package]
name = "parallhex"
version = "0.1.0"
edition = "2024"

[dependencies]
gpui = "0.2.2"         # UI framework (retained-mode, GPU-accelerated)
image = "0.25"         # RGBA frames for the overview / strip thumbnails
memmap2 = "0.9"        # Zero-copy mapping of multi-gigabyte files
rfd = "0.14"           # Native file dialogs
rayon = "1.10"         # Parallel entropy computation
```

There is no Hilbert-curve layout: every view is row-major.

## 2. Application State

```rust
pub struct AppState {
    pub file_path: Option<PathBuf>,
    pub mmap: Option<Arc<Mmap>>,
    pub file_size: usize,

    // Shared byte anchor: the offset the panels are scrolled to (§4.2).
    pub scroll_offset: usize,

    // Per-panel appearance.
    pub overview_colormap: Colormap,
    pub zoom_colormap: Colormap,
    pub hex_colormap: Colormap,
    pub pixel_zoom: f32,        // zoom view only, 1..=24 px per byte
    pub entropy_window: usize,  // 16..=4096, default 256

    // Shared selection state, always in byte offsets.
    pub hovered_offset: Option<usize>,
    pub selected_offset: Option<usize>,
    pub selection_range: Option<Range<usize>>,
}
```

The snippet covers the model only; the view also holds layout state (the two
resizable column widths, per-panel canvas bounds for hit-testing, window
geometry) and transient interaction state (drag/menu/dialog flags).

Notes:

- **No `bytes_per_row` field.** Each panel computes its own row length from its
  own width every frame (§3.A). Nothing is persisted about it.
- **No `hex_zoom` field.** The hex text size is fixed; only the zoom view zooms.
- The scroll position is a **byte offset**, not a row index, because the panels
  no longer share a row length.

## 3. Core Algorithms

### A. Row length per panel

Every panel lays bytes out row-major, but each computes its own row length from
its own width. All three are pure functions and unit-tested.

**Hex/ASCII column.** A row of `n` bytes occupies, in monospace glyph widths
`char_w` plus the gutter padding `ADDR_X`:

```
width_for(n) = ADDR_X + char_w * (10 + 3n + (n-1)/8 + 2 + n)
                                  ^^   ^^^  ^^^^^^^   ^   ^
                                  |    |    |         |   ASCII glyphs
                                  |    |    |         gap before ASCII
                                  |    |    extra space after every 8 bytes
                                  |    "HH " per byte
                                  "%08X" + two spaces
```

`hex_bytes_per_row` returns the largest multiple of 8 with
`width_for(n) <= panel_width`, floored at 8.

**Zoom view.** `zoom_bytes_per_row = max(1, floor(panel_width / pixel_zoom))`.

**Overview.** The whole file is fitted to the panel as a `w × h` cell grid
(`w`, `h` clamped to the panel size); cell `k` covers
`[k*len/cells, (k+1)*len/cells)`.

### B. Shannon entropy

For a window of `W` bytes:

$$H = -\sum_{i=0}^{255} p_i \log_2(p_i) \quad p_i = \frac{\text{count}(i)}{W}$$

Output range `[0.0, 8.0]` bits per byte. Entropy is computed once per file over
contiguous `W`-byte blocks, in parallel with `rayon`, and cached. Per-byte
entropy is not stored: it is interpolated between the two blocks around the
offset on demand.

### C. Color mapping

A single `Colormap` enum is selected independently per panel:

| Mode | Meaning |
|---|---|
| `None` | No per-byte color at all. |
| `Value` | Byte value → greyscale brightness (`0x00` black, `0xFF` white). |
| `Class` | Byte class palette (below). |
| `Entropy` | Sliding-window entropy → gradient (below). |

**Class palette:** `0x00` black · `0x01..=0x1F`, `0x7F` cyan `#17becf` ·
`0x20..=0x7E` blue `#1f77b4` · `0x80..=0xFE` orange `#ff7f0e` · `0xFF` white.

**Entropy gradient:** `0.0` deep purple → `4.0` green/cyan → `8.0` red/yellow.

In the hex/ASCII column the mode colors each cell's **background**, and the
glyph color is chosen for contrast against it (light text on dark cells, dark
text on light ones). Under `None` no background is drawn and glyphs use the
default foreground.

`None` is a legitimate choice for the pixel panels too, and it renders them
empty: the overview, the zoom view and the top-bar strip paint only their panel
background. They stay interactive — the viewport band, hover preview and
click-to-navigate all still work — so `None` reads as "mute this panel" rather
than "disable it". Selection and hover highlights are drawn in every mode.

## 4. UI Layout

Wide window, default `1600×900`, minimum `1000×600`.

```
+-----------------------------------------------------------------------+
|  ParallHex · file/size · [whole-file strip] · – □ ✕                    |
|  (Open File…, Entropy win, Reset view, Jump, Reset columns/all)        |
+------------------+---------------------+------------------------------+
|  Overview        |  Zoom view          |  Hex / ASCII                 |
|  Map: Entropy ▾  |  Map: Value ▾       |  Map: Class ▾                |
|  whole file      |  1 px  [──●──] Reset|  0x0000 – 0x047F             |
|                  |                     |                              |
|  one cell per    |  one band per byte  |  ADDR  HH HH … · ASCII       |
|  N bytes,        |  at pixel_zoom,     |  fixed text size, rows       |
|  viewport band,  |  rows flush,        |  fit the width, selection    |
|  click/drag      |  drag to pan        |  + hover highlight           |
|  navigates       |                     |                              |
+------------------+---------------------+------------------------------+
|  Status: offset · byte · H=… · selection · zoom · rows · messages      |
+-----------------------------------------------------------------------+
```

Every column header shows the panel's **visible byte range** (the overview
always shows the whole file) and a **`Map: … ▾` dropdown** selecting that
panel's colormap. Only the zoom view's header carries zoom controls: a `N px`
readout, a slider, and **Reset**.

Default colormaps: overview `Entropy`, zoom view `Value`, hex `Class`.

### 4.1 Columns

1. **Overview (left, resizable).** The whole file downsampled to one band per
   cell in its own colormap, regenerated on load, on colormap change and on
   resize. A translucent band marks the visible region. Hover previews the
   offset under the cursor; click and drag navigate.

2. **Zoom view (middle, resizable).** **One** band per byte — a single row of
   `pixel_zoom`-sized blocks, rows flush with no separator, so the panel reads
   as a true pixel image. `Ctrl+wheel`, `+`/`-` and the header slider set the
   zoom (1–24 px), which also changes how many bytes fit per row. Drag pans the
   shared anchor; click selects a byte; hover outlines it. Positions right of
   the last byte in a row resolve to no byte.

3. **Hex/ASCII (right, fills the remaining width).** Rows of `n` byte cells
   (§3.A) at a fixed text size: `%08X` address gutter, `HH` per byte with an
   extra space every 8, then the ASCII block (`.` for non-printable). Cell
   backgrounds follow the panel's colormap. The selection range is tinted and
   the hovered cell outlined. Primary drag selects; middle-drag or
   Ctrl/Alt+primary drag pans; right-click copies the selection as hex and
   Alt+right-click clears it.

### 4.2 Scroll model

All three columns share one **byte anchor**, `scroll_offset`. Each panel paints
from its own row-aligned start, `scroll_offset - (scroll_offset % bpr_panel)`,
so the panels stay anchored to the same region of the file even though their
rows do not line up.

- **Vertical only.** No panel ever scrolls horizontally; §3.A guarantees rows
  fit.
- The wheel scrolls by whole rows *of the panel under the pointer*, converted to
  bytes.
- The **hex column is the scroll reference**: it owns the visible height used to
  clamp the anchor, to size a page, and to center a jump target.
- The anchor is clamped to `[0, last_hex_row_start]`, where `last_hex_row_start`
  is the start of the row containing the final byte *in the hex column's* row
  length. A panel whose rows are longer runs out of file sooner; it simply paints
  what exists and stops rather than scrolling independently.

### 4.3 Keyboard

Arrow keys move the selection by one byte / one hex row; PageUp/PageDown by one
hex page; Home/End jump to the first/last byte. The view auto-scrolls to keep
the selection centered. `+`/`-` zoom the zoom view when the pointer is over it.
**Jump to offset (Ctrl/Cmd+G)** opens a centered dialog accepting a hex offset
(`0x` optional, underscores allowed), prefilled with the current selection, with
a live preview and an inline error for out-of-range input.

Accelerators use gpui's portable `secondary-` modifier: **Cmd on macOS, Ctrl
elsewhere.** Open `secondary-o`, Quit `secondary-q`, Jump `secondary-g`, Reset
view `secondary-0`, Reset columns `shift-secondary-l`, Copy hex `secondary-c`,
Copy ASCII `shift-secondary-c`.

### 4.4 Command line

- `parallhex <file>` — open a file on startup; errors appear in the status bar.
- `parallhex --help` / `-h` — print usage and exit. Unknown `-`-prefixed options
  are rejected; `--` ends option parsing so dash-prefixed names can be opened.

### 4.5 Top bar and window chrome

The top bar holds the title, file name and size, the horizontal whole-file
**strip** (256×1, following the overview's colormap; hover previews an offset,
click/drag navigates), the window buttons, and the controls: Open File…, the
logarithmic **entropy-window** slider (16–4096), Reset view, Jump to offset…,
Reset columns, Reset all settings.

Linux compositors need not implement `xdg-decoration` and GNOME's Mutter does
not, so the app requests **client-side decorations** on Linux and supplies its
own chrome: the title/file-name area is the drag handle (double-click maximizes,
right-click opens the window menu), the window buttons minimize / maximize /
close, and an invisible 6 px border along the edges and corners starts a
compositor resize. All of it is gated on `Decorations::Client`, so macOS and
Windows keep native titlebars.

### 4.6 Preferences

A small `key = value` file in the platform config directory
(`$XDG_CONFIG_HOME`, `$APPDATA`, `~/Library/Application Support`, else
`~/.config/parallhex/config.txt`), written a couple of seconds after the last
change and on exit:

| Key | Meaning |
|---|---|
| `entropy_window` | 16–4096 |
| `pixel_zoom` | zoom view zoom, 1–24 |
| `overview_colormap`, `zoom_colormap`, `hex_colormap` | `none` / `value` / `class` / `entropy` |
| `overview_width`, `zoom_width` | resizable column widths |
| `window_x`, `window_y`, `window_width`, `window_height` | last geometry |
| `window_maximized` | restore maximized |

Loading is tolerant: unknown keys, malformed lines and non-finite values are
ignored and out-of-range values are clamped. **Retired keys** —
`bytes_per_row`, `hex_zoom`, `pixel_colormap`, `pixels_width` — are simply
unknown keys to the new parser, so older config files load without error and are
rewritten in the new form.

## 5. Rendering Pipeline

- `memmap2::Mmap` for zero-copy mapping; the file is never read into memory.
- `rayon` computes block entropies in parallel, once per file / window size.
- **Virtualized:** every panel paints only the rows intersecting its viewport,
  computed from the shared anchor, so memory and frame cost stay flat for
  arbitrarily large files. No full-file texture is ever built; the overview and
  strip are small downsampled thumbnails.
- gpui canvases have no intrinsic size and must be sized explicitly
  (`canvas(..).size_full()`), or they lay out zero-height and paint nothing.

## 6. Layout Invariant: glyphs and cells must agree

The hex column derives cell rectangles and hit-testing from `RowGeo`, and glyph
positions from the character offsets in the row's text. **These two must resolve
to the same x for every byte**, or backgrounds drift away from the digits they
belong to. Two rules keep them in step:

- The row text is painted at `origin.x + ADDR_X` — the same gutter padding
  `RowGeo` builds into `hex_start`.
- `RowGeo` counts `(n-1)/8` group gaps in a row of `n` bytes, matching the text
  builder, which emits a space *between* groups only.

A regression test asserts, for every byte in a row and across group boundaries,
that the glyph x derived from the text offsets equals the rect x from `RowGeo`,
for both the hex and the ASCII block.

## 7. Testing

The pixel and geometry math lives in pure functions so it is testable without a
window:

- The §6 glyph/rect alignment identity.
- `hex_bytes_per_row`, `zoom_bytes_per_row`: fit, snapping, and minimum.
- Anchor → per-panel first row, including end-of-file clamping.
- Hit-testing: hex gaps and the gutter reject; the zoom view rejects positions
  past the last byte of a row and past end-of-file.
- Entropy: uniform data is 0, full-range is 8, block coverage, interpolation.
- Config round-trip for all keys including every colormap value and `none`.
