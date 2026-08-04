//! Whole-file and visible-region thumbnails, as raw RGBA buffers.
//!
//! These return plain `Vec<u8>` rather than a texture so the pixel maths is
//! testable without a window, and so a terminal frontend can consume the same
//! buffers (rendering two pixel rows per text row as half-block characters).

use rayon::prelude::*;

use super::color::Rgb;
use super::geom::ByteSource;

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

fn set_pixel(buf: &mut [u8], width: usize, x: usize, y: usize, c: Rgb) {
    let p = (y * width + x) * 4;
    buf[p] = c.r;
    buf[p + 1] = c.g;
    buf[p + 2] = c.b;
    // Opaque: a cell the colormap declined to paint is skipped by the caller
    // rather than written with alpha 0, so anything reaching here is visible.
    buf[p + 3] = 255;
}

/// Compute the raw RGBA pixels (`w` × `h`) of the 2D whole-file overview: one
/// band per cell in `colormap`. The pixel math is unit-testable without a gpui
/// window, and the app runs it on the background executor so the UI thread never
/// blocks on a whole-file pass.
///
/// Under `Colormap::None` every cell is left transparent, so the panel
/// background shows through — `None` mutes a panel rather than disabling it, and
/// the viewport band, hover preview and click-to-navigate all stay live.
pub(crate) fn build_overview_rgba(src: &ByteSource, w: usize, h: usize) -> Vec<u8> {
    // `sample_average` needs at least one byte (it indexes `len - 1`); an
    // empty buffer is the safe placeholder for a missing file. Zero dimensions
    // would divide by zero below (`k % w`), so floor them at 1.
    let w = w.max(1);
    let h = h.max(1);
    if src.is_empty() {
        return vec![0u8; w * h * 4];
    }
    let len = src.len();
    let cells = (w * h).max(1);
    let mut pixels = vec![0u8; w * h * 4];
    for k in 0..cells {
        let start = k * len / cells;
        let end = ((k + 1) * len / cells).max(start + 1);
        let mid = (start + (end - start) / 2).min(len - 1);
        let avg = sample_average(src.data, start, end);
        // Cell k sits at grid (col = k % w, row = k / w).
        if let Some(c) = src.color_of(avg, mid) {
            set_pixel(&mut pixels, w, k % w, k / w, c);
        }
    }
    pixels
}

/// Columns in the horizontal whole-file preview strip.
pub(crate) const STRIP_CELLS: usize = 256;

