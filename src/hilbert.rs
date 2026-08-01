//! Hilbert space-filling curve index mapping.
//!
//! For a grid of side length `n = 2^N`, `d2xy` maps a 1D index `d` to
//! `(x, y)` coordinates and `xy2d` is its inverse.

fn rot(n: usize, x: usize, y: usize, rx: usize, ry: usize) -> (usize, usize) {
    let (mut x, mut y) = (x, y);
    if ry == 0 {
        if rx == 1 {
            // Reflect within the quadrant. The canonical C version uses signed
            // ints and can go negative here; wrapping reproduces the same
            // two's-complement arithmetic (valid for grid sizes < 2^30).
            x = n.wrapping_sub(1).wrapping_sub(x);
            y = n.wrapping_sub(1).wrapping_sub(y);
        }
        std::mem::swap(&mut x, &mut y);
    }
    (x, y)
}

/// Convert a 1D index `d` on a `n x n` (n a power of two) Hilbert curve
/// to 2D coordinates `(x, y)`.
pub fn d2xy(n: usize, mut d: usize) -> (usize, usize) {
    let mut x = 0;
    let mut y = 0;
    let mut s = 1;
    while s < n {
        let rx = 1 & (d / 2);
        let ry = 1 & (d ^ rx);
        let (cx, cy) = rot(s, x, y, rx, ry);
        x = cx + s * rx;
        y = cy + s * ry;
        d /= 4;
        s *= 2;
    }
    (x, y)
}

/// Convert 2D coordinates `(x, y)` on a `n x n` (n a power of two) Hilbert
/// curve back to a 1D index `d`.
pub fn xy2d(n: usize, mut x: usize, mut y: usize) -> usize {
    let mut d = 0;
    let mut s = n / 2;
    while s > 0 {
        let rx = usize::from((x & s) > 0);
        let ry = usize::from((y & s) > 0);
        d += s * s * ((3 * rx) ^ ry);
        let (cx, cy) = rot(s, x, y, rx, ry);
        x = cx;
        y = cy;
        s /= 2;
    }
    d
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        for n in [1usize, 2, 4, 8, 16, 128, 512] {
            for d in 0..(n * n) {
                let (x, y) = d2xy(n, d);
                assert!(x < n && y < n, "n={n} d={d}");
                assert_eq!(xy2d(n, x, y), d, "n={n} d={d}");
            }
        }
    }

    #[test]
    fn start_and_end() {
        assert_eq!(d2xy(16, 0), (0, 0));
        assert_eq!(xy2d(16, 0, 0), 0);
        assert!(d2xy(16, 255).0 < 16);
        assert!(d2xy(16, 255).1 < 16);
    }
}
