#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod color;
mod config;
mod entropy;
mod panes;

use std::path::PathBuf;

use eframe::egui;

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
                eprintln!("entropymap: unknown option '{arg}'");
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
    println!("Usage: entropymap [OPTIONS] [FILE]");
    println!("       entropymap --help");
    println!();
    println!("Wide hex-viewer binary explorer.");
    println!();
    println!("Arguments:");
    println!("  FILE    binary file to open on startup (optional)");
    println!();
    println!("Options:");
    println!("  -h, --help    print this help and exit");
}

fn main() -> eframe::Result {
    let initial_file = match parse_args(std::env::args().skip(1)) {
        Cli::Exit(code) => std::process::exit(code),
        Cli::Launch(file) => file,
    };
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("EntropyMap")
            .with_inner_size([1600.0, 900.0])
            .with_min_inner_size([1000.0, 600.0]),
        ..Default::default()
    };
    eframe::run_native(
        "EntropyMap",
        options,
        Box::new(move |cc| Ok(Box::new(app::EntropyMapApp::new(cc, initial_file)))),
    )
}

#[cfg(test)]
mod tests {
    use super::{parse_args, Cli};
    use std::path::PathBuf;

    fn launch(args: &[&str]) -> Option<PathBuf> {
        match parse_args(args.iter().map(|s| s.to_string())) {
            Cli::Launch(file) => file,
            Cli::Exit(_) => panic!("expected launch, got exit"),
        }
    }

    fn exit_code(args: &[&str]) -> i32 {
        match parse_args(args.iter().map(|s| s.to_string())) {
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
        assert_eq!(
            launch(&["a.bin", "b.bin"]),
            Some(PathBuf::from("a.bin"))
        );
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
