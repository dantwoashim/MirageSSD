//! MirageSSD command-line interface.

#![deny(unsafe_code)]

#[allow(unsafe_code)]
mod client;
mod commands;
mod output;

use std::{ffi::OsStr, fmt::Display, fs::OpenOptions, io::Write, path::PathBuf, process::ExitCode};

use clap::Parser;
use commands::Command;

#[derive(Debug, Parser)]
#[command(
    name = "mirage",
    about = "MirageSSD virtual asset storage",
    version,
    disable_help_subcommand = true
)]
struct Cli {
    /// Emit a compact, versioned JSON envelope.
    #[arg(long, global = true)]
    json: bool,

    #[command(subcommand)]
    command: Command,
}

fn main() -> ExitCode {
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(error) => {
            append_operational_error(&error);
            error.exit()
        }
    };
    match commands::dispatch(cli.command, cli.json) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            append_operational_error(&error);
            output::emit_error(&error, cli.json)
        }
    }
}

fn append_operational_error(error: &impl Display) {
    let mut arguments = std::env::args_os();
    while let Some(argument) = arguments.next() {
        if argument != OsStr::new("--log-file") {
            continue;
        }
        let Some(path) = arguments.next().map(PathBuf::from) else {
            return;
        };
        if !path.is_absolute() {
            return;
        }
        if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(path) {
            let _ = writeln!(file, "MirageSSD command error: {error}");
        }
        return;
    }
}
