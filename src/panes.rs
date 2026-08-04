//! Central panes: three synchronized views of the same file.
//!
//! Layout (left to right): the **overview** column (whole-file thumbnail),
//! the **pixels** column (per-byte greyscale + entropy bands) and the
//! **hex** column (class-colored hex + ASCII cells). All three share one
//! scroll position (`app.scroll_rows`, in rows), so they stay in sync:
//! scrolling or dragging any column pans the others.
//!
//! Everything here is a pure function of the snapshot values handed to it:
//! gpui canvases cannot borrow the view, so `app.rs` clones the data it
//! needs (Arc'd mmap + entropy cache) and calls these helpers from the
//! canvas paint closures.

use std::fmt::Write as _;
use std::ops::Range;
use std::sync::Arc;

use rayon::prelude::*;

use gpui::{
    App, Background, BorderStyle, Bounds, Corners, Font, Hsla, PaintQuad, Pixels, Point,
    RenderImage, Rgba, ShapedLine, TextRun, Window, font, point, px, quad, rgb, rgba, size,
    transparent_black,
};

use crate::color::{self, Colormap};

pub(crate) const PIXEL_ZOOM_DEFAULT: f32 = 4.0;
pub(crate) const PIXEL_ZOOM_MIN: f32 = 1.0;
pub(crate) const PIXEL_ZOOM_MAX: f32 = 24.0;

/// Keyboard zoom step factor (`+` / `-`), applied multiplicatively per press.
pub(crate) const ZOOM_STEP: f32 = 1.25;

/// Clamp range for the entropy window, in bytes. Shared by the load-time clamp,
/// the slider's pointer→value mapping and its thumb position: three copies of
/// these bounds could disagree, and a thumb that renders somewhere a drag won't
/// put it is the visible symptom.
pub(crate) const ENTROPY_WINDOW_MIN: usize = 16;
pub(crate) const ENTROPY_WINDOW_MAX: usize = 4096;

/// Hex row geometry: the text size is fixed, so these are constants.
pub(crate) const ROW_H: f32 = 18.0;
pub(crate) const ROW_GAP: f32 = 3.0;
/// Monospace size for hex cells.
pub(crate) const HEX_FONT_SIZE: f32 = 13.0;
/// Left padding for the address gutter.
pub(crate) const ADDR_X: f32 = 8.0;
/// Height of one hex row including its separator gap. The hex text size is
/// fixed, so this is a constant rather than a function of a zoom.
pub(crate) const BLOCK_H: f32 = ROW_H + ROW_GAP;
/// Glyph color when the panel's colormap paints no background to contrast with.
const DEFAULT_FG: u32 = 0xc0caf5;

/// Apply a multiplicative zoom step, clamped to `[min, max]`. Shared by the
/// Ctrl+wheel handlers and the `+`/`-` keyboard shortcuts.
pub(crate) fn zoom_step(zoom: f32, factor: f32, min: f32, max: f32) -> f32 {
    (zoom * factor).clamp(min, max)
}

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

/// Width of the hex column's scrollbar, and the smallest thumb that stays
/// grabbable on a huge file.
pub(crate) const SCROLLBAR_W: f32 = 10.0;
const MIN_THUMB_H: f32 = 24.0;

/// A solid background quad: the common case of `quad` with no corner radius
/// and no border. Replaces the repeated five-argument `quad(...)` call sites.
pub(crate) fn filled_quad(bounds: Bounds<Pixels>, color: impl Into<Background>) -> PaintQuad {
    quad(
        bounds,
        px(0.),
        color.into(),
        px(0.),
        transparent_black(),
        BorderStyle::default(),
    )
}

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

