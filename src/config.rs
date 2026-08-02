//! Persisted UI preferences: layout, zooms, bytes-per-row and the entropy
//! window.
//!
//! Stored as a tiny `key = value` text file in the platform config
//! directory, so no serialization dependency is needed. Loading is
//! tolerant: unknown keys and malformed lines are skipped, so hand-edits
//! and future versions never break startup.

use std::fs;
use std::path::PathBuf;

use crate::color::Colormap;

/// UI preferences that survive across sessions.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Config {
    pub bytes_per_row: usize,
    pub entropy_window: usize,
    pub hex_zoom: f32,
    pub pixel_zoom: f32,
    pub pixel_colormap: Colormap,
    pub overview_width: f32,
    pub pixels_width: f32,
    /// Last window geometry `(x, y, width, height)` in screen pixels,
    /// restored on the next launch. `None` before the first save.
    pub window_bounds: Option<(f32, f32, f32, f32)>,
    /// Whether the window was maximized when it was last saved; on restore
    /// the window opens maximized with `window_bounds` as its un-maximize
    /// size.
    pub window_maximized: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            bytes_per_row: 32,
            entropy_window: 256,
            hex_zoom: 1.0,
            pixel_zoom: 1.0,
            pixel_colormap: Colormap::Greyscale,
            overview_width: 200.0,
            pixels_width: 320.0,
            window_bounds: None,
            window_maximized: false,
        }
    }
}

/// Platform config directory: `$XDG_CONFIG_HOME` (Linux), `$APPDATA`
/// (Windows), `~/Library/Application Support` (macOS), else `~/.config`.
fn config_dir() -> Option<PathBuf> {
    if let Ok(d) = std::env::var("XDG_CONFIG_HOME")
        && !d.is_empty()
    {
        return Some(PathBuf::from(d));
    }
    #[cfg(target_os = "windows")]
    if let Ok(d) = std::env::var("APPDATA")
        && !d.is_empty()
    {
        return Some(PathBuf::from(d));
    }
    #[cfg(target_os = "macos")]
    if let Ok(h) = std::env::var("HOME")
        && !h.is_empty()
    {
        return Some(PathBuf::from(h).join("Library/Application Support"));
    }
    if let Ok(h) = std::env::var("HOME")
        && !h.is_empty()
    {
        return Some(PathBuf::from(h).join(".config"));
    }
    None
}

/// Path of the preferences file (e.g. `~/.config/parallhex/config.txt`).
pub fn path() -> Option<PathBuf> {
    config_dir().map(|d| d.join("parallhex").join("config.txt"))
}

/// Parse a preferences file. Unknown keys, malformed values and non-finite
/// floats are ignored (the default is kept), so the file stays forward
/// compatible and corrupt entries can't break rendering.
pub fn parse(text: &str) -> Config {
    let mut cfg = Config::default();
    let mut window_x: Option<f32> = None;
    let mut window_y: Option<f32> = None;
    let mut window_w: Option<f32> = None;
    let mut window_h: Option<f32> = None;
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let value = value.trim();
        match key.trim() {
            "bytes_per_row" => {
                if let Ok(n) = value.parse() {
                    cfg.bytes_per_row = n;
                }
            }
            "entropy_window" => {
                if let Ok(n) = value.parse() {
                    cfg.entropy_window = n;
                }
            }
            "hex_zoom" => {
                if let Ok(f) = value.parse::<f32>()
                    && f.is_finite()
                {
                    cfg.hex_zoom = f;
                }
            }
            "pixel_zoom" => {
                if let Ok(f) = value.parse::<f32>()
                    && f.is_finite()
                {
                    cfg.pixel_zoom = f;
                }
            }
            "pixel_colormap" => {
                if let Some(cm) = Colormap::from_key(value) {
                    cfg.pixel_colormap = cm;
                }
            }
            "overview_width" => {
                if let Ok(f) = value.parse::<f32>()
                    && f.is_finite()
                {
                    cfg.overview_width = f;
                }
            }
            "pixels_width" => {
                if let Ok(f) = value.parse::<f32>()
                    && f.is_finite()
                {
                    cfg.pixels_width = f;
                }
            }
            "window_x" => window_x = parse_finite(value),
            "window_y" => window_y = parse_finite(value),
            "window_width" => window_w = parse_finite(value),
            "window_height" => window_h = parse_finite(value),
            "window_maximized" => {
                cfg.window_maximized = matches!(value, "true" | "1");
            }
            _ => {} // unknown key: ignore
        }
    }
    // Restore the geometry only when the whole tuple is present and sane;
    // a partial or degenerate set keeps the default (centered) placement.
    if let (Some(x), Some(y), Some(w), Some(h)) = (window_x, window_y, window_w, window_h)
        && w > 0.0
        && h > 0.0
    {
        cfg.window_bounds = Some((x, y, w, h));
    }
    cfg
}

/// Parse a finite `f32`, or `None` when the value is missing, malformed or
/// non-finite (NaN, inf) — such lines are skipped like the other keys.
fn parse_finite(value: &str) -> Option<f32> {
    value.parse::<f32>().ok().filter(|f| f.is_finite())
}

