//! gpui painting: everything that names a gpui type.
//!
//! The geometry and pixel maths these draw with lives in `core::geom` and
//! `core::thumb`; this module is the boundary where neutral values become gpui
//! quads, text runs and textures.

use std::ops::Range;
use std::sync::Arc;

use gpui::{
    App, Background, BorderStyle, Bounds, Corners, Font, Hsla, PaintQuad, Pixels, Point,
    RenderImage, Rgba, ShapedLine, TextRun, Window, font, point, px, quad, rgb, rgba, size,
    transparent_black,
};

use crate::core::color::{self, Rgb};
use crate::core::geom::{ByteSource, RowGeo, build_row_text_into, scrollbar_thumb, visible_rows};
use crate::core::thumb;

/// Keyboard zoom step factor (`+` / `-`), applied multiplicatively per press.
pub(crate) const ZOOM_STEP: f32 = 1.25;

/// Hex row geometry: the text size is fixed, so these are constants.
pub(crate) const ROW_H: f32 = 18.0;

pub(crate) const ROW_GAP: f32 = 3.0;

/// Left padding for the address gutter, in pixels. Passed to `RowGeo` rather
/// than baked into it, so a character-grid frontend can pass 0.
pub(crate) const ADDR_X: f32 = 8.0;

/// Monospace size for hex cells.
pub(crate) const HEX_FONT_SIZE: f32 = 13.0;

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

/// Width of the hex column's scrollbar, and the smallest thumb that stays
/// grabbable on a huge file.
pub(crate) const SCROLLBAR_W: f32 = 10.0;

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

/// The monospace font used for the hex/ASCII column, at `zoom`.
pub(crate) fn mono_font(family: &str) -> Font {
    font(family.to_owned())
}

/// Convert a core `Rgb` to the gpui colour type. This is the single boundary
/// where the toolkit-neutral palette becomes gpui's own representation.
pub(crate) fn to_rgba(c: Rgb) -> Rgba {
    Rgba {
        r: f32::from(c.r) / 255.0,
        g: f32::from(c.g) / 255.0,
        b: f32::from(c.b) / 255.0,
        a: 1.0,
    }
}

/// Convert an RGBA color to the HSLA used by gpui text runs.
fn to_hsla(c: Rgba) -> Hsla {
    Hsla::from(c)
}

/// Glyph color for a cell: contrast text against its background, or the
/// default foreground when the colormap paints no background.
fn cell_glyph_color(cell: Option<Rgb>) -> Hsla {
    match cell {
        Some(bg) => to_hsla(to_rgba(color::fg_for_bg(bg))),
        None => to_hsla(rgb(DEFAULT_FG)),
    }
}

/// Build the color runs for a row line: gray address, contrast-colored hex
/// digits and ASCII glyphs per byte, neutral (invisible) spaces. Glyph colors
/// come from the row's precomputed per-byte cell colors (`colors[i]`), so each
/// byte's color is computed once and reused for both cells and both glyphs
/// rather than four times per byte. Runs are written into the caller-owned
/// `runs` buffer.
fn build_row_runs(
    n: usize,
    hex_offsets: &[usize],
    ascii_offsets: &[usize],
    font: &Font,
    total_len: usize,
    colors: &[Option<Rgb>],
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

/// Paint the cell backgrounds and selection tint for a run of cells. A run
/// extends while the (color, selection) state is unchanged and stays within one
/// group of `split_every` cells (the hex side splits at 8 so a merged quad never
/// covers the group gap, which shows the panel background; `usize::MAX` merges
/// the whole row). Cell x comes from `x_of`, and `cell_w` is the advance from one
/// cell to the next.
///
/// `fill_w` is how much of each cell the byte colour covers. When it is narrower
/// than the advance — the hex side, where only the two digits are coloured and
/// the space between bytes stays panel background — the cells are not contiguous,
/// so each needs its own quad. The **selection** still paints as one continuous
/// band across the run either way, so a selected range reads as a block rather
/// than a dashed line.
#[allow(clippy::too_many_arguments)]
fn paint_cell_runs(
    window: &mut Window,
    origin: Point<Pixels>,
    y0: f32,
    colors: &[Option<Rgb>],
    selected: &[bool],
    x_of: impl Fn(usize) -> f32,
    cell_w: f32,
    fill_w: f32,
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
        let span = Bounds::new(
            point(origin.x + px(x0), origin.y + px(y0)),
            size(px(x_of(j - 1) + cell_w - x0), px(ROW_H)),
        );
        if let Some(bg) = bg {
            if fill_w >= cell_w {
                // Contiguous cells: the whole run is one quad.
                window.paint_quad(filled_quad(span, to_rgba(bg)));
            } else {
                for k in i..j {
                    let cell = Bounds::new(
                        point(origin.x + px(x_of(k)), origin.y + px(y0)),
                        size(px(fill_w), px(ROW_H)),
                    );
                    window.paint_quad(filled_quad(cell, to_rgba(bg)));
                }
            }
        }
        if s {
            window.paint_quad(filled_quad(span, rgba(0xffffff3d)));
        }
        i = j;
    }
}

