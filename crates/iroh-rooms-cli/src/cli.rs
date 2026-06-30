//! Command-line surface: the `clap` parser and the `run` dispatcher.
//!
//! Surface (spec IR-0101 §5):
//!
//! ```text
//! iroh-rooms [--data-dir <PATH>] identity create --name <NAME> [--force]
//! iroh-rooms [--data-dir <PATH>] identity show [--json]
//! ```

use std::path::PathBuf;

use anyhow::Result;
use clap::{Parser, Subcommand};

use crate::{identity, paths};

/// Local-first rooms over iroh — local identity and device management.
#[derive(Debug, Parser)]
#[command(name = "iroh-rooms", version, about, long_about = None)]
pub struct Cli {
    // This doc comment doubles as the clap `--help` text, so the env var name is
    // left bare (backticks would render literally in help output).
    #[allow(clippy::doc_markdown)]
    /// Data directory override (else $IROH_ROOMS_HOME, else the platform default).
    #[arg(long, global = true, value_name = "PATH")]
    data_dir: Option<PathBuf>,

    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Manage the local participant identity and device keys.
    Identity {
        #[command(subcommand)]
        action: IdentityAction,
    },
}

#[derive(Debug, Subcommand)]
enum IdentityAction {
    /// Generate and store a new identity + device keypair.
    Create {
        /// Display name for this participant (1..=64 bytes, no control chars).
        #[arg(long)]
        name: String,
        /// Replace an existing identity (permanently discards the current keys).
        #[arg(long)]
        force: bool,
    },
    /// Print the local identity and device IDs.
    Show {
        /// Emit a single-line JSON object instead of labeled lines.
        #[arg(long)]
        json: bool,
    },
}

/// Parse arguments and execute the selected command.
///
/// # Errors
/// Propagates any command failure for the binary to map to stderr + a non-zero
/// exit code. (`clap` handles `--help`/`--version` and parse errors itself.)
pub fn run() -> Result<()> {
    let cli = Cli::parse();
    let home = paths::data_dir(cli.data_dir.as_deref())?;

    match cli.command {
        Command::Identity { action } => match action {
            IdentityAction::Create { name, force } => {
                // `identity::create` validates the name first, then creates the
                // home directory, so an invalid name leaves the filesystem clean.
                let profile = identity::create(&home, &name, force)?;
                println!("created identity \"{}\"", profile.name);
                println!("identity_id: {}", profile.identity_id);
                println!("device_id: {}", profile.device_id);
                println!("next: run `iroh-rooms identity show`");
            }
            IdentityAction::Show { json } => {
                let profile = identity::Profile::load(&home)?;
                identity::print_show(&profile, json)?;
            }
        },
    }
    Ok(())
}
