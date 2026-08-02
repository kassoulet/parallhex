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
    AppContext, Application, Bounds, KeyBinding, TitlebarOptions, WindowBounds, WindowOptions,
    actions, px, size,
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

        let bounds = Bounds::centered(None, size(px(1600.), px(900.)), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                window_min_size: Some(size(px(1000.), px(600.))),
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

#[cfg(test)]
mod tests {
    use super::{Cli, parse_args};
    use std::path::PathBuf;

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
}
