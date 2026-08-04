//! Shannon entropy (in bits per byte) computed over byte windows.

use rayon::prelude::*;

/// Shannon entropy of a byte slice, normalized to `[0.0, 8.0]` bits per byte.
pub fn block_entropy(data: &[u8]) -> f32 {
    if data.is_empty() {
        return 0.0;
    }
    let mut counts = [0u32; 256];
    for &b in data {
        counts[b as usize] += 1;
    }
    let len = data.len() as f32;
    let mut h = 0.0;
    for &c in &counts {
        if c > 0 {
            let p = c as f32 / len;
            h -= p * p.log2();
        }
    }
    h
}

/// Entropy of every contiguous `window`-sized block of `data`, in parallel.
/// One value per block; pixel entropy is then looked up (and optionally
/// interpolated) per byte from this cache.
pub fn block_entropies(data: &[u8], window: usize) -> Vec<f32> {
    let w = window.max(1);
    let nblocks = data.len().div_ceil(w);
    (0..nblocks)
        .into_par_iter()
        .map(|b| {
            let start = b * w;
            let end = (start + w).min(data.len());
            block_entropy(&data[start..end])
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uniform_is_zero() {
        assert_eq!(block_entropy(&[0x41; 256]), 0.0);
    }

    #[test]
    fn full_range_is_eight() {
        let data: Vec<u8> = (0..=255u8).cycle().take(256).collect();
        let h = block_entropy(&data);
        assert!((h - 8.0).abs() < 0.01, "h={h}");
    }

    #[test]
    fn half_range_is_one() {
        let data: Vec<u8> = (0..256).map(|i| (i % 2) as u8 * 0x41).collect();
        let h = block_entropy(&data);
        assert!((h - 1.0).abs() < 0.01, "h={h}");
    }

    #[test]
    fn blocks_cover_file() {
        let data: Vec<u8> = (0..1000u16).map(|i| (i % 256) as u8).collect();
        let h = block_entropies(&data, 256);
        assert_eq!(h.len(), 4); // 1000 bytes -> ceil(1000/256) = 4 blocks
        assert_eq!(block_entropy(&data[..256]), h[0]);
    }
}
