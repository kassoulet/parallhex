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

use gpui::{
    App, Background, BorderStyle, Bounds, Corners, Font, Hsla, Pixels, Point, RenderImage, Rgba,
    ShapedLine, TextRun, Window, font, point, px, quad, rgb, rgba, size, transparent_black,
};

use crate::color::{self, Colormap};

pub(crate) const HEX_ZOOM_DEFAULT: f32 = 1.0;
pub(crate) const HEX_ZOOM_MIN: f32 = 0.5;
pub(crate) const HEX_ZOOM_MAX: f32 = 4.0;
pub(crate) const PIXEL_ZOOM_DEFAULT: f32 = 4.0;
pub(crate) const PIXEL_ZOOM_MIN: f32 = 1.0;
pub(crate) const PIXEL_ZOOM_MAX: f32 = 24.0;

/// Keyboard zoom step factor (`+` / `-`), applied multiplicatively per press.
pub(crate) const ZOOM_STEP: f32 = 1.25;

/// Hex row geometry at zoom 1.0 (pixels).
pub(crate) const ROW_H: f32 = 18.0;
pub(crate) const ROW_GAP: f32 = 3.0;
/// Base monospace size for hex cells at zoom 1.0.
pub(crate) const HEX_FONT_SIZE: f32 = 13.0;
/// Left padding for the address gutter.
pub(crate) const ADDR_X: f32 = 8.0;

/// Apply a multiplicative zoom step, clamped to `[min, max]`. Shared by the
/// Ctrl+wheel handlers and the `+`/`-` keyboard shortcuts.
pub(crate) fn zoom_step(zoom: f32, factor: f32, min: f32, max: f32) -> f32 {
    (zoom * factor).clamp(min, max)
}

/// Height of one hex row at zoom `zoom` (1.0 = default).
pub(crate) fn hex_row_h(zoom: f32) -> f32 {
    ROW_H * zoom
}