/// Raw RGBA pixels of the zoom column's visible region: `rows` rows of `bpr`
/// bytes starting at `first_row_start`, each byte a `block × block` pixel
/// square quantized to the integer pixel grid (blocks need not divide
/// evenly into pixels). Returns `(pixels, iw, ih)` — the buffer is `iw × ih`
/// with `iw = ceil(bpr·block)`, `ih = ceil(rows·block)`, so `paint_image`
/// scales it ~1:1 into the panel and no smoothing is needed.
///
/// Under `Colormap::None` every pixel stays transparent, so the panel
/// background shows through; the panel stays interactive either way.
pub(crate) fn build_zoom_rgba(
    src: &ByteSource,
    bpr: usize,
    first_row_start: usize,
    rows: usize,
    block: f32,
) -> (Vec<u8>, usize, usize) {
    let len = src.len();
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
            let Some(c) = src.color_at(off) else {
                continue;
            };
            let x0 = (i as f32 * block).round() as usize;
            let x1 = ((i + 1) as f32 * block).round() as usize;
            // `Rgb` is already 8-bit, so no scaling is needed; opaque because a
            // colour the colormap declined to paint was skipped above.
            let (r8, g8, b8, a8) = (c.r, c.g, c.b, 255);
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::color::Colormap;
    use crate::core::geom::test_support::*;

    #[test]
    fn overview_buffer_is_w_by_h_and_opaque() {
        let data = [0x41u8; 4096];
        let e = entropies(&data);
        let buf = build_overview_rgba(&src(&data, &e, 256, Colormap::Value), 8, 4);
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
        let buf = build_overview_rgba(&src(&data, &e, 256, Colormap::Value), 2, 2);
        for (x, y) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
            assert_eq!(px(&buf, 2, x, y), (170, 170, 170, 255), "cell ({x},{y})");
        }
    }

    #[test]
    fn overview_none_colormap_leaves_cells_transparent() {
        let data = [0xAAu8; 512];
        let e = entropies(&data);
        let buf = build_overview_rgba(&src(&data, &e, 256, Colormap::None), 2, 2);
        assert!(buf.iter().all(|&b| b == 0), "None must paint nothing");
    }

    #[test]
    fn overview_cells_tile_the_file_in_row_major_order() {
        // Four cells over four bytes: cell k is byte k, so row-major order is
        // directly visible in the buffer.
        let data = vec![0x00u8, 0x40, 0x80, 0xC0];
        let e = entropies(&data);
        let buf = build_overview_rgba(&src(&data, &e, 256, Colormap::Value), 2, 2);
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
        let buf = build_overview_rgba(&src(&data, &e, 256, Colormap::Entropy), 1, 1);
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
            build_overview_rgba(&src(&[], &[], 256, Colormap::Value), 4, 2)
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
            build_overview_rgba(&src(&data, &[], 256, Colormap::Value), 0, 4).len(),
            16
        );
        assert_eq!(
            build_overview_rgba(&src(&data, &[], 256, Colormap::Value), 3, 0).len(),
            12
        );
        let buf = build_overview_rgba(&src(&data, &[], 256, Colormap::Value), 0, 2);
        assert_eq!(buf.len(), 2 * 4);
    }

    /// The zoom texture with `block` whole pixels: 4 bytes/row × 2 rows of
    /// distinct bytes -> a 16×8 buffer where byte k occupies a 4×4 block.
    #[test]
    fn zoom_buffer_is_a_per_byte_pixel_grid() {
        let data = vec![0x00u8, 0x40, 0x80, 0xC0, 0x20, 0x60, 0xA0, 0xE0];
        let e = entropies(&data);
        let (buf, iw, ih) = build_zoom_rgba(&src(&data, &e, 256, Colormap::Value), 4, 0, 2, 4.0);
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
        let (buf, iw, _ih) = build_zoom_rgba(&src(&data, &e, 256, Colormap::Value), 2, 8, 2, 4.0);
        assert_eq!((buf[0], buf[1]), (8 * 8, 8 * 8)); // byte 8
        assert_eq!((buf[4 * 4], buf[4 * 4 + 1]), (9 * 8, 9 * 8)); // byte 9
        // Block row 1 spans pixel rows 4..8; it starts at byte 10.
        assert_eq!(px(&buf, iw, 0, 4).0, 10 * 8);
    }

    #[test]
    fn zoom_none_colormap_leaves_the_texture_transparent() {
        let data = [0xAAu8; 16];
        let e = entropies(&data);
        let (buf, iw, ih) = build_zoom_rgba(&src(&data, &e, 256, Colormap::None), 4, 0, 2, 4.0);
        assert_eq!(buf.len(), iw * ih * 4);
        assert!(buf.iter().all(|&b| b == 0), "None must paint nothing");
    }

    #[test]
    fn zoom_quantizes_fractional_blocks_to_the_pixel_grid() {
        // 5 bytes/row over a 19 px panel: block = 3.8 px. Every pixel column
        // still resolves to a byte and the buffer spans the full panel width.
        let data = [0x00u8, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0xFF];
        let e = entropies(&data);
        let (buf, iw, ih) = build_zoom_rgba(&src(&data, &e, 256, Colormap::Value), 5, 0, 2, 3.8);
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
        let (buf, iw, ih) = build_zoom_rgba(&src(&data, &e, 256, Colormap::Value), 6, 0, 2, 4.0);
        assert_eq!((iw, ih), (24, 8));
        // Block row 1 spans pixel rows 4..8 and holds bytes 6 and 7.
        assert_eq!(px(&buf, iw, 0, 4).0, 96); // byte 6 (0x60)
        assert_eq!(px(&buf, iw, 4, 4).0, 112); // byte 7 (0x70)
        assert_eq!(px(&buf, iw, 12, 4).3, 0); // past the last byte: transparent
        // Degenerate inputs never panic and yield an empty buffer.
        let (buf, iw, ih) = build_zoom_rgba(&src(&[], &[], 256, Colormap::Value), 4, 0, 2, 4.0);
        assert_eq!((buf.len(), iw, ih), (0, 0, 0));
        let (buf, iw, ih) = build_zoom_rgba(&src(&data, &e, 256, Colormap::Value), 0, 0, 2, 4.0);
        assert_eq!((buf.len(), iw, ih), (0, 0, 0));
        let (buf, iw, ih) = build_zoom_rgba(&src(&data, &e, 256, Colormap::Value), 6, 0, 0, 4.0);
        assert_eq!((buf.len(), iw, ih), (0, 0, 0));
    }
}
