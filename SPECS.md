Here is a complete, production-ready specification designed to be fed directly into an AI coding agent (e.g., Claude Code, Cursor, Aider) to build a native `binvis.io`-inspired binary viewer in Rust using `egui`.

---

# Architecture Specification: `binvis-rs` — Wide Hex-Viewer Edition

## 1. Project Overview & Dependencies

Build a cross-platform desktop application in Rust using `eframe`/`egui` for interactive exploration of raw binary files. The app presents **one wide window** styled like a hex viewer: the file is laid out as a linear (row-major) sequence of rows, and four **synchronized panes** render the same byte window in four different ways:

1. **Hex view** — classic hex dump. Each byte cell is drawn with its **`Class` color-mode palette as the background** (see §3.C) and a high-contrast foreground for the two hex digits.
2. **ASCII view** — printable representation of the same bytes (`'.'` for non-printable), rendered inline with the hex cells per row.
3. **Direct greyscale** — one pixel per byte, `Color32::from_gray(byte)` (byte value → brightness).
4. **Entropy** — one pixel per byte, colored by the Shannon entropy of the sliding window centered on that byte (see §3.B).

The **Hilbert curve layout is explicitly removed**: only linear scan data is displayed. There is no `LayoutMode` enum; every view is row-major with a configurable bytes-per-row width.

### `Cargo.toml`

```toml
[package]
name = "binvis-rs"
version = "0.1.0"
edition = "2021"

[dependencies]
eframe = "0.28" # Or latest stable
egui = "0.28"
egui_extras = "0.28"
memmap2 = "0.9"
rfd = "0.14" # Native file dialogs
rayon = "1.10" # Parallel processing for entropy computation
```

---

## 2. Application State & Data Architecture

```rust
pub struct AppState {
    pub file_path: Option<PathBuf>,
    pub mmap: Option<Arc<Mmap>>,
    pub file_size: usize,

    // Hex-viewer parameters
    pub bytes_per_row: usize,   // Bytes per row: 16, 32, or 64 (fills the wide window)
    pub entropy_window: usize,  // Sliding entropy window size (e.g., 256)

    // Shared scroll & selection state (drives all four panes)
    pub scroll_row: f32,                    // Vertical scroll position, in rows
    pub hovered_offset: Option<usize>,
    pub selected_offset: Option<usize>,
    pub selection_range: Option<Range<usize>>,
    pub drag_start: Option<usize>,
}
```

Notes:

- **No `LayoutMode`.** Hilbert is gone; data is always displayed linearly, `bytes_per_row` bytes per row.
- There is no color-mode switcher: the four panes are shown simultaneously and each has a fixed mapping (hex = class palette backgrounds, greyscale = byte value, entropy = entropy gradient).
- `scroll_row` is expressed in *rows* so that the hex/ASCII strip, greyscale map, and entropy map always show the same byte range (they are scrolled together by one shared virtualized scroll area — see §4).

---

## 3. Core Algorithms & Math Specifications

### A. Linear Row Layout

Given a file of `L` bytes and `B = bytes_per_row` bytes per row:

- Total rows: `R = ceil(L / B)`.
- Byte index `d` lives in row `row = d / B`, column `col = d % B`.
- Row `r` spans offsets `[r*B, min((r+1)*B, L))`.

This mapping is used identically by all four panes, which is what keeps them synchronized.

### B. Shannon Entropy Calculation

For a byte window of length `W` (e.g., `W = 256`):

$$H = -\sum_{i=0}^{255} p_i \log_2(p_i) \quad \text{where } p_i = \frac{\text{count}(i)}{W}$$

Standardized output range: `[0.0, 8.0]` bits per byte.

**Per-pixel entropy (sliding window):** for byte at offset `d`, use the entropy of the `W`-byte block that contains `d` — i.e., block `floor(d / W)` over `[floor(d/W)*W, floor(d/W)*W + W)`, clamped at file boundaries (matching the classic hex-viewer convention where each block is labeled by its first byte). To keep this fast for large files:

- Compute block entropies over contiguous `W`-byte blocks in parallel with `rayon`, then index per pixel (`entropies[d / W]`).
- Optionally smooth between adjacent blocks by linearly interpolating the two block entropies around `d`; this is a visual nicety, not a requirement.

### C. Color Mapping Rules

