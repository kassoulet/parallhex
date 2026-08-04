//! Pure geometry and byte-addressing maths: how many bytes fit a row, where a
//! row starts for a given anchor, where a byte sits horizontally, and which byte
//! a coordinate refers to.
//!
//! Every function here is a pure function of its arguments and names no UI
//! toolkit, which is what lets both frontends share it and what makes it all
//! testable without opening a window or a terminal.

use std::fmt::Write as _;

use super::color::{self, Colormap, Rgb};

pub(crate) const PIXEL_ZOOM_DEFAULT: f32 = 4.0;

pub(crate) const PIXEL_ZOOM_MIN: f32 = 1.0;

pub(crate) const PIXEL_ZOOM_MAX: f32 = 24.0;

/// Clamp range for the entropy window, in bytes. Shared by the load-time clamp,
/// the slider's pointer→value mapping and its thumb position: three copies of
/// these bounds could disagree, and a thumb that renders somewhere a drag won't
/// put it is the visible symptom.
pub(crate) const ENTROPY_WINDOW_MIN: usize = 16;

pub(crate) const ENTROPY_WINDOW_MAX: usize = 4096;

/// Left padding for the address gutter.
pub(crate) const ADDR_X: f32 = 8.0;

/// Width a hex row of `n` bytes needs: the address gutter, `"HH "` per byte, a
/// space between 8-byte groups, the two-space gap, then one ASCII glyph per
/// byte. Must mirror `build_row_text` exactly: cell rects come from here and
/// glyph positions from the row text's character offsets, so the two have to
/// resolve to the same x for every byte or backgrounds drift off the digits they
/// belong to (`hex_and_ascii_glyphs_sit_on_their_background_cells` asserts it).
fn hex_row_width(n: usize, char_w: f32) -> f32 {
    let chars = 12 + 4 * n + n.saturating_sub(1) / 8;
    ADDR_X + char_w * chars as f32
}

/// Bytes per row for a hex panel `panel_width` wide: the largest multiple of 8
/// that fits, floored at 8 so the 8-byte grouping is never split.
pub(crate) fn hex_bytes_per_row(panel_width: f32, char_w: f32) -> usize {
    if !char_w.is_finite() || char_w <= 0.0 || !panel_width.is_finite() {
        return 8;
    }
    let mut n = 8;
    // 4096 bytes per row is far past any real window; the bound only stops a
    // pathological `char_w` from spinning.
    while n < 4096 && hex_row_width(n + 8, char_w) <= panel_width {
        n += 8;
    }
    n
}

/// Width of one byte's block once `bpr` of them are spread across
/// `panel_width`. The zoom control picks a *target* block size; the panel then
/// redistributes the bytes so a row spans the full width exactly, leaving no
/// dead strip on the right.
pub(crate) fn zoom_block_w(panel_width: f32, bpr: usize) -> f32 {
    if bpr == 0 || !panel_width.is_finite() || panel_width <= 0.0 {
        return 1.0;
    }
    panel_width / bpr as f32
}

/// Bytes per row for the zoom panel: as many `zoom`-sized blocks as fit.
pub(crate) fn zoom_bytes_per_row(panel_width: f32, zoom: f32) -> usize {
    if !zoom.is_finite() || zoom <= 0.0 || !panel_width.is_finite() {
        return 1;
    }
    ((panel_width / zoom).floor() as usize).max(1)
}

/// The byte offset of the first row a panel with `bpr` bytes per row shows when
/// the shared anchor sits at `anchor`.
pub(crate) fn row_start_for(anchor: usize, bpr: usize) -> usize {
    if bpr == 0 {
        return anchor;
    }
    anchor - anchor % bpr
}

/// The row-aligned first visible offset for a panel showing `rows` rows of
/// `bpr` bytes, **centred** on the shared anchor: the anchor is the byte in the
/// middle of the viewport, so every panel puts that byte on the same line
/// however much data it shows. Near the start of the file this
/// saturates at 0 so the first rows stay reachable.
pub(crate) fn first_row_centred(anchor: usize, bpr: usize, rows: usize) -> usize {
    row_start_for(anchor.saturating_sub(rows / 2 * bpr), bpr)
}

