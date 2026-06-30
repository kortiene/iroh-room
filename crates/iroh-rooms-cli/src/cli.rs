//! Command-line surface: the `clap` parser and the `run` dispatcher.
//!
//! Surface (spec IR-0101 §5, IR-0102 §5):
//!
//! ```text
//! iroh-rooms [--data-dir <PATH>] identity create --name <NAME> [--force]
//! iroh-rooms [--data-dir <PATH>] identity show [--json]
//! iroh-rooms [--data-dir <PATH>] room create <NAME>
//! iroh-rooms [--data-dir <PATH>] room members <ROOM_ID>
//! ```

use std::path::PathBuf;

use anyhow::{anyhow, Result};
use clap::{Parser, Subcommand};
use iroh_rooms_core::event::ids::RoomId;

use crate::{identity, paths, room};

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
    /// Create and inspect rooms.
    Room {
        #[command(subcommand)]
        action: RoomAction,
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

#[derive(Debug, Subcommand)]
enum RoomAction {
    /// Create a private room and persist its genesis `room.created` event.
    Create {
        /// Room name (1..=128 bytes, no control chars).
        name: String,
    },
    /// Print the room's admin and members, re-derived from the persisted log.
    Members {
        // Backticks would render literally in clap `--help`, so the id format is
        // described in bare prose here.
        #[allow(clippy::doc_markdown)]
        /// The room id printed by `room create` (blake3:<hex>).
        room_id: String,
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
        Command::Room { action } => match action {
            RoomAction::Create { name } => {
                // `room::create` validates the name first, then loads secrets and
                // ensures the home, so an invalid name leaves the filesystem clean.
                let summary = room::create(&home, &name)?;
                println!("created room \"{}\"", summary.room_name);
                println!("room_id: {}", summary.room_id);
                println!("admin: {}", summary.admin_identity_id);
                println!("next: run `iroh-rooms room members {}`", summary.room_id);
            }
            RoomAction::Members { room_id } => {
                let room_id: RoomId = room_id
                    .parse()
                    .map_err(|_| anyhow!("invalid room id (expected `blake3:<hex>`)"))?;
                let view = room::members(&home, &room_id)?;
                room::print_members(&view);
            }
        },
    }
    Ok(())
}