/// Paint the hex column's scrollbar: a dim track with a brighter thumb.
pub(crate) fn paint_scrollbar(
    window: &mut Window,
    bounds: Bounds<Pixels>,
    anchor: usize,
    last_anchor: usize,
    visible: usize,
    len: usize,
) {
    window.paint_quad(filled_quad(bounds, rgba(0x00000033)));
    let track_h = bounds.size.height.to_f64() as f32;
    let (top, height) = scrollbar_thumb(track_h, anchor, last_anchor, visible, len);
    if height <= 0.0 {
        return;
    }
    let thumb = Bounds::new(
        point(bounds.left() + px(2.), bounds.top() + px(top)),
        size(bounds.size.width - px(4.), px(height)),
    );
    window.paint_quad(quad(
        thumb,
        px(3.),
        rgba(0x565f89cc),
        px(0.),
        transparent_black(),
        BorderStyle::default(),
    ));
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

/// The monospace font used for the hex/ASCII column, at `zoom`.
pub(crate) fn mono_font(family: &str) -> Font {
    font(family.to_owned())
}

/// Convert an RGBA color to the HSLA used by gpui text runs.
fn to_hsla(c: Rgba) -> Hsla {
    Hsla::from(c)
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

/// Map a canvas-local point to a file offset, or `None` when outside the
/// content or over a gap. `local` is relative to the hex canvas origin and rows
/// start at the row-aligned `first_row_start`.
pub(crate) fn hex_offset_at(
    local: Point<Pixels>,
    geo: &RowGeo,
    first_row_start: usize,
    len: usize,
) -> Option<usize> {
    let y = local.y.to_f64() as f32;
    if y < 0.0 || len == 0 {
        return None;
    }
    let row = (y / BLOCK_H) as usize;
    let row_start = first_row_start.checked_add(row.checked_mul(geo.bpr)?)?;
    if row_start >= len {
        return None;
    }
    let i = geo.byte_at_x(local.x.to_f64() as f32)?;
    let off = row_start + i;
    (off < len).then_some(off)
}

/// Map a point in the zoom canvas to a file offset, or `None` when it is
/// outside the painted bytes. Rows are flush: one `zoom`-sized band per byte,
/// `zoom` tall, starting at the row-aligned `first_row_start`.
///
/// `paint_zoom` only draws `bpr` blocks per row, so anything to the right of
/// them is empty background and must not resolve to a byte — without the `col`
/// bound the offset would silently run on into later rows.
pub(crate) fn zoom_offset_at(
    local: Point<Pixels>,
    bpr: usize,
    first_row_start: usize,
    block: f32,
    len: usize,
) -> Option<usize> {
    let x = local.x.to_f64() as f32;
    let y = local.y.to_f64() as f32;
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

// ---------------------------------------------------------------------------
// Text building for a hex row
// ---------------------------------------------------------------------------

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

/// Glyph color for a cell: contrast text against its background, or the
/// default foreground when the colormap paints no background.
fn cell_glyph_color(cell: Option<Rgba>) -> Hsla {
    match cell {
        Some(bg) => to_hsla(color::fg_for_bg(bg)),
        None => to_hsla(rgb(DEFAULT_FG)),
    }
}

/// Build the color runs for a row line: gray address, contrast-colored hex
/// digits and ASCII glyphs per byte, neutral (invisible) spaces. Glyph colors
/// come from the row's precomputed per-byte cell colors (`colors[i]`), so each
/// byte's color is computed once and reused for both cells and both glyphs
/// rather than four times per byte. Runs are written into the caller-owned
/// `runs` buffer.
#[allow(clippy::too_many_arguments)]
fn build_row_runs(
    n: usize,
    hex_offsets: &[usize],
    ascii_offsets: &[usize],
    font: &Font,
    total_len: usize,
    colors: &[Option<Rgba>],
    runs: &mut Vec<TextRun>,
) {
    runs.clear();
    let neutral = to_hsla(rgba(0x00000000));
    let addr_color = to_hsla(rgb(0x9a9a9a));
    let mut cur = 10usize;

    let gap = |from: usize, to: usize, runs: &mut Vec<TextRun>| {
        if to > from {
            runs.push(TextRun {
                len: to - from,
                font: font.clone(),
                color: neutral,
                background_color: None,
                underline: None,
                strikethrough: None,
            });
        }
    };

    // Address + leading spaces (chars 0..10).
    runs.push(TextRun {
        len: 10,
        font: font.clone(),
        color: addr_color,
        background_color: None,
        underline: None,
        strikethrough: None,
    });

    // Hex cells.
    for i in 0..n {
        gap(cur, hex_offsets[i], runs);
        cur = hex_offsets[i];
        let fg = cell_glyph_color(colors[i]);
        runs.push(TextRun {
            len: 2,
            font: font.clone(),
            color: fg,
            background_color: None,
            underline: None,
            strikethrough: None,
        });
        cur += 2;
        runs.push(TextRun {
            len: 1,
            font: font.clone(),
            color: neutral,
            background_color: None,
            underline: None,
            strikethrough: None,
        });
        cur += 1;
    }

    // Gap to the ASCII section.
    if n > 0 {
        gap(cur, ascii_offsets[0], runs);
        cur = ascii_offsets[0];
    }

    // ASCII cells.
    for &cell in colors.iter().take(n) {
        runs.push(TextRun {
            len: 1,
            font: font.clone(),
            color: cell_glyph_color(cell),
            background_color: None,
            underline: None,
            strikethrough: None,
        });
        cur += 1;
    }

    gap(cur, total_len, runs);
}

// ---------------------------------------------------------------------------
// Painting
// ---------------------------------------------------------------------------

/// Width of one glyph in the given monospace font, in pixels. Derived from
/// shaping 64 zeroes so the result is a clean average (no sub-pixel rounding
/// on a single glyph).
pub(crate) fn hex_char_width(window: &mut Window, font: &Font, font_size: Pixels) -> f32 {
    let run = TextRun {
        len: 64,
        font: font.clone(),
        color: gpui::white(),
        background_color: None,
        underline: None,
        strikethrough: None,
    };
    let line: ShapedLine = window.text_system().shape_line(
        "0000000000000000000000000000000000000000000000000000000000000000".into(),
        font_size,
        &[run],
        None,
    );
    (line.x_for_index(64).to_f64() as f32 / 64.0).max(1.0)
}

/// Paint merged quads for a run of cell backgrounds. A run extends while the
/// (color, selection) state is unchanged and stays within one group of
/// `split_every` cells (the hex side splits at 8 so a merged quad never covers
/// the group gap, which shows the panel background; `usize::MAX` merges the
/// whole row). Cell x comes from `x_of`.
#[allow(clippy::too_many_arguments)]
fn paint_cell_runs(
    window: &mut Window,
    origin: Point<Pixels>,
    y0: f32,
    colors: &[Option<Rgba>],
    selected: &[bool],
    x_of: impl Fn(usize) -> f32,
    cell_w: f32,
    split_every: usize,
) {
    let n = colors.len();
    let mut i = 0;
    while i < n {
        let bg = colors[i];
        let s = selected[i];
        let mut j = i + 1;
        while j < n && colors[j] == bg && selected[j] == s && j % split_every != 0 {
            j += 1;
        }
        let x0 = x_of(i);
        let rect = Bounds::new(
            point(origin.x + px(x0), origin.y + px(y0)),
            size(px(x_of(j - 1) + cell_w - x0), px(ROW_H)),
        );
        if let Some(bg) = bg {
            window.paint_quad(filled_quad(rect, bg));
        }
        if s {
            window.paint_quad(filled_quad(rect, rgba(0xffffff3d)));
        }
        i = j;
    }
}

/// Paint the hex column into `bounds`: class-colored hex + ASCII cells with
/// selection/hover overlays, one virtualized row at a time. Cell-background
/// quads are merged into runs of identical (color, selection) state — binary
/// data is repetitive, so the 2·bpr quads per row typically collapse to a
/// handful. Runs split at every 8-byte group boundary so a merged quad never
/// covers the group gap, which shows the panel background.
#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
pub(crate) fn paint_hex(
    window: &mut Window,
    cx: &mut App,
    bounds: Bounds<Pixels>,
    data: &[u8],
    font: &Font,
    char_w: f32,
    bpr: usize,
    first_row_start: usize,
    hovered: Option<usize>,
    sel: Option<&Range<usize>>,
    entropies: &[f32],
    entropy_window: usize,
    colormap: Colormap,
) {
    let len = data.len();
    if len == 0 || bpr == 0 {
        return;
    }
    let row_h = ROW_H;
    let font_size = px(HEX_FONT_SIZE);
    let geo = RowGeo::new(char_w, bpr);

    let rows = visible_rows(bounds.size.height.to_f64() as f32, BLOCK_H);

    let origin = bounds.origin;
    // Reusable per-row buffers: cleared and refilled for each row instead of
    // reallocated.
    let mut text = String::with_capacity(128);
    let mut hex_offsets = Vec::with_capacity(bpr);
    let mut ascii_offsets = Vec::with_capacity(bpr);
    let mut colors: Vec<Option<Rgba>> = Vec::with_capacity(bpr);
    let mut selected = vec![false; bpr];
    let mut runs: Vec<TextRun> = Vec::new();
    for r in 0..rows {
        let row_start = first_row_start + r * bpr;
        if row_start >= len {
            break;
        }
        let y0 = r as f32 * BLOCK_H;
        let n = (len - row_start).min(bpr);

        // Each byte's cell color is computed once and reused for the hex cell,
        // the ASCII cell and both glyphs, not recomputed for each of the four.
        colors.clear();
        colors.extend((0..n).map(|i| {
            let off = row_start + i;
            colormap.color_for(
                data[off],
                entropy_for(colormap, entropies, entropy_window, off),
            )
        }));
        for (i, s) in selected.iter_mut().enumerate().take(n) {
            *s = sel.is_some_and(|r| r.contains(&(row_start + i)));
        }

        // Merged background + selection quads. Hex cells split at every 8-byte
        // group boundary so merged quads never cover the group gap; ASCII
        // cells are contiguous and merge across the whole row.
        paint_cell_runs(
            window,
            origin,
            y0,
            &colors[..n],
            &selected[..n],
            |i| geo.cell_x(i),
            geo.cell_w,
            8,
        );
        paint_cell_runs(
            window,
            origin,
            y0,
            &colors[..n],
            &selected[..n],
            |i| geo.ascii_x(i),
            geo.char_w,
            usize::MAX,
        );

        // Hover outline across hex + ascii cells.
        if let Some(o) = hovered
            && (row_start..row_start + n).contains(&o)
        {
            let i = o - row_start;
            let rect = Bounds::new(
                point(origin.x + px(geo.cell_x(i)), origin.y + px(y0)),
                size(px(geo.cell_w), px(row_h)),
            );
            window.paint_quad(quad(
                rect,
                px(0.),
                transparent_black(),
                px(1.),
                gpui::white(),
                BorderStyle::default(),
            ));
        }

        // Text.
        build_row_text_into(
            data,
            row_start,
            n,
            &mut text,
            &mut hex_offsets,
            &mut ascii_offsets,
        );
        build_row_runs(
            n,
            &hex_offsets,
            &ascii_offsets,
            font,
            text.len(),
            &colors[..n],
            &mut runs,
        );
        let line = window
            .text_system()
            .shape_line(text.clone().into(), font_size, &runs, None);
        // `RowGeo` builds ADDR_X into `hex_start`, so the glyphs need the same
        // gutter or every background sits half a byte right of its digits.
        let _ = line.paint(
            point(origin.x + px(ADDR_X), origin.y + px(y0)),
            px(row_h),
            window,
            cx,
        );
    }
}

/// Paint the zoom column into `bounds`: the visible bytes as a single
/// `RenderImage` texture (built by `build_zoom_image`, cached across frames),
/// with selection, hover and the hex-column mark as overlay quads on top.
/// Rows are flush so the panel reads as a pixel image; the texture is built at
/// one pixel per screen pixel so it needs no smoothing. One upload per changed
/// visible region replaces the original quad-per-byte path, whose worst case was
/// ~540k quads per frame at `pixel_zoom = 1`.
#[allow(clippy::too_many_arguments)]
pub(crate) fn paint_zoom(
    window: &mut Window,
    bounds: Bounds<Pixels>,
    image: Option<&Arc<RenderImage>>,
    bpr: usize,
    first_row_start: usize,
    block: f32,
    hovered: Option<usize>,
    sel: Option<&Range<usize>>,
    mark: Option<&Range<usize>>,
    len: usize,
) {
    if bpr == 0 || !block.is_finite() || block <= 0.0 {
        return;
    }
    if let Some(image) = image {
        let _ = window.paint_image(bounds, Corners::all(px(0.)), image.clone(), 0, false);
    }
    let rows = visible_rows(bounds.size.height.to_f64() as f32, block);

    // Overlays: merged selection-tint quads, then the hover outline. The
    // selection is a single contiguous byte range, so its intersection with
    // each row is one merged quad.
    if let Some(sel) = sel
        && !sel.is_empty()
    {
        let y0px = bounds.top().to_f64() as f32;
        for r in 0..rows {
            let row_start = first_row_start + r * bpr;
            if row_start >= len {
                break;
            }
            let n = (len - row_start).min(bpr);
            let row_end = row_start + n;
            if sel.start >= row_end || sel.end <= row_start {
                continue;
            }
            let i0 = sel.start.saturating_sub(row_start);
            let i1 = sel.end.min(row_end) - row_start;
            let rect = Bounds::new(
                point(
                    bounds.left() + px(i0 as f32 * block),
                    px(y0px + r as f32 * block),
                ),
                size(px((i1 - i0) as f32 * block), px(block)),
            );
            window.paint_quad(filled_quad(rect, rgba(0xffffff30)));
        }
    }
    if let Some(off) = hovered
        && off >= first_row_start
    {
        let rel = off - first_row_start;
        let r = rel / bpr;
        if r < rows {
            let i = rel % bpr;
            let rect = Bounds::new(
                point(
                    bounds.left() + px(i as f32 * block),
                    px(bounds.top().to_f64() as f32 + r as f32 * block),
                ),
                size(px(block), px(block)),
            );
            window.paint_quad(quad(
                rect,
                px(0.),
                transparent_black(),
                px(1.),
                gpui::white(),
                BorderStyle::default(),
            ));
        }
    }

    // The rows the next panel (hex) is showing.
    if let Some(mark) = mark {
        paint_row_band(window, bounds, mark, first_row_start, bpr, block, rows);
    }
}

/// Outline the rows covering `mark` — the byte range the next panel to the
/// right is displaying — so each panel shows where the next one is looking.
fn paint_row_band(
    window: &mut Window,
    bounds: Bounds<Pixels>,
    mark: &Range<usize>,
    first_row_start: usize,
    bpr: usize,
    row_h: f32,
    rows: usize,
) {
    if mark.is_empty() || bpr == 0 {
        return;
    }
    let first_row = first_row_start / bpr;
    let start_row = (mark.start / bpr).max(first_row);
    let end_row = ((mark.end - 1) / bpr).max(start_row);
    if end_row < first_row {
        return;
    }
    let top = (start_row - first_row) as f32 * row_h;
    let height = ((end_row - start_row + 1) as f32 * row_h).max(2.0);
    let visible_h = rows as f32 * row_h;
    if top >= visible_h {
        return;
    }
    let band = Bounds::new(
        point(bounds.left(), bounds.top() + px(top)),
        size(bounds.size.width, px(height.min(visible_h - top))),
    );
    window.paint_quad(quad(
        band,
        px(0.),
        rgba(0xffffff1f),
        px(1.),
        rgba(0x7aa2f7cc),
        BorderStyle::default(),
    ));
}

/// Paint the whole-file 2D overview into `bounds`: the greyscale/entropy
/// thumbnail with a translucent band marking the visible byte range.
pub(crate) fn paint_overview(
    window: &mut Window,
    bounds: Bounds<Pixels>,
    image: &Arc<RenderImage>,
    file_size: usize,
    mark: Option<&Range<usize>>,
) {
    let _ = window.paint_image(bounds, Corners::all(px(2.)), image.clone(), 0, false);
    let Some(mark) = mark else { return };
    if file_size == 0 || mark.is_empty() {
        return;
    }
    // Cells run row-major top-to-bottom, so a byte range is a horizontal band.
    let h = bounds.size.height.to_f64() as f32;
    let frac = |off: usize| (off as f32 / file_size as f32).clamp(0.0, 1.0);
    let y0 = frac(mark.start) * h;
    let y1 = frac(mark.end) * h;
    let band = Bounds::new(
        point(bounds.left(), bounds.top() + px(y0)),
        size(bounds.size.width, px((y1 - y0).max(2.0))),
    );
    window.paint_quad(quad(
        band,
        px(0.),
        rgba(0xffffff2e),
        px(1.),
        rgba(0x7aa2f7cc),
        BorderStyle::default(),
    ));
}

/// Paint the horizontal whole-file preview strip into `bounds`, with the
/// visible-range band.
pub(crate) fn paint_strip(
    window: &mut Window,
    bounds: Bounds<Pixels>,
    image: &Arc<RenderImage>,
    file_size: usize,
    mark: Option<&Range<usize>>,
) {
    let _ = window.paint_image(bounds, Corners::all(px(2.)), image.clone(), 0, false);
    let Some(mark) = mark else { return };
    if file_size == 0 || mark.is_empty() {
        return;
    }
    // The strip maps x to file offset, so the band is vertical here.
    let w = bounds.size.width.to_f64() as f32;
    let frac = |off: usize| (off as f32 / file_size as f32).clamp(0.0, 1.0);
    let x0 = bounds.left().to_f64() as f32 + frac(mark.start) * w;
    let x1 = (bounds.left().to_f64() as f32 + frac(mark.end) * w).max(x0 + 2.0);
    let band = Bounds::from_corners(point(px(x0), bounds.top()), point(px(x1), bounds.bottom()));
    window.paint_quad(filled_quad(band, rgba(0xffffff2e)));
}

// ---------------------------------------------------------------------------
// Thumbnail image generation
// ---------------------------------------------------------------------------

/// Average byte value over `[start, end)`, sampled at a few points (a
/// thumbnail cell can cover many bytes).
fn sample_average(data: &[u8], start: usize, end: usize) -> u8 {
    const SAMPLES: usize = 8;
    let mut sum = 0u32;
    for k in 0..SAMPLES {
        let off = (start + (end - start) * k / SAMPLES).min(data.len() - 1);
        sum += u32::from(data[off]);
    }
    (sum / SAMPLES as u32) as u8
}

fn set_pixel(buf: &mut [u8], width: usize, x: usize, y: usize, c: Rgba) {
    let p = (y * width + x) * 4;
    buf[p] = (c.r * 255.0) as u8;
    buf[p + 1] = (c.g * 255.0) as u8;
    buf[p + 2] = (c.b * 255.0) as u8;
    buf[p + 3] = (c.a * 255.0) as u8;
}

/// Wrap a raw RGBA buffer as a gpui `RenderImage` (via the `image` crate's
/// frame type, which gpui uses internally).
pub(crate) fn render_image_from_rgba(
    width: usize,
    height: usize,
    rgba: Vec<u8>,
) -> Arc<RenderImage> {
    let buf =
        image::RgbaImage::from_raw(width as u32, height as u32, rgba).expect("rgba buffer size");
    let frame = image::Frame::new(buf);
    Arc::new(RenderImage::new(vec![frame]))
}

/// Compute the raw RGBA pixels (`w` × `h`) of the 2D whole-file overview: one
/// band per cell in `colormap`. The pixel math is unit-testable without a gpui
/// window, and the app runs it on the background executor so the UI thread never
/// blocks on a whole-file pass.
///
/// Under `Colormap::None` every cell is left transparent, so the panel
/// background shows through — `None` mutes a panel rather than disabling it, and
/// the viewport band, hover preview and click-to-navigate all stay live.
pub(crate) fn build_overview_rgba(
    data: &[u8],
    entropies: &[f32],
    entropy_window: usize,
    w: usize,
    h: usize,
    colormap: Colormap,
) -> Vec<u8> {
    // `sample_average` needs at least one byte (it indexes `len - 1`); an
    // empty buffer is the safe placeholder for a missing file. Zero dimensions
    // would divide by zero below (`k % w`), so floor them at 1.
    let w = w.max(1);
    let h = h.max(1);
    if data.is_empty() {
        return vec![0u8; w * h * 4];
    }
    let len = data.len();
    let cells = (w * h).max(1);
    let mut pixels = vec![0u8; w * h * 4];
    for k in 0..cells {
        let start = k * len / cells;
        let end = ((k + 1) * len / cells).max(start + 1);
        let mid = (start + (end - start) / 2).min(len - 1);
        let avg = sample_average(data, start, end);
        // Cell k sits at grid (col = k % w, row = k / w).
        if let Some(c) =
            colormap.color_for(avg, entropy_for(colormap, entropies, entropy_window, mid))
        {
            set_pixel(&mut pixels, w, k % w, k / w, c);
        }
    }
    pixels
}

/// Columns in the horizontal whole-file preview strip.
pub(crate) const STRIP_CELLS: usize = 256;

/// Build the horizontal whole-file preview strip: a fixed `STRIP_CELLS`×1 band
/// in `colormap`, x mapping to file offset.
///
/// A single row of the overview *is* the strip — `build_overview_rgba` lays its
/// cells out row-major, so at `h = 1` every cell is one column and the two
/// generators agree by construction rather than by keeping two copies of the
/// downsampling math in step.
pub(crate) fn build_strip_image(
    data: &[u8],
    entropies: &[f32],
    entropy_window: usize,
    colormap: Colormap,
) -> Arc<RenderImage> {
    let pixels = build_overview_rgba(data, entropies, entropy_window, STRIP_CELLS, 1, colormap);
    render_image_from_rgba(STRIP_CELLS, 1, pixels)
}

/// Raw RGBA pixels of the zoom column's visible region: `rows` rows of `bpr`
/// bytes starting at `first_row_start`, each byte a `block × block` pixel
/// square quantized to the integer pixel grid (blocks need not divide
/// evenly into pixels). Returns `(pixels, iw, ih)` — the buffer is `iw × ih`
/// with `iw = ceil(bpr·block)`, `ih = ceil(rows·block)`, so `paint_image`
/// scales it ~1:1 into the panel and no smoothing is needed.
///
/// Under `Colormap::None` every pixel stays transparent, so the panel
/// background shows through; the panel stays interactive either way.
#[allow(clippy::too_many_arguments)]
pub(crate) fn build_zoom_rgba(
    data: &[u8],
    entropies: &[f32],
    entropy_window: usize,
    bpr: usize,
    first_row_start: usize,
    rows: usize,
    block: f32,
    colormap: Colormap,
) -> (Vec<u8>, usize, usize) {
    let len = data.len();
    if len == 0 || bpr == 0 || rows == 0 || !block.is_finite() || block <= 0.0 {
        return (Vec::new(), 0, 0);
    }
    let iw = ((bpr as f32 * block).ceil() as usize).max(1);
    let ih = ((rows as f32 * block).ceil() as usize).max(1);
    let mut pixels = vec![0u8; iw * ih * 4];

    // Row y-ranges from the quantized grid, then disjoint mutable slices so
    // the fill can run in parallel over rows under rayon.
    let row_ys: Vec<(usize, usize)> = (0..rows)
        .map(|r| {
            let y0 = (r as f32 * block).round() as usize;
            let y1 = ((r + 1) as f32 * block).round() as usize;
            (y0, y1)
        })
        .collect();
    let mut slices: Vec<(usize, &mut [u8])> = Vec::with_capacity(rows);
    let mut rest = pixels.as_mut_slice();
    for (r, &(y0, y1)) in row_ys.iter().enumerate() {
        let span = (y1.saturating_sub(y0)) * iw * 4;
        let (head, tail) = rest.split_at_mut(span);
        slices.push((r, head));
        rest = tail;
    }

    let use_entropy = colormap.uses_entropy();
    slices.into_par_iter().for_each(|(r, buf)| {
        let row_start = first_row_start + r * bpr;
        if row_start >= len {
            return;
        }
        let n = (len - row_start).min(bpr);
        // `buf` spans exactly this row's y-range, so columns index from 0 and
        // its height (in pixel rows) is `y1 - y0`.
        let (y0, y1) = row_ys[r];
        let h = y1 - y0;
        for i in 0..n {
            let off = row_start + i;
            let e = if use_entropy {
                entropy_at(entropies, entropy_window, off)
            } else {
                0.0
            };
            let Some(c) = colormap.color_for(data[off], e) else {
                continue;
            };
            let x0 = (i as f32 * block).round() as usize;
            let x1 = ((i + 1) as f32 * block).round() as usize;
            let (r8, g8, b8, a8) = (
                (c.r * 255.0) as u8,
                (c.g * 255.0) as u8,
                (c.b * 255.0) as u8,
                (c.a * 255.0) as u8,
            );
            // Fill every pixel row of the block's column span.
            for row in buf.chunks_exact_mut(iw * 4).take(h) {
                for px in row[x0 * 4..x1 * 4].chunks_exact_mut(4) {
                    px[0] = r8;
                    px[1] = g8;
                    px[2] = b8;
                    px[3] = a8;
                }
            }
        }
    });
    (pixels, iw, ih)
}

/// Build the zoom column's visible-region texture. Wraps `build_zoom_rgba`
/// so the pixel math is unit-testable without a gpui window.
#[allow(clippy::too_many_arguments)]
pub(crate) fn build_zoom_image(
    data: &[u8],
    entropies: &[f32],
    entropy_window: usize,
    bpr: usize,
    first_row_start: usize,
    rows: usize,
    block: f32,
    colormap: Colormap,
) -> Option<Arc<RenderImage>> {
    let (pixels, iw, ih) = build_zoom_rgba(
        data,
        entropies,
        entropy_window,
        bpr,
        first_row_start,
        rows,
        block,
        colormap,
    );
    (iw > 0 && ih > 0).then(|| render_image_from_rgba(iw, ih, pixels))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Read the RGBA pixel at `(x, y)` from a raw buffer with the given width.
    fn px(buf: &[u8], width: usize, x: usize, y: usize) -> (u8, u8, u8, u8) {
        let p = (y * width + x) * 4;
        (buf[p], buf[p + 1], buf[p + 2], buf[p + 3])
    }

    /// Per-window Shannon entropies for `data`, as handed to the generators.
    fn entropies(data: &[u8]) -> Vec<f32> {
        crate::entropy::block_entropies(data, 256)
    }

    /// The strip's raw pixels: one row of the overview, exactly as
    /// `build_strip_image` builds it.
    fn strip_rgba(
        data: &[u8],
        entropies: &[f32],
        entropy_window: usize,
        colormap: Colormap,
    ) -> Vec<u8> {
        build_overview_rgba(data, entropies, entropy_window, STRIP_CELLS, 1, colormap)
    }

    #[test]
    fn overview_buffer_is_w_by_h_and_opaque() {
        let data = [0x41u8; 4096];
        let e = entropies(&data);
        let buf = build_overview_rgba(&data, &e, 256, 8, 4, Colormap::Value);
        assert_eq!(buf.len(), 8 * 4 * 4); // w × h × 4 channels, one band per cell
        for (i, &b) in buf.iter().enumerate() {
            if i % 4 == 3 {
                assert_eq!(b, 255, "alpha at byte {i} must be opaque");
            }
        }
    }

    #[test]
    fn overview_value_colormap_is_byte_brightness() {
        let data = [0xAAu8; 512];
        let e = entropies(&data);
        let buf = build_overview_rgba(&data, &e, 256, 2, 2, Colormap::Value);
        for (x, y) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
            assert_eq!(px(&buf, 2, x, y), (170, 170, 170, 255), "cell ({x},{y})");
        }
    }

    #[test]
    fn overview_none_colormap_leaves_cells_transparent() {
        let data = [0xAAu8; 512];
        let e = entropies(&data);
        let buf = build_overview_rgba(&data, &e, 256, 2, 2, Colormap::None);
        assert!(buf.iter().all(|&b| b == 0), "None must paint nothing");
    }

    #[test]
    fn overview_cells_tile_the_file_in_row_major_order() {
        // Four cells over four bytes: cell k is byte k, so row-major order is
        // directly visible in the buffer.
        let data = vec![0x00u8, 0x40, 0x80, 0xC0];
        let e = entropies(&data);
        let buf = build_overview_rgba(&data, &e, 256, 2, 2, Colormap::Value);
        assert_eq!(px(&buf, 2, 0, 0).0, 0x00);
        assert_eq!(px(&buf, 2, 1, 0).0, 0x40);
        assert_eq!(px(&buf, 2, 0, 1).0, 0x80);
        assert_eq!(px(&buf, 2, 1, 1).0, 0xC0);
    }

    #[test]
    fn overview_entropy_colormap_is_hot_for_full_range_bytes() {
        // One full 0..=255 cycle has entropy 8.0 — the hot end of the gradient.
        let data: Vec<u8> = (0..=255u8).cycle().take(256).collect();
        let e = entropies(&data);
        assert!((e[0] - 8.0).abs() < 0.01, "e={}", e[0]);
        let buf = build_overview_rgba(&data, &e, 256, 1, 1, Colormap::Entropy);
        assert_eq!(px(&buf, 1, 0, 0), (255, 60, 40, 255)); // entropy_color(8.0)
    }

    #[test]
    fn strip_buffer_is_256x1_and_opaque() {
        let data = [0u8; 512];
        let e = entropies(&data);
        let buf = strip_rgba(&data, &e, 256, Colormap::Value);
        assert_eq!(buf.len(), 256 * 4);
        for (i, &b) in buf.iter().enumerate() {
            if i % 4 == 3 {
                assert_eq!(b, 255, "alpha at byte {i} must be opaque");
            }
        }
    }

    #[test]
    fn strip_maps_file_offset_to_x() {
        // 512 bytes over 256 columns -> 2 bytes each; left half 0xFF, right 0x00.
        let data: Vec<u8> = [vec![0xFF; 256], vec![0x00; 256]].concat();
        let e = entropies(&data);
        let buf = strip_rgba(&data, &e, 256, Colormap::Value);
        assert_eq!(px(&buf, 256, 0, 0), (255, 255, 255, 255));
        assert_eq!(px(&buf, 256, 127, 0), (255, 255, 255, 255));
        assert_eq!(px(&buf, 256, 128, 0), (0, 0, 0, 255));
        assert_eq!(px(&buf, 256, 255, 0), (0, 0, 0, 255));
    }

    #[test]
    fn strip_handles_a_single_byte_file() {
        // Every strip column maps back to the one byte.
        let data = [0xABu8; 1];
        let e = entropies(&data);
        let buf = strip_rgba(&data, &e, 256, Colormap::Value);
        assert_eq!(buf.len(), 256 * 4);
        assert_eq!(px(&buf, 256, 0, 0), (171, 171, 171, 255));
        assert_eq!(px(&buf, 256, 200, 0), (171, 171, 171, 255));
    }

    #[test]
    fn empty_data_yields_an_empty_transparent_buffer() {
        // The app never builds thumbnails without a file, but the generators
        // should not panic (sample_average indexes len - 1) if handed one.
        assert!(
            build_overview_rgba(&[], &[], 256, 4, 2, Colormap::Value)
                .iter()
                .all(|&b| b == 0)
        );
        assert!(
            strip_rgba(&[], &[], 256, Colormap::Value)
                .iter()
                .all(|&b| b == 0)
        );
    }

    #[test]
    fn overview_handles_zero_dimensions() {
        // A zero width/height must not panic (k % w would divide by zero and
        // the image crate rejects 0-sized buffers); the dimensions floor at 1.
        let data = [0x41u8; 64];
        // w → 1, so 1 × 4 × 4 channels = 16 bytes; h → 1, so 3 × 1 × 4 = 12.
        assert_eq!(
            build_overview_rgba(&data, &[], 256, 0, 4, Colormap::Value).len(),
            16
        );
        assert_eq!(
            build_overview_rgba(&data, &[], 256, 3, 0, Colormap::Value).len(),
            12
        );
        let buf = build_overview_rgba(&data, &[], 256, 0, 2, Colormap::Value);
        assert_eq!(buf.len(), 2 * 4);
        let _img = render_image_from_rgba(1, 2, buf);
    }

    #[test]
    fn thumbnail_wrappers_build_valid_images() {
        let data = [0x41u8; 4096];
        let e = entropies(&data);
        // Routing the RGBA buffer through the image crate panics on a
        // buffer-size mismatch — so simply constructing the image here
        // verifies the pixel-buffer invariants end to end.
        let buf = build_overview_rgba(&data, &e, 256, 3, 2, Colormap::Value);
        assert_eq!(buf.len(), 3 * 2 * 4);
        let _img = render_image_from_rgba(3, 2, buf);
        let _strip = build_strip_image(&data, &e, 256, Colormap::Value);
    }

    /// The test harness binary is itself an ELF. Feed a genuine ELF through
    /// both thumbnail generators and check they produce valid, non-trivial
    /// output — the whole-file previews must still generate for real binaries
    /// (mixed code/data/sections), not just synthetic buffers.
    #[test]
    fn thumbnails_generate_for_a_real_elf() {
        let exe = std::env::current_exe().expect("path to the test binary");
        let data = std::fs::read(&exe).expect("read the test binary");
        assert!(
            data.starts_with(b"\x7fELF"),
            "expected the test binary to be an ELF (magic: {:02X?})",
            &data[..data.len().min(4)]
        );

        let e = entropies(&data);

        // A real ELF has real structure: zeroed regions (~0 bits/byte) and
        // code/data (several bits/byte), so per-block entropy must vary.
        let (mut lo, mut hi) = (f32::MAX, f32::MIN);
        for &v in &e {
            lo = lo.min(v);
            hi = hi.max(v);
        }
        assert!(
            hi - lo > 1.0,
            "real ELF entropy should vary (min={lo:.2} max={hi:.2})"
        );

        // Strip: valid 256×2 buffer, fully opaque, with genuine content.
        let strip = strip_rgba(&data, &e, 256, Colormap::Value);
        assert_eq!(strip.len(), 256 * 4);
        assert!(strip.iter().skip(3).step_by(4).all(|&a| a == 255));
        let distinct: std::collections::HashSet<[u8; 4]> = strip
            .chunks_exact(4)
            .map(|c| [c[0], c[1], c[2], c[3]])
            .collect();
        assert!(
            distinct.len() > 4,
            "strip should show real content, not a flat color ({} colors)",
            distinct.len()
        );

        // Overview: valid buffer for a small grid; the image wrapper builds
        // (it runs the buffer through the image crate's size checks).
        let (w, h) = (16usize, 8usize);
        let overview = build_overview_rgba(&data, &e, 256, w, h, Colormap::Value);
        assert_eq!(overview.len(), w * h * 4);
        assert!(overview.iter().skip(3).step_by(4).all(|&a| a == 255));
        let _img = render_image_from_rgba(w, h, overview);
        let _strip = build_strip_image(&data, &e, 256, Colormap::Value);
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
        let hit = |x: f32, y: f32, first: usize| {
            zoom_offset_at(point(gpui::px(x), gpui::px(y)), 16, first, 4.0, 60)
        };

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
        assert_eq!(
            zoom_offset_at(point(gpui::px(0.), gpui::px(0.)), 16, 0, 4.0, 0),
            None
        );
        assert_eq!(
            zoom_offset_at(point(gpui::px(0.), gpui::px(0.)), 16, 0, 0.0, 60),
            None
        );
    }

    #[test]
    fn hex_offset_at_maps_y_to_row_and_x_to_byte() {
        let geo = row_geo();
        assert_eq!(BLOCK_H, 21.0);
        let len = 64usize;
        let hit = |y: f32, x: f32| hex_offset_at(point(gpui::px(x), gpui::px(y)), &geo, 0, len);

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
        let hit = |y: f32, x: f32| hex_offset_at(point(gpui::px(x), gpui::px(y)), &geo, 32, len);

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

    /// The zoom texture with `block` whole pixels: 4 bytes/row × 2 rows of
    /// distinct bytes -> a 16×8 buffer where byte k occupies a 4×4 block.
    #[test]
    fn zoom_buffer_is_a_per_byte_pixel_grid() {
        let data = vec![0x00u8, 0x40, 0x80, 0xC0, 0x20, 0x60, 0xA0, 0xE0];
        let e = entropies(&data);
        let (buf, iw, ih) = build_zoom_rgba(&data, &e, 256, 4, 0, 2, 4.0, Colormap::Value);
        assert_eq!((iw, ih), (16, 8)); // ceil(4·4) × ceil(2·4)
        assert_eq!(buf.len(), 16 * 8 * 4);
        // Each byte is a solid 4×4 block at (row, col) = (k / 4, k % 4).
        for (k, &b) in data.iter().enumerate() {
            let (row, col) = (k / 4, k % 4);
            let expected = (b, b, b, 255);
            // Corners and centre of the block all carry the byte's color.
            for (dx, dy) in [(0, 0), (1, 1), (3, 0), (2, 3)] {
                assert_eq!(
                    px(&buf, iw, col * 4 + dx, row * 4 + dy),
                    expected,
                    "byte {k} at block corner ({dx},{dy})"
                );
            }
        }
    }

    #[test]
    fn zoom_buffer_anchors_at_first_row_start() {
        // 2 bytes/row anchored at byte 8: the texture shows bytes 8,9 then
        // 10,11 — not bytes 0..4.
        let data: Vec<u8> = (0..12).map(|i| i * 8).collect();
        let e = entropies(&data);
        let (buf, iw, _ih) = build_zoom_rgba(&data, &e, 256, 2, 8, 2, 4.0, Colormap::Value);
        assert_eq!((buf[0], buf[1]), (8 * 8, 8 * 8)); // byte 8
        assert_eq!((buf[4 * 4], buf[4 * 4 + 1]), (9 * 8, 9 * 8)); // byte 9
        // Block row 1 spans pixel rows 4..8; it starts at byte 10.
        assert_eq!(px(&buf, iw, 0, 4).0, 10 * 8);
    }

    #[test]
    fn zoom_none_colormap_leaves_the_texture_transparent() {
        let data = [0xAAu8; 16];
        let e = entropies(&data);
        let (buf, iw, ih) = build_zoom_rgba(&data, &e, 256, 4, 0, 2, 4.0, Colormap::None);
        assert_eq!(buf.len(), iw * ih * 4);
        assert!(buf.iter().all(|&b| b == 0), "None must paint nothing");
    }

    #[test]
    fn zoom_quantizes_fractional_blocks_to_the_pixel_grid() {
        // 5 bytes/row over a 19 px panel: block = 3.8 px. Every pixel column
        // still resolves to a byte and the buffer spans the full panel width.
        let data = [0x00u8, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF];
        let e = entropies(&data);
        let (buf, iw, ih) = build_zoom_rgba(&data, &e, 256, 5, 0, 2, 3.8, Colormap::Value);
        assert_eq!((iw, ih), (19, 8)); // ceil(5·3.8) × ceil(2·3.8)
        // No pixel is transparent: every column is covered by a byte block.
        for y in 0..ih {
            for x in 0..iw {
                assert_eq!(px(&buf, iw, x, y).3, 255, "pixel ({x},{y})");
            }
        }
    }

    #[test]
    fn zoom_buffer_handles_partial_last_row_and_empty_data() {
        // 8 bytes, 6/row: the second row holds only 2 bytes; columns 2..6 of
        // that row stay transparent (paint_image leaves them as panel bg).
        let data: Vec<u8> = (0..8).map(|i| i * 16).collect();
        let e = entropies(&data);
        let (buf, iw, ih) = build_zoom_rgba(&data, &e, 256, 6, 0, 2, 4.0, Colormap::Value);
        assert_eq!((iw, ih), (24, 8));
        // Block row 1 spans pixel rows 4..8 and holds bytes 6 and 7.
        assert_eq!(px(&buf, iw, 0, 4).0, 96); // byte 6 (0x60)
        assert_eq!(px(&buf, iw, 4, 4).0, 112); // byte 7 (0x70)
        assert_eq!(px(&buf, iw, 12, 4).3, 0); // past the last byte: transparent
        // Degenerate inputs never panic and yield an empty buffer.
        let (buf, iw, ih) = build_zoom_rgba(&[], &[], 256, 4, 0, 2, 4.0, Colormap::Value);
        assert_eq!((buf.len(), iw, ih), (0, 0, 0));
        let (buf, iw, ih) = build_zoom_rgba(&data, &e, 256, 0, 0, 2, 4.0, Colormap::Value);
        assert_eq!((buf.len(), iw, ih), (0, 0, 0));
        let (buf, iw, ih) = build_zoom_rgba(&data, &e, 256, 6, 0, 0, 4.0, Colormap::Value);
        assert_eq!((buf.len(), iw, ih), (0, 0, 0));
    }

    #[test]
    fn zoom_image_wrapper_builds_a_valid_texture() {
        let data: Vec<u8> = (0..32).map(|i| i as u8).collect();
        let e = entropies(&data);
        let img = build_zoom_image(&data, &e, 256, 8, 0, 2, 4.0, Colormap::Class);
        assert!(img.is_some());
        let img = build_zoom_image(&[], &e, 256, 8, 0, 2, 4.0, Colormap::Class);
        assert!(img.is_none());
    }
}