/// The furthest the shared anchor may scroll: the start of the row holding the
/// last byte, in the hex column's row length — the hex column is the scroll
/// reference, so a panel with longer rows just runs out of file sooner and
/// paints what exists rather than scrolling independently.
pub(crate) fn max_anchor(file_size: usize, hex_bpr: usize) -> usize {
    if file_size == 0 || hex_bpr == 0 {
        return 0;
    }
    row_start_for(file_size - 1, hex_bpr)
}

/// Rows needed to cover `panel_height`, including a partially visible last row.
pub(crate) fn visible_rows(panel_height: f32, row_h: f32) -> usize {
    if !row_h.is_finite() || row_h <= 0.0 || !panel_height.is_finite() {
        return 1;
    }
    ((panel_height / row_h).ceil() as usize).max(1)
}

const MIN_THUMB_H: f32 = 24.0;

/// Thumb `(top, height)` for a vertical scrollbar `track_h` tall, showing
/// `visible` bytes of `len` anchored at `anchor`.
pub(crate) fn scrollbar_thumb(
    track_h: f32,
    anchor: usize,
    last_anchor: usize,
    visible: usize,
    len: usize,
) -> (f32, f32) {
    if !track_h.is_finite() || track_h <= 0.0 {
        return (0.0, 0.0);
    }
    if len == 0 || visible >= len || last_anchor == 0 {
        return (0.0, track_h);
    }
    let height = ((visible as f32 / len as f32) * track_h).clamp(MIN_THUMB_H.min(track_h), track_h);
    let travel = (track_h - height).max(0.0);
    let top = ((anchor as f32 / last_anchor as f32).clamp(0.0, 1.0) * travel).min(travel);
    (top, height)
}

/// The anchor a pointer at `y` on the track selects, grabbing the thumb by its
/// centre so the thumb lands under the cursor.
pub(crate) fn scrollbar_anchor_at(
    y: f32,
    track_h: f32,
    last_anchor: usize,
    visible: usize,
    len: usize,
) -> usize {
    if !y.is_finite() || !track_h.is_finite() || track_h <= 0.0 || len == 0 || visible >= len {
        return 0;
    }
    let (_, height) = scrollbar_thumb(track_h, 0, last_anchor, visible, len);
    let travel = (track_h - height).max(1.0);
    let t = ((y - height / 2.0) / travel).clamp(0.0, 1.0);
    (t * last_anchor as f32) as usize
}

/// Format a half-open byte range as a hex header label, e.g.
/// `0x00000000 – 0x000000FF`.
pub(crate) fn range_label(start: usize, end_exclusive: usize) -> String {
    if end_exclusive > start {
        format!("0x{start:08X} – 0x{:08X}", end_exclusive - 1)
    } else {
        format!("0x{start:08X}")
    }
}

/// Entropy (bits/byte) at `offset`: linearly interpolates between the two
/// `window`-sized blocks overlapping the offset.
pub(crate) fn entropy_at(entropies: &[f32], window: usize, offset: usize) -> f32 {
    let w = window.max(1);
    if entropies.is_empty() {
        return 0.0;
    }
    let block = (offset / w).min(entropies.len() - 1);
    let h0 = entropies[block];
    let Some(&h1) = entropies.get(block + 1) else {
        return h0;
    };
    let t = (offset % w) as f32 / w as f32;
    h0 + (h1 - h0) * t
}

/// Entropy at `offset` for a byte lookup under `colormap`. Only the `Entropy`
/// colormap consumes it, so the other three skip the interpolating lookup
/// entirely — without this gate the default `Class` hex colormap paid tens of
/// thousands of discarded interpolations per frame.
pub(crate) fn entropy_for(
    colormap: Colormap,
    entropies: &[f32],
    window: usize,
    offset: usize,
) -> f32 {
    if colormap.uses_entropy() {
        entropy_at(entropies, window, offset)
    } else {
        0.0
    }
}

/// The bytes a pane shows and how it colors them.
///
/// These four values travel together through every painter and thumbnail
/// generator — a colormap is meaningless without the entropy cache and window it
/// may need to consult — so they move as one argument rather than four, and the
/// per-byte lookup lives here instead of being rebuilt at each site.
pub(crate) struct ByteSource<'a> {
    pub data: &'a [u8],
    pub entropies: &'a [f32],
    pub entropy_window: usize,
    pub colormap: Colormap,
}

