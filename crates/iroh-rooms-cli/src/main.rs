//! The `iroh-rooms` binary.
//!
//! Thin entry point: parse and dispatch in [`cli::run`], then map any error to a
//! stderr message and a non-zero exit code. All real work lives in the modules.

use std::process::ExitCode;

mod cli;
mod clock;
mod identity;
mod invite;
mod paths;
mod room;

fn main() -> ExitCode {
    match cli::run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            // `{:#}` includes the full anyhow context chain. No secret material
            // ever reaches an error path (spec D8 / §9).
            eprintln!("error: {err:#}");
            ExitCode::FAILURE
        }
    }
}
