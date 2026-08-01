//! Persisted UI preferences: layout, zooms, bytes-per-row and the entropy
//! window.
//!
//! Stored as a tiny `key = value` text file in the platform config
//! directory, so no serialization dependency is needed. Loading is
//! tolerant: unknown keys and malformed lines are skipped, so hand-edits
//! and future versions never break startup.

use std::fs;
use std::path::PathBuf;

/// UI preferences that survive across sessions.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Config {
    pub bytes_per_row: usize,
    pub entropy_window: usize,
    pub hex_zoom: f32,
    pub pixel_zoom: f32,
    pub overview_width: f32,
    pub pixels_width: f32,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            bytes_per_row: 32,
            entropy_window: 256,
            hex_zoom: 1.0,
            pixel_zoom: 4.0,
            overview_width: 200.0,
            pixels_width: 320.0,
        }
    }
}

/// Platform config directory: `$XDG_CONFIG_HOME` (Linux), `$APPDATA`
/// (Windows), `~/Library/Application Support` (macOS), else `~/.config`.
fn config_dir() -> Option<PathBuf> {
    if let Ok(d) = std::env::var("XDG_CONFIG_HOME") {
        if !d.is_empty() {
            return Some(PathBuf::from(d));
        }
    }
    #[cfg(target_os = "windows")]
    if let Ok(d) = std::env::var("APPDATA") {
        if !d.is_empty() {
            return Some(PathBuf::from(d));
        }
    }
    #[cfg(target_os = "macos")]
    if let Ok(h) = std::env::var("HOME") {
        if !h.is_empty() {
            return Some(PathBuf::from(h).join("Library/Application Support"));
        }
    }
    if let Ok(h) = std::env::var("HOME") {
        if !h.is_empty() {
            return Some(PathBuf::from(h).join(".config"));
        }
    }
    None
}

/// Path of the preferences file (e.g. `~/.config/entropymap/config.txt`).
pub fn path() -> Option<PathBuf> {
    config_dir().map(|d| d.join("entropymap").join("config.txt"))
}

/// Parse a preferences file. Unknown keys, malformed values and non-finite
/// floats are ignored (the default is kept), so the file stays forward
/// compatible and corrupt entries can't break rendering.
pub fn parse(text: &str) -> Config {
    let mut cfg = Config::default();
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
                if let Ok(f) = value.parse::<f32>() {
                    if f.is_finite() {
                        cfg.hex_zoom = f;
                    }
                }
            }
            "pixel_zoom" => {
                if let Ok(f) = value.parse::<f32>() {
                    if f.is_finite() {
                        cfg.pixel_zoom = f;
                    }
                }
            }
            "overview_width" => {
                if let Ok(f) = value.parse::<f32>() {
                    if f.is_finite() {
                        cfg.overview_width = f;
                    }
                }
            }
            "pixels_width" => {
                if let Ok(f) = value.parse::<f32>() {
                    if f.is_finite() {
                        cfg.pixels_width = f;
                    }
                }
            }
            _ => {} // unknown key: ignore
        }
    }
    cfg
}

/// Serialize to the `key = value` file format. Widths are rounded to whole
/// pixels so an unchanged layout doesn't keep rewriting the file.
pub fn serialize(cfg: &Config) -> String {
    format!(
        "# EntropyMap preferences\n\
         bytes_per_row = {}\n\
         entropy_window = {}\n\
         hex_zoom = {}\n\
         pixel_zoom = {}\n\
         overview_width = {}\n\
         pixels_width = {}\n",
        cfg.bytes_per_row,
        cfg.entropy_window,
        cfg.hex_zoom,
        cfg.pixel_zoom,
        cfg.overview_width.round(),
        cfg.pixels_width.round(),
    )
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
            overview_width: 250.0,
            pixels_width: 400.0,
        };
        assert_eq!(parse(&serialize(&cfg)), cfg);
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
        assert_eq!(cfg.pixel_zoom, 4.0);
        assert_eq!(cfg.overview_width, 200.0);
    }

    #[test]
    fn parse_rejects_non_finite_and_unparseable_values() {
        let cfg = parse("bytes_per_row = abc\nhex_zoom =\npixel_zoom = nan\noverview_width = inf\n");
        assert_eq!(cfg.bytes_per_row, 32);
        assert_eq!(cfg.hex_zoom, 1.0);
        assert_eq!(cfg.pixel_zoom, 4.0);
        assert_eq!(cfg.overview_width, 200.0);
    }
}