impl ByteSource<'_> {
    /// Color for `byte`, sampling entropy at `offset`. The two are separate
    /// because the thumbnails color a *sampled average* of a cell's bytes while
    /// reading entropy at the cell's midpoint.
    pub fn color_of(&self, byte: u8, offset: usize) -> Option<Rgb> {
        self.colormap.color_for(
            byte,
            entropy_for(self.colormap, self.entropies, self.entropy_window, offset),
        )
    }

    /// Color for the byte at `offset`. Panics if `offset` is out of bounds —
    /// callers already clamp to the visible range.
    pub fn color_at(&self, offset: usize) -> Option<Rgb> {
        self.color_of(self.data[offset], offset)
    }

    pub fn len(&self) -> usize {
        self.data.len()
    }

    pub fn is_empty(&self) -> bool {
        self.data.is_empty()
    }
}

/// Per-row horizontal geometry for one bytes-per-row layout. All offsets are
/// derived from the monospace glyph width so the text, the background cells
/// and the hit-testing always agree.
pub(crate) struct RowGeo {
    pub bpr: usize,
    pub hex_start: f32,
    pub cell_w: f32,
    pub group_gap: f32,
    pub ascii_start: f32,
    pub char_w: f32,
}

impl RowGeo {
    /// Build the per-row geometry from the monospace glyph width.
    pub fn new(char_w: f32, bpr: usize) -> Self {
        let hex_start = ADDR_X + 8.0 * char_w + 2.0 * char_w;
        let cell_w = 3.0 * char_w; // two hex digits + one space
        let group_gap = char_w; // extra space between 8-byte groups
        // `build_row_text` emits a space *between* groups, so a row of `bpr`
        // bytes has `(bpr - 1) / 8` of them, not `bpr / 8`. Counting one too
        // many put the ASCII block a full character right of its glyphs.
        let hex_w = bpr as f32 * cell_w + (bpr.saturating_sub(1) / 8) as f32 * group_gap;
        let ascii_start = hex_start + hex_w + 2.0 * char_w;
        Self {
            bpr,
            hex_start,
            cell_w,
            group_gap,
            ascii_start,
            char_w,
        }
    }

    pub fn cell_x(&self, i: usize) -> f32 {
        self.hex_start + i as f32 * self.cell_w + (i / 8) as f32 * self.group_gap
    }

    /// Width the byte colour fills in a hex cell: the two digits only, not the
    /// space that separates them from the next byte. `cell_w` remains the
    /// *advance* — hit-testing still claims the whole cell, so clicking in the
    /// gap selects the byte to its left.
    pub fn hex_fill_w(&self) -> f32 {
        2.0 * self.char_w
    }

    pub fn ascii_x(&self, i: usize) -> f32 {
        self.ascii_start + i as f32 * self.char_w
    }

    /// Column index under an x position within the row, or `None` when the
    /// pointer is over a gap or the address gutter.
    pub fn byte_at_x(&self, x: f32) -> Option<usize> {
        if x >= self.hex_start {
            for i in 0..self.bpr {
                let x0 = self.cell_x(i);
                if x >= x0 && x < x0 + self.cell_w {
                    return Some(i);
                }
            }
        }
        if x >= self.ascii_start {
            for i in 0..self.bpr {
                let x0 = self.ascii_x(i);
                if x >= x0 && x < x0 + self.char_w {
                    return Some(i);
                }
            }
        }
        None
    }
}

/// Map a pane-local `(x, y)` to a file offset, or `None` when outside the content
/// or over a gap. Coordinates are relative to the hex pane's origin, rows are
/// `row_h` apart and start at the row-aligned `first_row_start`.
///
/// `row_h` is a parameter rather than a constant so the same inverse works for a
/// pixel canvas and a character grid.
pub(crate) fn hex_offset_at(
    x: f32,
    y: f32,
    geo: &RowGeo,
    row_h: f32,
    first_row_start: usize,
    len: usize,
) -> Option<usize> {
    if y < 0.0 || len == 0 || !row_h.is_finite() || row_h <= 0.0 {
        return None;
    }
    let row = (y / row_h) as usize;
    let row_start = first_row_start.checked_add(row.checked_mul(geo.bpr)?)?;
    if row_start >= len {
        return None;
    }
    let i = geo.byte_at_x(x)?;
    let off = row_start + i;
    (off < len).then_some(off)
}

