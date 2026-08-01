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

Use a **wide** window (default inner size `[1600, 900]`, minimum `[1000, 600]`) and a 3-panel layout:

```
+-----------------------------------------------------------------------+
|  Top Panel: Menu Bar & Controls (Open File, Bytes/Row, Entropy Win)   |
+-----------------------------------+-----------------------------------+
|                                   |  Side Panel (Right):              |
|  Central Panel (wide):            |  - File information               |
|  ┌─────────────────────────────┐  |  - Hover / Inspector stats       |
|  │  Hex + ASCII strip (top)    │  |  - Selection range actions       |
|  │  (class-colored backgrounds)│  |  - Copy Hex / Copy ASCII         |
|  ├─────────────────────────────┤  |                                   |
|  │  Direct greyscale map       │  |                                   |
|  ├─────────────────────────────┤  |                                   |
|  │  Entropy map                │  |                                   |
|  └─────────────────────────────┘  |                                   |
|  (one shared virtualized scroll)  |                                   |
+-----------------------------------+-----------------------------------+
|  Bottom Panel: Status Bar (Offset, Byte, Entropy under cursor)       |
+-----------------------------------------------------------------------+
```

### Central Panel — Four Synchronized Panes

1. **One shared `ScrollArea`** contains all three stacked panes (hex+ASCII strip, greyscale map, entropy map). They share a single vertical scrollbar and the same horizontal scale (`bytes_per_row` cells per row), so a given offset appears in the same column in every pane. This is the "synchronized scroll" mechanism — no manual offset syncing is required because all panes render from the same row index.

2. **Hex + ASCII strip (top pane):**
   * Rows of `bytes_per_row` cells. Each cell shows `"%02X"` in a monospace font, **background filled with the Class palette color** (§3.C.1), high-contrast foreground text.
   * An ASCII column follows the hex cells on each row, showing `printable(b)` per byte (`'.'` for non-printable), same class-colored backgrounds.
   * Row headers show the starting offset `r*B` in `%08X`.
   * Selection range is highlighted (semi-transparent overlay across the selected cells); hovered cell gets a bright outline.

3. **Direct greyscale map (middle pane):**
   * Renders the same row range as a pixel strip: one `Color32::from_gray(byte)` pixel per byte, with each row's pixels scaled to the same column width as the hex cells so columns line up.

4. **Entropy map (bottom pane):**
   * Same geometry; each pixel colored by the sliding-window entropy at that offset (§3.B), using the entropy gradient (§3.C.3).

5. **Selection & hover (shared):**
   * Hover updates `hovered_offset`; the same offset is highlighted in all panes (hex cell outline, greyscale/entropy pixel marker) and reported in the bottom status bar.
   * Primary click + drag on the hex strip sets `selection_range`; all panes highlight the selected byte range.
   * Clicking selects a single byte (`selected_offset`).

### Top Panel Controls

* **Open File…** (and Ctrl/Cmd+O) — `rfd` native dialog.
* **Bytes/Row** combo: `16 / 32 / 64` (default 32; wider rows fill the wide window).
* **Entropy window** slider: `16..=4096`, logarithmic (default 256).
* **Reset view** — jump scroll back to row 0.

### Side Panel (Right)

* File name, size (`human_size`).
* Inspector: hovered and selected byte — offset (`0x%08X`), value (`0x%02X`), printable char, and local entropy `H`.
* Selection section: range `0x…–0x…`, length, **Copy Hex**, **Copy ASCII**, **Clear**.
* A **mini overview map** is optional (whole-file entropy/greyscale thumbnail for navigation).

### Bottom Status Bar

`Offset: 0x… Byte: 0x… 'c' H=…` under cursor · file size · `Rows: R · Bytes/row: B`.

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
   * Add the top control panel (Open File, Bytes/Row, Entropy window, Reset view).
   * Add the right inspector panel and bottom status bar.
5. **Implement the Central Panel**:
   * One virtualized `ScrollArea` computing `first_row..last_row` from `scroll_row`.
   * Render the hex+ASCII strip with class-colored cell backgrounds and selection/hover overlays.
   * Render the greyscale and entropy panes beneath it at the same column scale.
   * Wire hover/click/drag so all four panes update the shared hover/selection state.
6. **Wire Data Loading**: memory-map the file on Open; reset scroll/selection; compute entropy blocks in parallel (recompute only when the file or entropy window changes).
7. **Polish**: contrast-aware hex text, hover outlines, selection copy actions, status bar readout.