1. **Hex/ASCII view — Class palette (cell backgrounds):**
   Each byte cell's **background** is colored by byte class:

   * `0x00` (Null): `#000000` (Black)
   * `0x20..=0x7E` (Printable ASCII): `#1f77b4` (Blue)
   * `0x01..=0x1F`, `0x7F` (Control characters): `#17becf` (Cyan)
   * `0x80..=0xFE` (High/Non-ASCII): `#ff7f0e` (Orange)
   * `0xFF` (Fill/Padded): `#ffffff` (White)

   Foreground text is chosen for contrast against the class background (white text on dark cells, black text on `0xFF` white cells), so hex digits and ASCII glyphs stay legible.

2. **Greyscale view — direct byte value:**
   * `Color32::from_gray(byte)` — byte value maps linearly to brightness (`0x00` → black, `0xFF` → white).

3. **Entropy view — gradient:**
   Map entropy `H ∈ [0.0, 8.0]` to a gradient:
   * `0.0` → Deep Purple/Black (low entropy / uniform)
   * `4.0` → Green/Cyan (structured data / text)
   * `8.0` → Bright Red/Yellow (high entropy / compressed / encrypted)

---

## 4. UI Layout Specs (`egui`)

Use a **wide** window (default inner size `[1600, 900]`, minimum `[1000, 600]`) and a three-column layout, with all info in the top bar:

```
+-----------------------------------------------------------------------+
|  Top Bar: Title · File name/size · Hovered/Selected byte · Controls   |
|  (Open File, Bytes/Row, Entropy Win, Reset, Jump, zoom readout)       |
+------------------------+------------------------+---------------------+
|  Overview column       |  Pixels column         |  Hex column         |
|  (left, resizable)     |  (middle, resizable)   |  (right, central)   |
|  whole-file thumbnail  |  per-byte greyscale +  |  class-colored hex  |
|  (greyscale / entropy) |  entropy bands         |  + ASCII cells      |
|  + viewport band       |  drag to pan,          |  drag to select,    |
|  click/drag navigates  |  Ctrl+wheel zoom       |  Ctrl+wheel zoom    |
+------------------------+------------------------+---------------------+
```

Each column has a header showing its **visible byte range** (e.g. `0x00000000 – 0x000000FF`; the overview column always shows the whole-file range) and — for the zoomable pixels/hex columns — a live zoom readout (`×1.00`, `4 px`) with a **Reset zoom** button that restores that column's default zoom.

### Three Synchronized Columns

All three columns share one scroll position (`scroll_rows`, in rows). The **hex column is the master** (it owns the scrollbar); the pixels and overview columns follow it, and dragging any column pans it: primary drag pans the pixels and overview columns, while the hex column pans with a **middle-mouse drag** or a **Ctrl/Alt + primary drag** (its plain primary drag selects bytes instead). A `Ctrl+wheel` / pinch over a column adjusts that column's zoom (hex row height ×0.5–4, pixel size 1–24 px).

1. **Hex column (right, central panel):**
   * Rows of `bytes_per_row` cells. Each cell shows `"%02X"` in a monospace font, **background filled with the Class palette color** (§3.C.1), high-contrast foreground text.
   * An ASCII column follows the hex cells on each row, showing `printable(b)` per byte (`'.'` for non-printable), same class-colored backgrounds.
   * Row headers show the starting offset `r*B` in `%08X`.
   * Selection range is highlighted (semi-transparent overlay across the selected cells); hovered cell gets a bright outline.
   * Primary click + drag sets `selection_range`; right-click offers **Copy Hex / Copy ASCII / Clear selection**. Middle-mouse or Ctrl/Alt + primary drag pans the column (the same shared-scroll pan gesture as the pixels column).

2. **Pixels column (middle):**
   * One `Color32::from_gray(byte)` pixel per byte on the top half of each row, one entropy-colored pixel (sliding-window entropy §3.B, gradient §3.C.3) on the bottom half.
   * Renders only the visible rows for `scroll_rows`; drag pans the shared scroll (all columns follow), click selects a byte, hover outlines the byte.

3. **Overview column (left):**
   * Whole-file 2-row thumbnail (greyscale on top, entropy below) with a translucent band marking the currently visible range.
   * Hover previews the offset under the cursor in the top bar; click / drag jumps the view (centered, selects/hovers the byte).

4. **Keyboard navigation:** arrow keys move the selection by one byte / one row; PageUp / PageDown move by a page (visible rows); Home / End jump to the file start / end. The view auto-scrolls to keep the selection centered. Page size scales with the hex zoom.

