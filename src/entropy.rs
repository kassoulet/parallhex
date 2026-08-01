//! Shannon entropy (in bits per byte) computed over byte windows.

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

/// Entropy of the `window`-sized block containing `offset`.
pub fn window_entropy_at(data: &[u8], offset: usize, window: usize) -> f32 {
    if data.is_empty() {
        return 0.0;
    }
    let w = window.max(1);
    let start = (offset / w) * w;
    let end = (start + w).min(data.len());
    block_entropy(&data[start..end])
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
}