/// Map a pane-local `(x, y)` in the zoom pane to a file offset, or `None` when it is
/// outside the painted bytes. Rows are flush: one `zoom`-sized band per byte,
/// `zoom` tall, starting at the row-aligned `first_row_start`.
///
/// `paint_zoom` only draws `bpr` blocks per row, so anything to the right of
/// them is empty background and must not resolve to a byte — without the `col`
/// bound the offset would silently run on into later rows.
pub(crate) fn zoom_offset_at(
    x: f32,
    y: f32,
    bpr: usize,
    first_row_start: usize,
    block: f32,
    len: usize,
) -> Option<usize> {
    if x < 0.0 || y < 0.0 || len == 0 || bpr == 0 || !block.is_finite() || block <= 0.0 {
        return None;
    }
    let col = (x / block) as usize;
    if col >= bpr {
        return None;
    }
    let row = (y / block) as usize;
    let off = row
        .checked_mul(bpr)?
        .checked_add(first_row_start)?
        .checked_add(col)?;
    (off < len).then_some(off)
}

/// Build the display text of one hex row:
/// `ADDR  HH HH …  AAAA` with an extra space every 8 hex bytes. Writes into
/// caller-owned buffers that are cleared and refilled — `paint_hex` reuses one
/// set across every row instead of allocating per row.
pub(crate) fn build_row_text_into(
    data: &[u8],
    row_start: usize,
    n: usize,
    text: &mut String,
    hex_offsets: &mut Vec<usize>,
    ascii_offsets: &mut Vec<usize>,
) {
    text.clear();
    hex_offsets.clear();
    ascii_offsets.clear();
    let _ = write!(text, "{row_start:08X}  ");
    for i in 0..n {
        if i > 0 && i % 8 == 0 {
            text.push(' ');
        }
        let b = data[row_start + i];
        let off = text.len();
        let _ = write!(text, "{b:02X} ");
        hex_offsets.push(off);
    }
    text.push_str("  ");
    for i in 0..n {
        let off = text.len();
        text.push(color::printable(data[row_start + i]));
        ascii_offsets.push(off);
    }
}

#[cfg(test)]
pub(crate) mod test_support {
    //! Helpers shared by the geom, thumb and paint test modules.
    use super::ByteSource;
    use crate::core::color::Colormap;

    /// Read the RGBA pixel at `(x, y)` from a raw buffer with the given width.
    pub(crate) fn px(buf: &[u8], width: usize, x: usize, y: usize) -> (u8, u8, u8, u8) {
        let p = (y * width + x) * 4;
        (buf[p], buf[p + 1], buf[p + 2], buf[p + 3])
    }

    /// Per-window Shannon entropies for `data`, as handed to the generators.
    pub(crate) fn entropies(data: &[u8]) -> Vec<f32> {
        crate::core::entropy::block_entropies(data, 256)
    }

    /// The strip's raw pixels: one row of the overview, exactly as
    /// `build_strip_image` builds it.
    pub(crate) fn strip_rgba(
        data: &[u8],
        entropies: &[f32],
        entropy_window: usize,
        colormap: Colormap,
    ) -> Vec<u8> {
        crate::core::thumb::build_overview_rgba(
            &src(data, entropies, entropy_window, colormap),
            crate::core::thumb::STRIP_CELLS,
            1,
        )
    }