/// Serialize to the `key = value` file format. Sizes are rounded to whole
/// pixels so an unchanged layout doesn't keep rewriting the file. The window
/// geometry lines are only written once a window has actually been placed.
pub fn serialize(cfg: &Config) -> String {
    use std::fmt::Write as _;
    let mut out = String::new();
    // `write!` into a `String` cannot fail; discard the `Result` it returns.
    let _ = writeln!(out, "# ParallHex preferences");
    let _ = writeln!(out, "bytes_per_row = {}", cfg.bytes_per_row);
    let _ = writeln!(out, "entropy_window = {}", cfg.entropy_window);
    let _ = writeln!(out, "hex_zoom = {}", cfg.hex_zoom);
    let _ = writeln!(out, "pixel_zoom = {}", cfg.pixel_zoom);
    let _ = writeln!(out, "pixel_colormap = {}", cfg.pixel_colormap.key());
    let _ = writeln!(out, "overview_width = {}", cfg.overview_width.round());
    let _ = writeln!(out, "pixels_width = {}", cfg.pixels_width.round());
    if let Some((left, top, width, height)) = cfg.window_bounds {
        let _ = writeln!(out, "window_x = {}", left.round());
        let _ = writeln!(out, "window_y = {}", top.round());
        let _ = writeln!(out, "window_width = {}", width.round());
        let _ = writeln!(out, "window_height = {}", height.round());
    }
    let _ = writeln!(out, "window_maximized = {}", cfg.window_maximized);
    out
}

/// Load preferences from disk, falling back to defaults on any error.
pub fn load() -> Config {
    match path() {
        Some(p) => match fs::read_to_string(&p) {
            Ok(text) => parse(&text),
            Err(_) => Config::default(),
        },
        None => Config::default(),
    }
}

/// Write preferences to disk; failures (unwritable dir, …) are silent.
pub fn save(cfg: &Config) {
    let Some(p) = path() else { return };
    if let Some(dir) = p.parent() {
        let _ = fs::create_dir_all(dir);
    }
    let _ = fs::write(&p, serialize(cfg));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_round_trip() {
        let cfg = Config {
            bytes_per_row: 64,
            entropy_window: 512,
            hex_zoom: 2.0,
            pixel_zoom: 8.0,
            pixel_colormap: Colormap::Entropy,
            overview_width: 250.0,
            pixels_width: 400.0,
            window_bounds: Some((120.0, 80.0, 1600.0, 900.0)),
            window_maximized: true,
        };
        assert_eq!(parse(&serialize(&cfg)), cfg);
    }

    #[test]
    fn round_trip_without_window_geometry() {
        let cfg = Config::default();
        assert_eq!(parse(&serialize(&cfg)), cfg);
    }

    #[test]
    fn parse_restores_window_geometry() {
        let cfg = parse(
            "window_x = 320\n\
             window_y = 240\n\
             window_width = 1280\n\
             window_height = 720\n\
             window_maximized = true\n",
        );
        assert_eq!(cfg.window_bounds, Some((320.0, 240.0, 1280.0, 720.0)));
        assert!(cfg.window_maximized);
    }

    #[test]
    fn parse_rejects_partial_or_degenerate_geometry() {
        // Only some of the four keys: geometry stays `None`.
        let partial = parse("window_x = 320\nwindow_width = 1280\n");
        assert_eq!(partial.window_bounds, None);
        // Zero / negative size is degenerate.
        let zero = parse(
            "window_x = 0\n\
             window_y = 0\n\
             window_width = 0\n\
             window_height = 0\n",
        );
        assert_eq!(zero.window_bounds, None);
        // Non-finite values are skipped like the other keys.
        let nan = parse(
            "window_x = nan\n\
             window_y = 0\n\
             window_width = 1280\n\
             window_height = 720\n",
        );
        assert_eq!(nan.window_bounds, None);
    }

    #[test]
    fn parse_entropy_window() {
        assert_eq!(parse("entropy_window = 1024").entropy_window, 1024);
        assert_eq!(parse("entropy_window = abc").entropy_window, 256);
    }

    #[test]
    fn parse_ignores_unknown_and_malformed_lines() {
        let cfg = parse(
            "# comment\n\
             bytes_per_row = 64\n\
             entropy_window = 1024\n\
             unknown_key = 1\n\
             hex_zoom = 2.5\n\
             garbage line without equals\n",
        );
        assert_eq!(cfg.bytes_per_row, 64);
        assert_eq!(cfg.entropy_window, 1024);
        assert_eq!(cfg.hex_zoom, 2.5);
        // Everything else keeps its default.
        assert_eq!(cfg.pixel_zoom, 1.0);
        assert_eq!(cfg.overview_width, 200.0);
    }

    #[test]
    fn parse_rejects_non_finite_and_unparseable_values() {
        let cfg =
            parse("bytes_per_row = abc\nhex_zoom =\npixel_zoom = nan\noverview_width = inf\n");
        assert_eq!(cfg.bytes_per_row, 32);
        assert_eq!(cfg.hex_zoom, 1.0);
        assert_eq!(cfg.pixel_zoom, 1.0);
        assert_eq!(cfg.overview_width, 200.0);
    }
}
