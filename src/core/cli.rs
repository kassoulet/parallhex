//! Command-line parsing, shared by every frontend.
//!
//! The binary name is threaded through rather than hardcoded, so each frontend's
//! usage text names the command the user actually typed.

use std::path::PathBuf;

/// Result of parsing the command line.
pub enum Cli {
    /// Launch the app, optionally opening a file.
    Launch(Option<PathBuf>),
    /// Print usage / an error and exit with this status code.
    Exit(i32),
}

/// Parse command-line arguments. Positional arguments are file paths (the
/// first is opened on startup); `-h`/`--help` prints usage; `--` ends option
/// parsing (everything after is a file path); any other `-`-prefixed option
/// is rejected instead of being treated as a file.
pub fn parse_args(args: impl Iterator<Item = String>, bin_name: &str) -> Cli {
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
                print_usage(bin_name);
                return Cli::Exit(0);
            }
            _ if arg.starts_with('-') => {
                eprintln!("{bin_name}: unknown option '{arg}'");
                print_usage(bin_name);
                return Cli::Exit(2);
            }
            _ if file.is_none() => file = Some(PathBuf::from(arg)),
            _ => {} // extra positional arguments are ignored
        }
    }
    Cli::Launch(file)
}

fn print_usage(bin_name: &str) {
    println!("Usage: {bin_name} [OPTIONS] [FILE]");
    println!("       {bin_name} --help");
    println!();
    println!("Wide hex-viewer binary explorer.");
    println!();
    println!("Arguments:");
    println!("  FILE    binary file to open on startup (optional)");
    println!();
    println!("Options:");
    println!("  -h, --help    print this help and exit");
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every frontend parses the same way; the name only shapes the text.
    const BIN: &str = "parallhex-gpui";

    fn launch(args: &[&str]) -> Option<PathBuf> {
        match parse_args(args.iter().map(std::string::ToString::to_string), BIN) {
            Cli::Launch(file) => file,
            Cli::Exit(_) => panic!("expected launch, got exit"),
        }
    }

    fn exit_code(args: &[&str]) -> i32 {
        match parse_args(args.iter().map(std::string::ToString::to_string), BIN) {
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
    fn usage_names_the_calling_binary() {
        // The name is a parameter so parallhex-tui advertises itself, not the
        // gpui binary; behaviour is identical either way.
        assert!(matches!(
            parse_args(["-h".to_owned()].into_iter(), "parallhex-tui"),
            Cli::Exit(0)
        ));
        assert!(matches!(
            parse_args(["--bogus".to_owned()].into_iter(), "parallhex-tui"),
            Cli::Exit(2)
        ));
        assert!(matches!(
            parse_args(["f.bin".to_owned()].into_iter(), "parallhex-tui"),
            Cli::Launch(Some(_))
        ));
    }
}