    /// A `ByteSource` over `data`, the shape every generator now takes.
    pub(crate) fn src<'a>(
        data: &'a [u8],
        entropies: &'a [f32],
        entropy_window: usize,
        colormap: Colormap,
    ) -> ByteSource<'a> {
        ByteSource {
            data,
            entropies,
            entropy_window,
            colormap,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_colour_fills_the_digits_but_not_the_gap() {
        let geo = RowGeo::new(8.0, 16);
        // Two digits wide, against a three-character advance: the space between
        // bytes is left to the panel background.
        assert_eq!(geo.hex_fill_w(), 16.0);
        assert_eq!(geo.cell_w, 24.0);
        assert!(geo.hex_fill_w() < geo.cell_w);
        // The uncovered strip is exactly one glyph, and the next cell starts
        // after it rather than overlapping the fill.
        assert_eq!(geo.cell_w - geo.hex_fill_w(), geo.char_w);
        assert_eq!(geo.cell_x(1) - geo.cell_x(0), geo.cell_w);
        // ASCII cells stay contiguous, so their colour still merges.
        assert_eq!(geo.ascii_x(1) - geo.ascii_x(0), geo.char_w);
    }

    #[test]
    fn entropy_at_interpolates_across_block_boundaries() {
        let e = [0.0f32, 8.0];
        assert_eq!(entropy_at(&e, 256, 0), 0.0);
        assert!((entropy_at(&e, 256, 255) - 8.0 * 255.0 / 256.0).abs() < 1e-5);
        assert_eq!(entropy_at(&e, 256, 256), 8.0); // last block has no neighbor
        assert_eq!(entropy_at(&e, 256, 10_000), 8.0); // clamped to last block
        assert_eq!(entropy_at(&[], 256, 5), 0.0); // empty cache
    }

    /// `char_w` = 10 keeps every cell boundary an exact number, so the
    /// expectations below are precise rather than approximate.
    fn row_geo() -> RowGeo {
        RowGeo::new(10.0, 16)
    }

    /// The x a monospace glyph lands at, given its character offset in the row
    /// text: the row is painted at `ADDR_X`, so glyph `k` sits at
    /// `ADDR_X + k * char_w`.
    fn glyph_x(char_offset: usize, char_w: f32) -> f32 {
        ADDR_X + char_offset as f32 * char_w
    }

    #[test]
    fn hex_and_ascii_glyphs_sit_on_their_background_cells() {
        let char_w = 10.0;
        for bpr in [8usize, 16, 32] {
            let geo = RowGeo::new(char_w, bpr);
            let data: Vec<u8> = (0..bpr).map(|i| i as u8).collect();
            let mut text = String::new();
            let mut hex_offsets = Vec::new();
            let mut ascii_offsets = Vec::new();
            build_row_text_into(
                &data,
                0,
                bpr,
                &mut text,
                &mut hex_offsets,
                &mut ascii_offsets,
            );
            for i in 0..bpr {
                assert_eq!(
                    glyph_x(hex_offsets[i], char_w),
                    geo.cell_x(i),
                    "hex byte {i} of {bpr} misaligned"
                );
                assert_eq!(
                    glyph_x(ascii_offsets[i], char_w),
                    geo.ascii_x(i),
                    "ascii byte {i} of {bpr} misaligned"
                );
            }
        }
    }

    #[test]
    fn row_geo_byte_at_x_maps_cells_gaps_and_ascii() {
        let geo = row_geo();
        // Derived layout: addr gutter (8 + 10 glyphs), then hex cells.
        assert_eq!(geo.hex_start, 108.0);
        assert_eq!(geo.cell_w, 30.0);
        assert_eq!(geo.ascii_start, 618.0);

        // Hex cells are contiguous: cell i spans [cell_x(i), cell_x(i)+cell_w).
        assert_eq!(geo.cell_x(0), 108.0);
        assert_eq!(geo.byte_at_x(108.0), Some(0));
        assert_eq!(geo.byte_at_x(123.0), Some(0));
        assert_eq!(geo.byte_at_x(137.9), Some(0));
        assert_eq!(geo.byte_at_x(138.0), Some(1)); // next cell starts exactly

        // The address gutter and the 1-glyph group gap (after every 8 bytes)
        // are not selectable cells.
        assert_eq!(geo.byte_at_x(107.9), None);
        assert_eq!(geo.cell_x(7) + geo.cell_w, 348.0);
        assert_eq!(geo.byte_at_x(348.0), None);
        assert_eq!(geo.byte_at_x(350.0), None);
        assert_eq!(geo.byte_at_x(358.0), Some(8));

        // ASCII cells sit after a fixed gap and are char_w wide.
        assert_eq!(geo.byte_at_x(617.9), None);
        assert_eq!(geo.byte_at_x(618.0), Some(0));
        assert_eq!(geo.byte_at_x(633.0), Some(1));
        assert_eq!(geo.byte_at_x(geo.ascii_x(15) + geo.char_w), None); // past last byte
    }

    #[test]
    fn zoom_offset_at_maps_rows_flush_and_rejects_blank_space() {
        // 16 bytes/row at 4 px: bytes span x in [0,64); rows are 4 px tall.
        let hit = |x: f32, y: f32, first: usize| zoom_offset_at(x, y, 16, first, 4.0, 60);

        assert_eq!(hit(0.0, 0.0, 0), Some(0));
        assert_eq!(hit(63.9, 0.0, 0), Some(15));
        assert_eq!(hit(64.0, 0.0, 0), None); // right of the last byte
        assert_eq!(hit(300.0, 0.0, 0), None);
        // Rows are flush: the second row starts at y = zoom, not 2*zoom + 1.
        assert_eq!(hit(0.0, 4.0, 0), Some(16));
        assert_eq!(hit(0.0, 8.0, 0), Some(32));
        // Anchored elsewhere in the file.
        assert_eq!(hit(0.0, 0.0, 32), Some(32));
        assert_eq!(hit(4.0, 4.0, 32), Some(49));
        // Past end of file (the last row holds only 48..60).
        assert_eq!(hit(48.0, 0.0, 48), None);
        assert_eq!(hit(44.0, 0.0, 48), Some(59));
        // Degenerate inputs.
        assert_eq!(hit(-1.0, 0.0, 0), None);
        assert_eq!(hit(0.0, -1.0, 0), None);
        assert_eq!(zoom_offset_at(0.0, 0.0, 16, 0, 4.0, 0), None);
        assert_eq!(zoom_offset_at(0.0, 0.0, 16, 0, 0.0, 60), None);
    }

    #[test]
    fn hex_offset_at_maps_y_to_row_and_x_to_byte() {
        let geo = row_geo();
        let len = 64usize;
        // Row pitch is a parameter now; 21.0 is what the gpui frontend passes
        // (paint::BLOCK_H), and the expected offsets below are derived from it.
        let hit = |y: f32, x: f32| hex_offset_at(x, y, &geo, 21.0, 0, len);

        // Row 0 (offsets 0..16), both hex and ascii cells.
        assert_eq!(hit(0.0, 108.0), Some(0));
        assert_eq!(hit(0.0, 138.0), Some(1));
        assert_eq!(hit(10.0, 108.0), Some(0)); // still within the first row block
        assert_eq!(hit(0.0, 618.0), Some(0));
        // Row 1 starts at y = BLOCK_H.
        assert_eq!(hit(21.0, 108.0), Some(16));
        assert_eq!(hit(21.0, 618.0), Some(16));
        // Last full row (offsets 48..64).
        assert_eq!(hit(63.0, 108.0), Some(48));

        // Outside the content.
        assert_eq!(hit(-1.0, 108.0), None); // above the first row
        assert_eq!(hit(84.0, 108.0), None); // past end of file
        assert_eq!(hit(0.0, 50.0), None); // address gutter
        assert_eq!(hit(0.0, 350.0), None); // group gap between bytes 7 and 8
    }

    #[test]
    fn hex_offset_at_is_anchored_and_clamps_to_file_end() {
        let geo = row_geo();
        // 60 bytes -> the last row holds only 12 bytes (48..60).
        let len = 60usize;
        let hit = |y: f32, x: f32| hex_offset_at(x, y, &geo, 21.0, 32, len);

        // Anchored at byte 32: the top row is 32..48, the next 48..60.
        assert_eq!(hit(0.0, 108.0), Some(32));
        assert_eq!(hit(21.0, 108.0), Some(48));
        // Last byte of the file sits in cell 11 of that row (48 + 11 = 59).
        assert_eq!(geo.cell_x(11), 448.0);
        assert_eq!(hit(21.0, 448.0), Some(59));
        // Cell 12 would be offset 60 == len: clamped to None.
        assert_eq!(hit(21.0, 478.0), None);
        // A row below the last one is always None.
        assert_eq!(hit(42.0, 108.0), None);
    }

    #[test]
    fn hex_bytes_per_row_fits_and_snaps_to_eight() {
        let char_w = 10.0;
        // width_for(n) = ADDR_X + char_w * (12 + 4n + (n-1)/8)
        // n = 8  -> 8 + 10 * (12 + 32 + 0) = 448
        // n = 16 -> 8 + 10 * (12 + 64 + 1) = 778
        // n = 24 -> 8 + 10 * (12 + 96 + 2) = 1108
        assert_eq!(hex_bytes_per_row(448.0, char_w), 8);
        assert_eq!(hex_bytes_per_row(777.0, char_w), 8);
        assert_eq!(hex_bytes_per_row(778.0, char_w), 16);
        assert_eq!(hex_bytes_per_row(1107.0, char_w), 16);
        assert_eq!(hex_bytes_per_row(1108.0, char_w), 24);
        // Always a multiple of 8, never below 8, however narrow the panel.
        assert_eq!(hex_bytes_per_row(0.0, char_w), 8);
        assert_eq!(hex_bytes_per_row(-50.0, char_w), 8);
        for w in [500.0, 900.0, 1500.0, 4000.0] {
            assert_eq!(hex_bytes_per_row(w, char_w) % 8, 0, "width {w}");
        }
        // Degenerate glyph width must not divide by zero or loop forever.
        assert_eq!(hex_bytes_per_row(1000.0, 0.0), 8);
        assert_eq!(hex_bytes_per_row(f32::NAN, char_w), 8);
    }

    #[test]
    fn zoom_block_w_spreads_bytes_across_the_whole_width() {
        // 319 px at a target of 4 px: 79 blocks fit, widened to 4.038 px each
        // so the row spans the panel exactly instead of leaving a dead strip.
        let bpr = zoom_bytes_per_row(319.0, 4.0);
        assert_eq!(bpr, 79);
        let block = zoom_block_w(319.0, bpr);
        assert!(
            block >= 4.0,
            "block {block} must not shrink below the target"
        );
        assert!(
            (bpr as f32 * block - 319.0).abs() < 1e-3,
            "row must fill the width exactly, got {}",
            bpr as f32 * block
        );
        // Degenerate inputs stay usable rather than dividing by zero.
        assert_eq!(zoom_block_w(319.0, 0), 1.0);
        assert_eq!(zoom_block_w(0.0, 8), 1.0);
        assert_eq!(zoom_block_w(f32::NAN, 8), 1.0);
    }

    #[test]
    fn zoom_bytes_per_row_is_width_over_zoom() {
        assert_eq!(zoom_bytes_per_row(320.0, 4.0), 80);
        assert_eq!(zoom_bytes_per_row(320.0, 8.0), 40);
        assert_eq!(zoom_bytes_per_row(321.0, 8.0), 40); // floors
        assert_eq!(zoom_bytes_per_row(7.0, 8.0), 1); // never zero
        assert_eq!(zoom_bytes_per_row(0.0, 8.0), 1);
        assert_eq!(zoom_bytes_per_row(320.0, 0.0), 1); // degenerate zoom
        assert_eq!(zoom_bytes_per_row(f32::NAN, 8.0), 1);
    }

    #[test]
    fn scrollbar_thumb_tracks_the_anchor() {
        let (len, visible, last) = (1000usize, 100usize, 900usize);
        // At the top, the thumb sits at the top and is 1/10th of the track.
        let (top, h) = scrollbar_thumb(400.0, 0, last, visible, len);
        assert_eq!(top, 0.0);
        assert!((h - 40.0).abs() < 1e-3, "h={h}");
        // At the last anchor it bottoms out exactly.
        let (top, h) = scrollbar_thumb(400.0, last, last, visible, len);
        assert!((top + h - 400.0).abs() < 1e-3, "top={top} h={h}");
        // Halfway.
        let (top, _) = scrollbar_thumb(400.0, last / 2, last, visible, len);
        assert!((top - 180.0).abs() < 1.0, "top={top}");
        // A tiny visible fraction still leaves a grabbable thumb.
        let (_, h) = scrollbar_thumb(400.0, 0, 9_999_999, 1, 10_000_000);
        assert!(h >= 24.0, "h={h}");
        // Whole file visible, no file, or nowhere to scroll: full-height thumb.
        assert_eq!(scrollbar_thumb(400.0, 0, last, 1000, 1000), (0.0, 400.0));
        assert_eq!(scrollbar_thumb(400.0, 0, last, 10, 0), (0.0, 400.0));
        assert_eq!(scrollbar_thumb(400.0, 0, 0, 10, 100), (0.0, 400.0));
        assert_eq!(scrollbar_thumb(0.0, 0, last, 10, 100), (0.0, 0.0));
    }

    #[test]
    fn scrollbar_anchor_round_trips_with_the_thumb() {
        let (track, visible, len, last) = (400.0, 100usize, 1000usize, 900usize);
        // Dragging to the top and bottom saturates.
        assert_eq!(scrollbar_anchor_at(-10.0, track, last, visible, len), 0);
        assert_eq!(scrollbar_anchor_at(0.0, track, last, visible, len), 0);
        assert_eq!(scrollbar_anchor_at(500.0, track, last, visible, len), last);
        // Grabbing the thumb centre puts the thumb back where it was.
        for anchor in [0usize, 200, 450, 900] {
            let (top, h) = scrollbar_thumb(track, anchor, last, visible, len);
            let back = scrollbar_anchor_at(top + h / 2.0, track, last, visible, len);
            assert!(back.abs_diff(anchor) <= 4, "anchor {anchor} -> {back}");
        }
        // Degenerate inputs.
        assert_eq!(scrollbar_anchor_at(10.0, track, last, 1000, 1000), 0);
        assert_eq!(scrollbar_anchor_at(10.0, 0.0, last, 10, 100), 0);
    }

    #[test]
    fn row_start_aligns_the_anchor_to_each_panel() {
        assert_eq!(row_start_for(0, 16), 0);
        assert_eq!(row_start_for(15, 16), 0);
        assert_eq!(row_start_for(16, 16), 16);
        assert_eq!(row_start_for(100, 16), 96);
        // The same anchor aligns differently per panel — that is the point.
        assert_eq!(row_start_for(100, 40), 80);
        assert_eq!(row_start_for(100, 1), 100);
        assert_eq!(row_start_for(100, 0), 100); // degenerate bpr is a no-op
    }

    #[test]
    fn first_row_centred_puts_the_anchor_in_the_middle() {
        // 10 rows of 16 bytes: half a screen is 5 rows = 80 bytes.
        assert_eq!(first_row_centred(800, 16, 10), 720);
        // The anchor byte lands on row 5 of the 10 shown.
        assert_eq!((800 - first_row_centred(800, 16, 10)) / 16, 5);
        // Near the start of the file it saturates so row 0 stays reachable.
        assert_eq!(first_row_centred(0, 16, 10), 0);
        assert_eq!(first_row_centred(50, 16, 10), 0);
        // Two panels showing very different amounts of data start at very
        // different rows for the same anchor — which is exactly why they align
        // on their middle line and not their top.
        let hex_first = first_row_centred(8000, 16, 10); // 160 bytes visible
        let zoom_first = first_row_centred(8000, 80, 100); // 8000 bytes visible
        assert_eq!(hex_first, 7920);
        assert_eq!(zoom_first, 4000);
        // ...yet the anchor sits at the centre of both.
        assert_eq!(hex_first + 5 * 16, 8000);
        assert_eq!(zoom_first + 50 * 80, 8000);
        assert_eq!(first_row_centred(100, 0, 10), 100); // degenerate bpr
    }

    #[test]
    fn max_anchor_is_the_last_hex_row_start() {
        // 60 bytes, 16 per row -> rows at 0,16,32,48; the last starts at 48.
        assert_eq!(max_anchor(60, 16), 48);
        assert_eq!(max_anchor(64, 16), 48);
        assert_eq!(max_anchor(65, 16), 64);
        assert_eq!(max_anchor(1, 16), 0);
        assert_eq!(max_anchor(0, 16), 0); // empty file
        assert_eq!(max_anchor(60, 0), 0); // degenerate bpr
    }

    #[test]
    fn visible_rows_covers_partial_rows() {
        assert_eq!(visible_rows(100.0, 20.0), 5);
        assert_eq!(visible_rows(101.0, 20.0), 6); // 5 full + 1 partial
        assert_eq!(visible_rows(0.0, 20.0), 1);
        assert_eq!(visible_rows(100.0, 0.0), 1); // degenerate row height
        assert_eq!(visible_rows(f32::NAN, 20.0), 1);
    }
}
