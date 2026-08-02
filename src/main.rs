#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
// Curated `clippy::pedantic` allows (see Cargo.toml `[lints.clippy]`):
// - Packed RGB/RGBA color literals (`rgb(0x7aa2f7)`) are the idiomatic form
//   for a hex viewer's palette; forcing digit separators hurts readability.
// - UI/scroll math converts between f32, f64 and usize (pixel sizes, row
//   offsets, viewport heights); the values are bounded and the casts are
//   deliberate, so the flagged precision loss/truncation cannot occur.
// - Float equality is used only against exactly-representable constants
//   (0.0, 1.0) and in test assertions.
#![allow(
    clippy::unreadable_literal,
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    clippy::float_cmp
)]

mod app;
mod color;
mod config;
mod entropy;
mod panes;

use std::path::PathBuf;

use gpui::{
    AppContext, Application, Bounds, KeyBinding, Pixels, TitlebarOptions, WindowBounds,
    WindowOptions, actions, point, px, size,
};

// All keyboard actions. App-level (root view) bindings dispatch navigation,
// zoom, open/jump and copy; the jump dialog's text field additionally
// handles `Backspace` / `Delete` / `MoveLeft` / `MoveRight` / `Paste` and
// `JumpSubmit` / `JumpCancel`.
actions!(
    parallhex,
    [
        OpenFile,
        JumpToOffset,
        ResetView,
        ResetColumns,
        ResetSettings,
        ZoomIn,
        ZoomOut,
        NavigateLeft,
        NavigateRight,
        NavigateUp,
        NavigateDown,
        NavigatePageUp,
        NavigatePageDown,
        NavigateHome,
        NavigateEnd,
        CopySelectionHex,
        CopySelectionAscii,
        ClearSelection,
        // Jump dialog text field. Cursor movement reuses NavigateLeft /
        // NavigateRight so only one keybinding is needed per key.
        Backspace,
        Delete,
        Paste,
        JumpSubmit,
        JumpCancel,
    ]
);

/// Minimum window size, shared by the launch options and the restore clamp
/// in `restored_bounds` so the two can't diverge.
const MIN_WINDOW_W: f32 = 1000.0;
const MIN_WINDOW_H: f32 = 600.0;

/// Result of parsing the command line.
enum Cli {
    /// Launch the app, optionally opening a file.
    Launch(Option<PathBuf>),
    /// Print usage / an error and exit with this status code.
    Exit(i32),
}

/// Parse command-line arguments. Positional arguments are file paths (the
/// first is opened on startup); `-h`/`--help` prints usage; `--` ends option
/// parsing (everything after is a file path); any other `-`-prefixed option
/// is rejected instead of being treated as a file.
fn parse_args(args: impl Iterator<Item = String>) -> Cli {
    let mut file: Option<PathBuf> = None;
    let mut positional_only = false;
    for arg in args {
        if positional_only {
            if file.is_none() {
                file = Some(PathBuf::from(arg));
            }
            continue;
        }
        match arg.as_str() {
            "--" => positional_only = true,
            "-h" | "--help" => {
                print_usage();
                return Cli::Exit(0);
            }
            _ if arg.starts_with('-') => {
                eprintln!("parallhex: unknown option '{arg}'");
                print_usage();
                return Cli::Exit(2);
            }
            _ if file.is_none() => file = Some(PathBuf::from(arg)),
            _ => {} // extra positional arguments are ignored
        }
    }
    Cli::Launch(file)
}

fn print_usage() {
    println!("Usage: parallhex [OPTIONS] [FILE]");
    println!("       parallhex --help");
    println!();
    println!("Wide hex-viewer binary explorer.");
    println!();
    println!("Arguments:");
    println!("  FILE    binary file to open on startup (optional)");
    println!();
    println!("Options:");
    println!("  -h, --help    print this help and exit");
}

fn main() {
    let initial_file = match parse_args(std::env::args().skip(1)) {
        Cli::Exit(code) => std::process::exit(code),
        Cli::Launch(file) => file,
    };
    Application::new().run(move |cx: &mut gpui::App| {
        cx.bind_keys([
            KeyBinding::new("cmd-o", OpenFile, None),
            KeyBinding::new("cmd-g", JumpToOffset, None),
            KeyBinding::new("cmd-0", ResetView, None),
            KeyBinding::new("shift-cmd-l", ResetColumns, None),
            KeyBinding::new("=", ZoomIn, None),
            KeyBinding::new("-", ZoomOut, None),
            KeyBinding::new("left", NavigateLeft, None),
            KeyBinding::new("right", NavigateRight, None),
            KeyBinding::new("up", NavigateUp, None),
            KeyBinding::new("down", NavigateDown, None),
            KeyBinding::new("pageup", NavigatePageUp, None),
            KeyBinding::new("pagedown", NavigatePageDown, None),
            KeyBinding::new("home", NavigateHome, None),
            KeyBinding::new("end", NavigateEnd, None),
            KeyBinding::new("cmd-c", CopySelectionHex, None),
            KeyBinding::new("shift-cmd-c", CopySelectionAscii, None),
            KeyBinding::new("backspace", Backspace, None),
            KeyBinding::new("delete", Delete, None),
            KeyBinding::new("cmd-v", Paste, None),
            KeyBinding::new("enter", JumpSubmit, None),
            KeyBinding::new("escape", JumpCancel, None),
        ]);

        let prefs = config::load();
        let displays: Vec<Bounds<Pixels>> = cx.displays().iter().map(|d| d.bounds()).collect();
        let bounds = restored_bounds(
            &prefs,
            &displays,
            Bounds::centered(None, size(px(1600.), px(900.)), cx),
        );
        let window_bounds = Some(if prefs.window_maximized {
            WindowBounds::Maximized(bounds)
        } else {
            WindowBounds::Windowed(bounds)
        });
        cx.open_window(
            WindowOptions {
                window_bounds,
                window_min_size: Some(size(px(MIN_WINDOW_W), px(MIN_WINDOW_H))),
                titlebar: Some(TitlebarOptions {
                    title: Some("ParallHex".into()),
                    ..Default::default()
                }),
                focus: true,
                ..Default::default()
            },
            move |window, cx| cx.new(|cx| app::ParallHexApp::new(window, cx, initial_file)),
        )
        .unwrap();
        cx.on_window_closed(|cx| cx.quit()).detach();
        cx.activate(true);
    });
}

