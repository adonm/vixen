//! vixen-headless binary entry point (docs/SPEC.md "Headless CLI surface").

#![forbid(unsafe_code)]

use std::process::ExitCode;

use clap::Parser;
use vixen_headless::Cli;

fn main() -> ExitCode {
    vixen_headless::run(Cli::parse())
}
