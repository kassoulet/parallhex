//! The terminal frontend's entry point.

use parallhex::core::cli::{Cli, parse_args};

fn main() {
    match parse_args(std::env::args().skip(1), "parallhex-tui") {
        Cli::Exit(code) => std::process::exit(code),
        Cli::Launch(file) => {
            if let Err(e) = parallhex::tui::run(file) {
                eprintln!("parallhex-tui: {e}");
                std::process::exit(1);
            }
        }
    }
}
