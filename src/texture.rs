//! Parallel texture generation pipeline.
//!
//! Builds a per-byte `Color32` image over the whole file, in parallel with
//! rayon, and loads it into an egui texture with nearest-neighbor filtering.
//! For files too large to render at 1 pixel per byte a `stride` is introduced
//! so that one texture pixel represents `stride` consecutive bytes.

use eframe::egui;
use rayon::prelude::*;

use crate::app::{ColorMode, EntropyMapApp, LayoutMode};
use crate::color;
use crate::entropy;
use crate::hilbert;

pub struct TextureInfo {
    pub image: egui::ColorImage,
    pub width: usize,
    pub height: usize,
    pub stride: usize,
}

pub fn generate(state: &EntropyMapApp) -> Option<TextureInfo> {
    let mmap = state.mmap.as_ref()?;
    let data: &[u8] = &mmap[..];
    let len = data.len();
    if len == 0 {
        return None;
    }

    let (width, height, stride) = match state.layout_mode {
        LayoutMode::Scan => {
            let w = 256usize;
            let max_h = 8192usize;
            let stride = len.div_ceil(w * max_h).max(1);
            let num_px = len.div_ceil(stride);
            let h = num_px.div_ceil(w);
            (w, h, stride)
        }
        LayoutMode::Hilbert => {
            let max_side = 4096usize;
            let stride = len.div_ceil(max_side * max_side).max(1);
            let num_px = len.div_ceil(stride);
            let side = next_pow2(num_px.isqrt().max(1)).clamp(1, max_side);
            (side, side, stride)
        }
    };

    let num_px = len.div_ceil(stride);
    let window = state.window_size.max(1);

    let entropies: Vec<f32> = if state.color_mode == ColorMode::Entropy {
        let nblocks = len.div_ceil(window);
        (0..nblocks)
            .into_par_iter()
            .map(|b| {
                let start = b * window;
                let end = (start + window).min(len);
                entropy::block_entropy(&data[start..end])
            })
            .collect()
    } else {
        Vec::new()
    };

    let mut pixels = vec![egui::Color32::from_gray(12); width * height];
    pixels
        .par_chunks_mut(width)
        .enumerate()
        .for_each(|(y, row)| {
            for (x, px) in row.iter_mut().enumerate() {
                let i = match state.layout_mode {
                    LayoutMode::Scan => y * width + x,
                    LayoutMode::Hilbert => hilbert::xy2d(height, x, y),
                };
                if i >= num_px {
                    *px = egui::Color32::from_gray(12);
                    continue;
                }
                let off = i * stride;
                let byte = data[off];
                *px = match state.color_mode {
                    ColorMode::Class => color::class_color(byte),
                    ColorMode::Byte => egui::Color32::from_gray(byte),
                    ColorMode::Entropy => color::entropy_color(entropies[off / window]),
                };
            }
        });

    Some(TextureInfo {
        image: egui::ColorImage {
            size: [width, height],
            pixels,
        },
        width,
        height,
        stride,
    })
}

/// Smallest power of two `>= v`.
fn next_pow2(v: usize) -> usize {
    if v <= 1 {
        return 1;
    }
    let shift = usize::BITS - (v - 1).leading_zeros();
    1usize << shift
}
