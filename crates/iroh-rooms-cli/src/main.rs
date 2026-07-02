//! The `iroh-rooms` binary.
//!
//! Thin entry point: parse and dispatch in [`cli::run`], then map any error to a
//! stderr message and a non-zero exit code. All real work lives in the modules.

use std::process::ExitCode;

mod agent;
mod audit;
mod cli;
mod clock;
mod display;
mod error;
mod file;
mod identity;
mod invite;
mod join;
mod message;
mod paths;
mod pipe;
mod room;

fn main() -> ExitCode {
    match cli::run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            // `{:#}` includes the full anyhow context chain. No secret material
            // ever reaches an error path (spec D8 / §9). A coded failure (spec
            // IR-0110) renders the pinned `error[<code>]:` line and the matching
            // category exit code so scripts can branch on `$?`; an uncoded failure
            // falls back to the generic `error:` line and exit 1.
            if let Some(code) = error::code_of(&err) {
                eprintln!("error[{}]: {err:#}", code.code());
                // IR-0303 §5.1: a second, additive stderr line naming the concrete
                // next step — secret-free by construction (a fixed template). The
                // machine surface above is unchanged; scripts matching `^error\[`
                // or branching on `$?` are unaffected.
                if let Some(next) = code.next_action() {
                    eprintln!("next: {next}");
                }
                ExitCode::from(code.exit_code())
            } else {
                eprintln!("error: {err:#}");
                ExitCode::FAILURE
            }
        }
    }
}