5. **Jump to offset (Ctrl/Cmd+G):** a centered dialog accepts a hex offset (`0x…` prefix optional, underscores allowed), prefilled with the current selection; Enter or **Jump** navigates to that byte (scrolls, selects, and hovers it), with a live preview of the parsed offset in the top bar. Out-of-range or invalid input shows an error and keeps the dialog open. Also reachable via the **Jump to offset… (Ctrl+G)** button in the top panel.

### Command Line

* **`entropymap <file>`** — opens the file on startup (optional positional argument; errors are shown in the status bar).
* **`entropymap --help`** (or `-h`) — prints usage and exits. Unknown `-`-prefixed options print an error and exit instead of being treated as files. `--` ends option parsing, so a file whose name starts with `-` can be opened (e.g. `entropymap -- -foo.bin`).

### Top Bar (Title + Info + Controls)

* **Title** `EntropyMap`, **file name** and size (`human_size`).
* **Hovered / selected byte** readout: `0x… · 0x… 'c' · H=…` (live; the overview hover preview takes precedence while hovering it).
* **Open File…** (and Ctrl/Cmd+O) — `rfd` native dialog.
* **Bytes/Row** combo: `16 / 32 / 64` (default 32; wider rows fill the wide window).
* **Entropy window** slider: `16..=4096`, logarithmic (default 256).
* **Reset view** — jump scroll back to row 0.
* **Jump to offset… (Ctrl+G)** — open the jump-to-offset dialog (live preview while typing).
* **Zoom readout** — `hex ×… · px …` (Ctrl+wheel over the hex/pixels columns to zoom); each zoomable column also has its own **Reset zoom** button in its header.
* Error messages (yellow) are shown here too.

There is **no side panel and no bottom status bar**: file info, hovered/selected byte readout, zoom state and error messages all live in the top bar. Selection copy actions are available from the hex column's right-click context menu (Copy Hex / Copy ASCII / Clear selection).

---

## 5. Parallel Pipeline Requirements

* Use `memmap2::Mmap` for zero-copy mapping of multi-gigabyte files.
* Use `rayon` to compute block entropies in parallel (`par_chunks` over `W`-byte blocks).
* **Virtualized rendering:** rows are only drawn when inside the visible viewport (compute `first_row..last_row` from the shared scroll offset, exactly like a virtual list). This keeps memory flat and scrolling smooth for arbitrarily large files — no full-file texture is generated.
* Greyscale and entropy pixels for visible rows can be produced in parallel with `rayon::par_iter` over the visible row range.

---

## Step-by-Step Implementation Instructions for Agent

1. **Scaffold Project**: Create `Cargo.toml` with `eframe`, `egui`, `egui_extras`, `memmap2`, `rfd`, and `rayon`. Remove any Hilbert-related files; the project is linear-only.
2. **Implement Data Models**: Define `AppState` (with `bytes_per_row`, `entropy_window`, shared `scroll_row`, selection state). **Do not** define a `LayoutMode`; there is no Hilbert mode.
3. **Implement Math Utilities**:
   * Write the Shannon entropy block function (§3.B).
   * Write the byte-to-class-color mapping for hex backgrounds (§3.C.1).
   * Write the entropy gradient mapping (§3.C.3).
   * Write `printable(b)` for the ASCII view.
4. **Build UI Framework**:
   * Set up `eframe::App` shell with a wide default viewport (`1600×900`).
   * Add the top bar with title, file info, hovered/selected byte readout, and controls (Open File, Bytes/Row, Entropy window, Reset view, Jump).
5. **Implement the Three Columns**:
   * Hex column: one virtualized `ScrollArea` computing `first_row..last_row` from the shared `scroll_rows`; class-colored hex+ASCII cells with selection/hover overlays; master scrollbar; drag to select; right-click context menu (Copy Hex / Copy ASCII / Clear); Ctrl+wheel zoom.
   * Pixels column: per-byte greyscale + entropy bands for the visible rows; drag to pan (writes `scroll_rows`), wheel scroll, Ctrl+wheel zoom, click selects.
   * Overview column (left): whole-file greyscale/entropy thumbnail with a viewport band; click/drag navigates.
   * Wire hover/click/drag so all three columns update the shared hover/selection/scroll state.
6. **Wire Data Loading**: memory-map the file on Open; reset scroll/selection; compute entropy blocks in parallel (recompute only when the file or entropy window changes).
7. **Polish**: contrast-aware hex text, hover outlines, selection copy via context menu, top-bar readout.