/// Paint the hex column into `bounds`: class-colored hex + ASCII cells with
/// selection/hover overlays, one virtualized row at a time.
///
/// On the hex side the byte colour covers the two digits only, so the space
/// between bytes keeps the panel background and the cells read as discrete
/// values rather than one continuous band. ASCII glyphs are adjacent, so there
/// the colour does run together. Selection tints and run-merging still work
/// across both — see `paint_cell_runs` — and runs split at every 8-byte group
/// boundary so no merged quad covers the group gap.
#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
pub(crate) fn paint_hex(
    window: &mut Window,
    cx: &mut App,
    bounds: Bounds<Pixels>,
    src: &ByteSource,
    font: &Font,
    char_w: f32,
    bpr: usize,
    first_row_start: usize,
    hovered: Option<usize>,
    sel: Option<&Range<usize>>,
) {
    let len = src.len();
    if len == 0 || bpr == 0 {
        return;
    }
    let row_h = ROW_H;
    let font_size = px(HEX_FONT_SIZE);
    let geo = RowGeo::new(ADDR_X, char_w, bpr);

    let rows = visible_rows(bounds.size.height.to_f64() as f32, BLOCK_H);

    let origin = bounds.origin;
    // Reusable per-row buffers: cleared and refilled for each row instead of
    // reallocated.
    let mut text = String::with_capacity(128);
    let mut hex_offsets = Vec::with_capacity(bpr);
    let mut ascii_offsets = Vec::with_capacity(bpr);
    let mut colors: Vec<Option<Rgb>> = Vec::with_capacity(bpr);
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
        colors.extend((0..n).map(|i| src.color_at(row_start + i)));
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
            geo.hex_fill_w(),
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
            src.data,
            row_start,
            n,
            bpr,
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

/// Build the horizontal whole-file preview strip: a fixed `STRIP_CELLS`×1 band
/// in `colormap`, x mapping to file offset.
///
/// A single row of the overview *is* the strip — `build_overview_rgba` lays its
/// cells out row-major, so at `h = 1` every cell is one column and the two
/// generators agree by construction rather than by keeping two copies of the
/// downsampling math in step.
pub(crate) fn build_strip_image(src: &ByteSource) -> Arc<RenderImage> {
    let pixels = thumb::build_overview_rgba(src, thumb::STRIP_CELLS, 1);
    render_image_from_rgba(thumb::STRIP_CELLS, 1, pixels)
}

/// Build the zoom column's visible-region texture. Wraps `build_zoom_rgba`
/// so the pixel math is unit-testable without a gpui window.
pub(crate) fn build_zoom_image(
    src: &ByteSource,
    bpr: usize,
    first_row_start: usize,
    rows: usize,
    block: f32,
) -> Option<Arc<RenderImage>> {
    let (pixels, iw, ih) = thumb::build_zoom_rgba(src, bpr, first_row_start, rows, block);
    (iw > 0 && ih > 0).then(|| render_image_from_rgba(iw, ih, pixels))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::color::Colormap;
    use crate::core::geom::test_support::*;
    use crate::core::thumb::build_overview_rgba;

    #[test]
    fn thumbnail_wrappers_build_valid_images() {
        let data = [0x41u8; 4096];
        let e = entropies(&data);
        // Routing the RGBA buffer through the image crate panics on a
        // buffer-size mismatch — so simply constructing the image here
        // verifies the pixel-buffer invariants end to end.
        let buf = build_overview_rgba(&src(&data, &e, 256, Colormap::Value), 3, 2);
        assert_eq!(buf.len(), 3 * 2 * 4);
        let _img = render_image_from_rgba(3, 2, buf);
        let _strip = build_strip_image(&src(&data, &e, 256, Colormap::Value));
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
        let overview = build_overview_rgba(&src(&data, &e, 256, Colormap::Value), w, h);
        assert_eq!(overview.len(), w * h * 4);
        assert!(overview.iter().skip(3).step_by(4).all(|&a| a == 255));
        let _img = render_image_from_rgba(w, h, overview);
        let _strip = build_strip_image(&src(&data, &e, 256, Colormap::Value));
    }

    #[test]
    fn zoom_image_wrapper_builds_a_valid_texture() {
        let data: Vec<u8> = (0..32).map(|i| i as u8).collect();
        let e = entropies(&data);
        let img = build_zoom_image(&src(&data, &e, 256, Colormap::Class), 8, 0, 2, 4.0);
        assert!(img.is_some());
        let img = build_zoom_image(&src(&[], &e, 256, Colormap::Class), 8, 0, 2, 4.0);
        assert!(img.is_none());
    }
}