/// Choose the window bounds to open with. The persisted geometry is used
/// when it intersects at least one connected display; otherwise (first run,
/// monitor unplugged, resolution shrunk) a centered default keeps the window
/// on-screen. The restored size is never smaller than the window minimum.
fn restored_bounds(
    prefs: &config::Config,
    displays: &[Bounds<Pixels>],
    fallback: Bounds<Pixels>,
) -> Bounds<Pixels> {
    let Some((left, top, width, height)) = prefs.window_bounds else {
        return fallback;
    };
    let candidate = Bounds::new(
        point(px(left), px(top)),
        size(px(width.max(MIN_WINDOW_W)), px(height.max(MIN_WINDOW_H))),
    );
    if displays
        .iter()
        .any(|display| display.intersects(&candidate))
    {
        candidate
    } else {
        fallback
    }
}

#[cfg(test)]
mod tests {
    use super::{Cli, parse_args, restored_bounds};
    use std::path::PathBuf;

    use crate::config;
    use gpui::{Bounds, Pixels, point, px, size};

    fn bounds(x: f32, y: f32, w: f32, h: f32) -> Bounds<Pixels> {
        Bounds::new(point(px(x), px(y)), size(px(w), px(h)))
    }

    fn launch(args: &[&str]) -> Option<PathBuf> {
        match parse_args(args.iter().map(std::string::ToString::to_string)) {
            Cli::Launch(file) => file,
            Cli::Exit(_) => panic!("expected launch, got exit"),
        }
    }

    fn exit_code(args: &[&str]) -> i32 {
        match parse_args(args.iter().map(std::string::ToString::to_string)) {
            Cli::Exit(code) => code,
            Cli::Launch(_) => panic!("expected exit, got launch"),
        }
    }

    #[test]
    fn no_args_launches_without_file() {
        assert_eq!(launch(&[]), None);
    }

    #[test]
    fn positional_arg_is_opened() {
        assert_eq!(launch(&["data.bin"]), Some(PathBuf::from("data.bin")));
    }

    #[test]
    fn first_positional_wins() {
        assert_eq!(launch(&["a.bin", "b.bin"]), Some(PathBuf::from("a.bin")));
    }

    #[test]
    fn help_exits_cleanly() {
        assert_eq!(exit_code(&["--help"]), 0);
        assert_eq!(exit_code(&["-h"]), 0);
    }

    #[test]
    fn unknown_flag_is_rejected() {
        assert_eq!(exit_code(&["--bogus"]), 2);
        assert_eq!(exit_code(&["-x"]), 2);
    }

    #[test]
    fn unknown_flag_precedes_file() {
        assert_eq!(exit_code(&["--bogus", "data.bin"]), 2);
    }

    #[test]
    fn help_beats_positional() {
        assert_eq!(exit_code(&["data.bin", "--help"]), 0);
    }

    #[test]
    fn double_dash_allows_dash_prefixed_file() {
        assert_eq!(launch(&["--", "-foo.bin"]), Some(PathBuf::from("-foo.bin")));
    }

    #[test]
    fn double_dash_makes_help_a_file() {
        // After `--`, even `--help` is a file path, not a flag.
        assert_eq!(launch(&["--", "--help"]), Some(PathBuf::from("--help")));
    }

    #[test]
    fn restored_geometry_used_when_on_screen() {
        let prefs = config::Config {
            window_bounds: Some((100.0, 120.0, 1400.0, 800.0)),
            window_maximized: false,
            ..Default::default()
        };
        let displays = vec![bounds(0.0, 0.0, 1920.0, 1080.0)];
        let fallback = bounds(50.0, 50.0, 1600.0, 900.0);
        assert_eq!(
            restored_bounds(&prefs, &displays, fallback),
            bounds(100.0, 120.0, 1400.0, 800.0)
        );
    }

    #[test]
    fn restored_geometry_recenters_when_off_screen() {
        let prefs = config::Config {
            window_bounds: Some((5000.0, 5000.0, 1400.0, 800.0)),
            ..Default::default()
        };
        let displays = vec![bounds(0.0, 0.0, 1920.0, 1080.0)];
        let fallback = bounds(50.0, 50.0, 1600.0, 900.0);
        assert_eq!(restored_bounds(&prefs, &displays, fallback), fallback);
    }

    #[test]
    fn restored_geometry_falls_back_without_saved_position() {
        let prefs = config::Config::default();
        let fallback = bounds(50.0, 50.0, 1600.0, 900.0);
        assert_eq!(restored_bounds(&prefs, &[], fallback), fallback);
    }

    #[test]
    fn restored_geometry_enforces_minimum_size() {
        let prefs = config::Config {
            window_bounds: Some((10.0, 10.0, 200.0, 150.0)),
            ..Default::default()
        };
        let displays = vec![bounds(0.0, 0.0, 1920.0, 1080.0)];
        let fallback = bounds(50.0, 50.0, 1600.0, 900.0);
        assert_eq!(
            restored_bounds(&prefs, &displays, fallback),
            bounds(10.0, 10.0, 1000.0, 600.0)
        );
    }
}
