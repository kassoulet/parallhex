//! The gpui frontend's entry point. Parses the command line, then hands off to
//! the library.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use parallhex::gui::{Cli, parse_args, run};

fn main() {
    match parse_args(std::env::args().skip(1)) {
        Cli::Exit(code) => std::process::exit(code),
        Cli::Launch(file) => run(file),
    }
}