/// Height of one hex row including its separator gap.
pub(crate) fn hex_block_h(zoom: f32) -> f32 {
    hex_row_h(zoom) + ROW_GAP
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
        let group_gap = char_w; // extra space every 8 bytes
        let hex_w = bpr as f32 * cell_w + (bpr / 8) as f32 * group_gap;
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
/// content or over a gap. `local` is relative to the hex canvas origin.
pub(crate) fn hex_offset_at(
    local: Point<Pixels>,
    geo: &RowGeo,
    scroll_rows: f32,
    block_h: f32,
    total_rows: usize,
    len: usize,
) -> Option<usize> {
    let y = local.y.to_f64() as f32;
    if y < 0.0 {
        return None;
    }
    let row = ((y / block_h).floor() + scroll_rows) as usize;
    if row >= total_rows {
        return None;
    }
    let row_start = row * geo.bpr;
    if row_start >= len {
        return None;
    }
    let i = geo.byte_at_x(local.x.to_f64() as f32)?;
    let off = row_start + i;
    (off < len).then_some(off)
}

// ---------------------------------------------------------------------------
// Text building for a hex row
// ---------------------------------------------------------------------------

/// The assembled row line plus the char offsets of each byte's hex digits and
/// ASCII glyph. Offsets are UTF-8 byte indices into `text`.
pub(crate) struct RowText {
    pub text: String,
    pub hex_offsets: Vec<usize>,
    pub ascii_offsets: Vec<usize>,
}

/// Build the display text of one hex row:
/// `ADDR  HH HH …  AAAA` with an extra space every 8 hex bytes.
pub(crate) fn build_row_text(data: &[u8], row_start: usize, n: usize) -> RowText {
    let mut text = format!("{row_start:08X}  ");
    let mut hex_offsets = Vec::with_capacity(n);
    let mut ascii_offsets = Vec::with_capacity(n);
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
    RowText {
        text,
        hex_offsets,
        ascii_offsets,
    }
}

/// Build the color runs for a row line: gray address, contrast-colored hex
/// digits and ASCII glyphs per byte, neutral (invisible) spaces.
fn build_row_runs(
    data: &[u8],
    row_start: usize,
    n: usize,
    hex_offsets: &[usize],
    ascii_offsets: &[usize],
    font: &Font,
    total_len: usize,
) -> Vec<TextRun> {
    let neutral = to_hsla(rgba(0x00000000));
    let addr_color = to_hsla(rgb(0x9a9a9a));
    let mut runs = Vec::new();
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
        gap(cur, hex_offsets[i], &mut runs);
        cur = hex_offsets[i];
        let fg = to_hsla(color::fg_for_class(color::class_color(data[row_start + i])));
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
        gap(cur, ascii_offsets[0], &mut runs);
        cur = ascii_offsets[0];
    }

    // ASCII cells.
    for i in 0..n {
        let fg = to_hsla(color::fg_for_class(color::class_color(data[row_start + i])));
        runs.push(TextRun {
            len: 1,
            font: font.clone(),
            color: fg,
            background_color: None,
            underline: None,
            strikethrough: None,
        });
        cur += 1;
    }

    gap(cur, total_len, &mut runs);
    runs
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

/// Paint the hex column into `bounds`: class-colored hex + ASCII cells with
/// selection/hover overlays, one virtualized row at a time.
#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
pub(crate) fn paint_hex(
    window: &mut Window,
    cx: &mut App,
    bounds: Bounds<Pixels>,
    data: &[u8],
    font: &Font,
    zoom: f32,
    bpr: usize,
    scroll_rows: f32,
    hovered: Option<usize>,
    sel: Option<&Range<usize>>,
) {
    let len = data.len();
    if len == 0 {
        return;
    }
    let total_rows = len.div_ceil(bpr);
    let row_h = hex_row_h(zoom);
    let block_h = hex_block_h(zoom);
    let font_size = px(HEX_FONT_SIZE * zoom);
    let char_w = hex_char_width(window, font, font_size);
    let geo = RowGeo::new(char_w, bpr);

    let first = scroll_rows.floor().max(0.0) as usize;
    let vis_rows = (bounds.size.height / px(block_h)).ceil() as usize + 1;
    let last = (first + vis_rows).min(total_rows);

    let origin = bounds.origin;
    for row in first..last {
        let y0 = (row as f32 - scroll_rows) * block_h;
        let row_start = row * bpr;
        let n = (len - row_start).min(bpr);

        // Cell backgrounds (+ selection overlay).
        for i in 0..n {
            let off = row_start + i;
            let class: Background = color::class_color(data[off]).into();
            let hex_x = geo.cell_x(i);
            let hex_rect = Bounds::new(
                point(origin.x + px(hex_x), origin.y + px(y0)),
                size(px(geo.cell_w), px(row_h)),
            );
            window.paint_quad(quad(
                hex_rect,
                px(0.),
                class,
                px(0.),
                transparent_black(),
                BorderStyle::default(),
            ));
            let ascii_x = geo.ascii_x(i);
            let ascii_rect = Bounds::new(
                point(origin.x + px(ascii_x), origin.y + px(y0)),
                size(px(geo.char_w), px(row_h)),
            );
            window.paint_quad(quad(
                ascii_rect,
                px(0.),
                class,
                px(0.),
                transparent_black(),
                BorderStyle::default(),
            ));
            if sel.is_some_and(|r| r.contains(&off)) {
                let tint: Background = rgba(0xffffff3d).into();
                window.paint_quad(quad(
                    hex_rect,
                    px(0.),
                    tint,
                    px(0.),
                    transparent_black(),
                    BorderStyle::default(),
                ));
                window.paint_quad(quad(
                    ascii_rect,
                    px(0.),
                    tint,
                    px(0.),
                    transparent_black(),
                    BorderStyle::default(),
                ));
            }
        }

        // Hover outline across hex + ascii cells.
        if let Some(o) = hovered
            && (row_start..row_start + n).contains(&o)
        {
            let i = o - row_start;
            let x0 = geo.cell_x(i);
            let rect = Bounds::new(
                point(origin.x + px(x0), origin.y + px(y0)),
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
        let rt = build_row_text(data, row_start, n);
        let runs = build_row_runs(
            data,
            row_start,
            n,
            &rt.hex_offsets,
            &rt.ascii_offsets,
            font,
            rt.text.len(),
        );
        let line = window
            .text_system()
            .shape_line(rt.text.into(), font_size, &runs, None);
        let _ = line.paint(point(origin.x, origin.y + px(y0)), px(row_h), window, cx);
    }
}

/// Paint the pixels column into `bounds`: a per-byte band in the selected
/// colormap over an entropy band, at an adjustable zoom. Virtualized.
#[allow(clippy::too_many_arguments)]
pub(crate) fn paint_pixels(
    window: &mut Window,
    _cx: &mut App,
    bounds: Bounds<Pixels>,
    data: &[u8],
    bpr: usize,
    scroll_rows: f32,
    pixel_zoom: f32,
    hovered: Option<usize>,
    sel: Option<&Range<usize>>,
    entropies: &[f32],
    entropy_window: usize,
    colormap: Colormap,
) {
    let len = data.len();
    if len == 0 {
        return;
    }
    let total_rows = len.div_ceil(bpr);
    let px_size = pixel_zoom.clamp(PIXEL_ZOOM_MIN, PIXEL_ZOOM_MAX);
    let band_h = px_size;
    let row_h = 2.0 * band_h + 1.0;

    let first = scroll_rows.floor().max(0.0) as usize;
    let vis_rows = (bounds.size.height / px(row_h)).ceil() as usize + 1;
    let last = (first + vis_rows).min(total_rows);

    for row in first..last {
        let y = bounds.top().to_f64() as f32 + (row as f32 - scroll_rows) * row_h;
        let row_start = row * bpr;
        let n = (len - row_start).min(bpr);
        for i in 0..n {
            let off = row_start + i;
            let b = data[off];
            let x = bounds.left() + px(i as f32 * px_size);
            let grey_rect = Bounds::new(point(x, px(y)), size(px(px_size), px(band_h)));
            let top: Background = colormap
                .color_for(b, entropy_at(entropies, entropy_window, off))
                .into();
            window.paint_quad(quad(
                grey_rect,
                px(0.),
                top,
                px(0.),
                transparent_black(),
                BorderStyle::default(),
            ));
            let entr_rect = Bounds::new(point(x, px(y + band_h)), size(px(px_size), px(band_h)));
            let bot: Background =
                color::entropy_color(entropy_at(entropies, entropy_window, off)).into();
            window.paint_quad(quad(
                entr_rect,
                px(0.),
                bot,
                px(0.),
                transparent_black(),
                BorderStyle::default(),
            ));
            if sel.is_some_and(|r| r.contains(&off)) {
                let tint: Background = rgba(0xffffff30).into();
                window.paint_quad(quad(
                    grey_rect,
                    px(0.),
                    tint,
                    px(0.),
                    transparent_black(),
                    BorderStyle::default(),
                ));
                window.paint_quad(quad(
                    entr_rect,
                    px(0.),
                    tint,
                    px(0.),
                    transparent_black(),
                    BorderStyle::default(),
                ));
            }
        }
        if let Some(o) = hovered
            && (row_start..row_start + n).contains(&o)
        {
            let i = o - row_start;
            let x = bounds.left() + px(i as f32 * px_size);
            let rect = Bounds::new(point(x, px(y)), size(px(px_size), px(row_h)));
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
}

/// Paint the whole-file 2D overview into `bounds`: the greyscale/entropy
/// thumbnail with a translucent band marking the visible byte range.
pub(crate) fn paint_overview(
    window: &mut Window,
    _cx: &mut App,
    bounds: Bounds<Pixels>,
    image: &Arc<RenderImage>,
    file_size: usize,
    view_frac: f32,
    view_frac_h: f32,
) {
    let _ = window.paint_image(bounds, Corners::all(px(2.)), image.clone(), 0, false);
    if file_size == 0 {
        return;
    }
    let x0 = bounds.left().to_f64() as f32
        + view_frac.clamp(0.0, 1.0) * bounds.size.width.to_f64() as f32;
    let x1 = bounds.left().to_f64() as f32
        + (view_frac + view_frac_h).clamp(0.0, 1.0) * bounds.size.width.to_f64() as f32;
    let band = Bounds::from_corners(point(px(x0), bounds.top()), point(px(x1), bounds.bottom()));
    window.paint_quad(quad(
        band,
        px(0.),
        rgba(0xffffff2e),
        px(0.),
        transparent_black(),
        BorderStyle::default(),
    ));
}

/// Paint the horizontal whole-file preview strip into `bounds`, with the
/// visible-range band.
pub(crate) fn paint_strip(
    window: &mut Window,
    _cx: &mut App,
    bounds: Bounds<Pixels>,
    image: &Arc<RenderImage>,
    file_size: usize,
    view_frac: f32,
    view_frac_h: f32,
) {
    let _ = window.paint_image(bounds, Corners::all(px(2.)), image.clone(), 0, false);
    if file_size == 0 {
        return;
    }
    let x0 = bounds.left().to_f64() as f32
        + view_frac.clamp(0.0, 1.0) * bounds.size.width.to_f64() as f32;
    let x1 = bounds.left().to_f64() as f32
        + (view_frac + view_frac_h).clamp(0.0, 1.0) * bounds.size.width.to_f64() as f32;
    let band = Bounds::from_corners(point(px(x0), bounds.top()), point(px(x1), bounds.bottom()));
    window.paint_quad(quad(
        band,
        px(0.),
        rgba(0xffffff2e),
        px(0.),
        transparent_black(),
        BorderStyle::default(),
    ));
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

/// Compute the raw RGBA pixels (`w` × `2h`) of the 2D whole-file overview.
/// Extracted from `build_overview_image` so the pixel math is unit-testable
/// without a gpui window; the wrapper only wraps the buffer as an image.
fn build_overview_rgba(
    data: &[u8],
    entropies: &[f32],
    entropy_window: usize,
    w: usize,
    h: usize,
) -> Vec<u8> {
    // `sample_average` needs at least one byte (it indexes `len - 1`); an
    // empty buffer is the safe placeholder for a missing file. Zero
    // dimensions would divide by zero below (`k % w`), so floor them at 1.
    let w = w.max(1);
    let h = h.max(1);
    if data.is_empty() {
        return vec![0u8; w * 2 * h * 4];
    }
    let len = data.len();
    let cells = (w * h).max(1);
    let mut pixels = vec![0u8; w * 2 * h * 4];
    for k in 0..cells {
        let start = k * len / cells;
        let end = ((k + 1) * len / cells).max(start + 1);
        // Cell k sits at grid (col = k % w, row = k / w); each cell is a 1×2
        // block of pixels: a colormap band over an entropy band.
        let col = k % w;
        let row = 2 * (k / w);
        let avg = sample_average(data, start, end);
        set_pixel(
            &mut pixels,
            w,
            col,
            row,
            Colormap::Greyscale.color_for(avg, 0.0),
        );
        let mid = (start + (end - start) / 2).min(len - 1);
        set_pixel(
            &mut pixels,
            w,
            col,
            row + 1,
            color::entropy_color(entropy_at(entropies, entropy_window, mid)),
        );
    }
    pixels
}

/// Build the 2D whole-file overview: the file is downsampled into a `w × h`
/// cell grid, each cell drawn as a colormap band over an entropy band. The
/// texture is `w` wide and `2h` rows tall (two rows per cell).
pub(crate) fn build_overview_image(
    data: &[u8],
    entropies: &[f32],
    entropy_window: usize,
    w: usize,
    h: usize,
) -> (Arc<RenderImage>, (usize, usize)) {
    // Keep the reported cell grid consistent with the guarded buffer dims.
    let w = w.max(1);
    let h = h.max(1);
    let pixels = build_overview_rgba(data, entropies, entropy_window, w, h);
    (render_image_from_rgba(w, 2 * h, pixels), (w, h))
}

/// Compute the raw RGBA pixels (256×2) of the horizontal whole-file preview
/// strip. Extracted from `build_strip_image` for the same reason as
/// `build_overview_rgba`.
fn build_strip_rgba(data: &[u8], entropies: &[f32], entropy_window: usize) -> Vec<u8> {
    const W: usize = 256;
    if data.is_empty() {
        return vec![0u8; W * 2 * 4];
    }
    let len = data.len();
    let mut pixels = vec![0u8; W * 2 * 4];
    for x in 0..W {
        let start = x * len / W;
        let end = ((x + 1) * len / W).max(start + 1);
        let avg = sample_average(data, start, end);
        set_pixel(
            &mut pixels,
            W,
            x,
            0,
            Colormap::Greyscale.color_for(avg, 0.0),
        );
        let mid = (start + (end - start) / 2).min(len - 1);
        set_pixel(
            &mut pixels,
            W,
            x,
            1,
            color::entropy_color(entropy_at(entropies, entropy_window, mid)),
        );
    }
    pixels
}

/// Build the horizontal whole-file preview strip: a fixed 256×2 colormap /
/// entropy thumbnail, x mapping to file offset.
pub(crate) fn build_strip_image(
    data: &[u8],
    entropies: &[f32],
    entropy_window: usize,
) -> Arc<RenderImage> {
    let pixels = build_strip_rgba(data, entropies, entropy_window);
    render_image_from_rgba(256, 2, pixels)
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

    #[test]
    fn overview_buffer_is_w_by_2h_and_opaque() {
        let data = [0x41u8; 4096];
        let buf = build_overview_rgba(&data, &[], 256, 8, 4);
        assert_eq!(buf.len(), 8 * (2 * 4) * 4); // w × 2h × 4 channels
        for (i, &b) in buf.iter().enumerate() {
            if i % 4 == 3 {
                assert_eq!(b, 255, "alpha at byte {i} must be opaque");
            }
        }
    }

    #[test]
    fn overview_greyscale_band_is_byte_brightness() {
        let data = [0xAAu8; 512];
        let buf = build_overview_rgba(&data, &[], 256, 2, 2);
        // Every cell's top pixel (greyscale band) mirrors the average byte.
        assert_eq!(px(&buf, 2, 0, 0), (170, 170, 170, 255));
        assert_eq!(px(&buf, 2, 1, 0), (170, 170, 170, 255));
        assert_eq!(px(&buf, 2, 0, 2), (170, 170, 170, 255));
        assert_eq!(px(&buf, 2, 1, 2), (170, 170, 170, 255));
    }

    #[test]
    fn overview_cells_tile_the_file_in_row_major_order() {
        // First half of the file is 0xFF, second half 0x00. With w = 1, h = 2
        // the two cells split the file exactly in half: cell 0 (rows 0-1)
        // should be white over the low-entropy purple, cell 1 (rows 2-3)
        // black over the same purple.
        let data: Vec<u8> = [vec![0xFF; 256], vec![0x00; 256]].concat();
        let e = entropies(&data);
        let buf = build_overview_rgba(&data, &e, 256, 1, 2);
        assert_eq!(px(&buf, 1, 0, 0), (255, 255, 255, 255)); // cell 0 greyscale
        assert_eq!(px(&buf, 1, 0, 1), (12, 0, 40, 255)); // cell 0 entropy (uniform)
        assert_eq!(px(&buf, 1, 0, 2), (0, 0, 0, 255)); // cell 1 greyscale
        assert_eq!(px(&buf, 1, 0, 3), (12, 0, 40, 255)); // cell 1 entropy (uniform)
    }

    #[test]
    fn overview_entropy_band_high_for_full_range_bytes() {
        // One full 0..=255 cycle has entropy 8.0 (the hot end of the gradient)
        // and a mean byte of 112 on the greyscale band.
        let data: Vec<u8> = (0..=255u8).cycle().take(256).collect();
        let e = entropies(&data);
        assert!((e[0] - 8.0).abs() < 0.01, "e={}", e[0]);
        let buf = build_overview_rgba(&data, &e, 256, 1, 1);
        assert_eq!(px(&buf, 1, 0, 0), (112, 112, 112, 255));
        assert_eq!(px(&buf, 1, 0, 1), (255, 60, 40, 255)); // entropy_color(8.0)
    }

    #[test]
    fn strip_buffer_is_256x2_and_opaque() {
        let data = [0u8; 512];
        let buf = build_strip_rgba(&data, &[], 256);
        assert_eq!(buf.len(), 256 * 2 * 4);
        for (i, &b) in buf.iter().enumerate() {
            if i % 4 == 3 {
                assert_eq!(b, 255, "alpha at byte {i} must be opaque");
            }
        }
    }

    #[test]
    fn strip_maps_file_offset_to_x() {
        // 512 bytes over 256 strip columns -> 2 bytes per column. The left
        // half is 0xFF, the right half 0x00; each entropy block is uniform, so
        // the entropy band sits at the low end of the gradient.
        let data: Vec<u8> = [vec![0xFF; 256], vec![0x00; 256]].concat();
        let e = entropies(&data);
        let buf = build_strip_rgba(&data, &e, 256);
        assert_eq!(px(&buf, 256, 0, 0), (255, 255, 255, 255));
        assert_eq!(px(&buf, 256, 127, 0), (255, 255, 255, 255));
        assert_eq!(px(&buf, 256, 128, 0), (0, 0, 0, 255));
        assert_eq!(px(&buf, 256, 255, 0), (0, 0, 0, 255));
        assert_eq!(px(&buf, 256, 0, 1), (12, 0, 40, 255));
        assert_eq!(px(&buf, 256, 255, 1), (12, 0, 40, 255));
    }

    #[test]
    fn strip_handles_a_single_byte_file() {
        // Every strip column maps back to the one byte; a uniform 1-byte file
        // has entropy 0.
        let data = [0xABu8; 1];
        let e = entropies(&data);
        let buf = build_strip_rgba(&data, &e, 256);
        assert_eq!(buf.len(), 256 * 2 * 4);
        assert_eq!(px(&buf, 256, 0, 0), (171, 171, 171, 255));
        assert_eq!(px(&buf, 256, 200, 0), (171, 171, 171, 255));
        assert_eq!(px(&buf, 256, 0, 1), (12, 0, 40, 255)); // uniform -> entropy 0
    }

    #[test]
    fn empty_data_yields_an_empty_transparent_buffer() {
        // The app never builds thumbnails without a file, but the generators
        // should not panic (sample_average indexes len - 1) if handed one.
        assert!(
            build_overview_rgba(&[], &[], 256, 4, 2)
                .iter()
                .all(|&b| b == 0)
        );
        assert!(build_strip_rgba(&[], &[], 256).iter().all(|&b| b == 0));
    }

    #[test]
    fn overview_handles_zero_dimensions() {
        // A zero width/height must not panic (k % w would divide by zero and
        // the image crate rejects 0-sized buffers); the dimensions floor at 1.
        let data = [0x41u8; 64];
        // w → 1, so 1 × 2·4 × 4 channels = 32 bytes; h → 1, so 3 × 2·1 × 4 = 24.
        assert_eq!(build_overview_rgba(&data, &[], 256, 0, 4).len(), 32);
        assert_eq!(build_overview_rgba(&data, &[], 256, 3, 0).len(), 24);
        let (_image, cells) = build_overview_image(&data, &[], 256, 0, 2);
        assert_eq!(cells, (1, 2));
    }

    #[test]
    fn thumbnail_wrappers_build_valid_images() {
        let data = [0x41u8; 4096];
        let e = entropies(&data);
        // Both wrappers route the RGBA buffer through the image crate, which
        // panics on a buffer-size mismatch — so simply constructing them here
        // verifies the pixel-buffer invariants end to end.
        let (_image, cells) = build_overview_image(&data, &e, 256, 3, 2);
        assert_eq!(cells, (3, 2));
        let _strip = build_strip_image(&data, &e, 256);
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
        let strip = build_strip_rgba(&data, &e, 256);
        assert_eq!(strip.len(), 256 * 2 * 4);
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

        // Overview: valid buffer for a small grid; both image wrappers build
        // (they run the buffer through the image crate's size checks).
        let (w, h) = (16usize, 8usize);
        let overview = build_overview_rgba(&data, &e, 256, w, h);
        assert_eq!(overview.len(), w * (2 * h) * 4);
        assert!(overview.iter().skip(3).step_by(4).all(|&a| a == 255));
        let (_image, cells) = build_overview_image(&data, &e, 256, w, h);
        assert_eq!(cells, (w, h));
        let _strip = build_strip_image(&data, &e, 256);
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

    #[test]
    fn row_geo_byte_at_x_maps_cells_gaps_and_ascii() {
        let geo = row_geo();
        // Derived layout: addr gutter (8 + 10 glyphs), then hex cells.
        assert_eq!(geo.hex_start, 108.0);
        assert_eq!(geo.cell_w, 30.0);
        assert_eq!(geo.ascii_start, 628.0);

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
        assert_eq!(geo.byte_at_x(627.9), None);
        assert_eq!(geo.byte_at_x(628.0), Some(0));
        assert_eq!(geo.byte_at_x(643.0), Some(1));
        assert_eq!(geo.byte_at_x(geo.ascii_x(15) + geo.char_w), None); // past last byte
    }

    #[test]
    fn hex_offset_at_maps_y_to_row_and_x_to_byte() {
        let geo = row_geo();
        let block_h = hex_block_h(1.0); // 18 + 3 = 21 px per row
        assert_eq!(block_h, 21.0);
        let len = 64usize;
        let total_rows = len.div_ceil(geo.bpr);
        let hit = |y: f32, x: f32| {
            hex_offset_at(
                point(gpui::px(x), gpui::px(y)),
                &geo,
                0.0,
                block_h,
                total_rows,
                len,
            )
        };

        // Row 0 (offsets 0..16), both hex and ascii cells.
        assert_eq!(hit(0.0, 108.0), Some(0));
        assert_eq!(hit(0.0, 138.0), Some(1));
        assert_eq!(hit(10.0, 108.0), Some(0)); // still within the first row block
        assert_eq!(hit(0.0, 628.0), Some(0));
        // Row 1 starts at y = block_h.
        assert_eq!(hit(21.0, 108.0), Some(16));
        assert_eq!(hit(21.0, 628.0), Some(16));
        // Last full row (offsets 48..64).
        assert_eq!(hit(63.0, 108.0), Some(48));

        // Outside the content.
        assert_eq!(hit(-1.0, 108.0), None); // above the first row
        assert_eq!(hit(84.0, 108.0), None); // row 4 >= total_rows
        assert_eq!(hit(0.0, 50.0), None); // address gutter
        assert_eq!(hit(0.0, 350.0), None); // group gap between bytes 7 and 8
    }

    #[test]
    fn hex_offset_at_scrolls_and_clamps_to_file_end() {
        let geo = row_geo();
        let block_h = hex_block_h(1.0);
        // 60 bytes -> 4 rows, but the last row holds only 12 bytes (48..60).
        let len = 60usize;
        let total_rows = len.div_ceil(geo.bpr);
        let hit = |y: f32, x: f32| {
            hex_offset_at(
                point(gpui::px(x), gpui::px(y)),
                &geo,
                2.5,
                block_h,
                total_rows,
                len,
            )
        };

        // Scroll of 2.5 rows: row 2 sits at the viewport top (y=0) and row 3
        // spans [21, 42); the fractional part is dropped by the `as usize` cast.
        assert_eq!(hit(0.0, 108.0), Some(32));
        assert_eq!(hit(21.0, 108.0), Some(48)); // row 3 (offsets 48..60)
        // Last byte of the file sits in cell 11 of row 3 (48 + 11 = 59).
        assert_eq!(geo.cell_x(11), 448.0);
        assert_eq!(hit(21.0, 448.0), Some(59));
        // Cell 12 would be offset 60 == len: clamped to None.
        assert_eq!(hit(21.0, 478.0), None);
        // A row below the last one is always None.
        assert_eq!(hit(42.0, 108.0), None);
    }
}
