//! Command-line surface: the `clap` parser and the `run` dispatcher.
//!
//! Surface (spec IR-0101 §5, IR-0102 §5):
//!
//! ```text
//! iroh-rooms [--data-dir <PATH>] identity create --name <NAME> [--force]
//! iroh-rooms [--data-dir <PATH>] identity show [--json]
//! iroh-rooms [--data-dir <PATH>] room create <NAME>
//! iroh-rooms [--data-dir <PATH>] room members <ROOM_ID>
//! iroh-rooms [--data-dir <PATH>] room invite <ROOM_ID> --invitee <IDENTITY_ID> [--role <ROLE>] [--expires <DURATION>]
//! ```

use std::path::PathBuf;

use anyhow::{anyhow, Context, Result};
use clap::{Parser, Subcommand};
use iroh_rooms_core::event::ids::RoomId;

use crate::{identity, invite, message, paths, room};

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
    /// Mint a key-bound invite ticket for a known invitee identity.
    Invite {
        // Backticks would render literally in clap `--help`, so the id format is
        // described in bare prose here.
        #[allow(clippy::doc_markdown)]
        /// The room id printed by `room create` (blake3:<hex>).
        room_id: String,
        // Backticks would render literally in clap `--help`.
        #[allow(clippy::doc_markdown)]
        /// The invitee's identity id (64-char lowercase hex from `identity show`).
        #[arg(long)]
        invitee: String,
        /// Invited role: `member` (default) or `agent`.
        #[arg(long, default_value = "member")]
        role: String,
        // Backticks would render literally in clap `--help`.
        #[allow(clippy::doc_markdown)]
        /// Optional expiry as <int>{s|m|h|d}, e.g. 24h.
        #[arg(long)]
        expires: Option<String>,
    },
    /// Send a signed text message to the room and push it to connected peers.
    Send {
        // Backticks would render literally in clap `--help`.
        #[allow(clippy::doc_markdown)]
        /// The room id printed by `room create` (blake3:<hex>).
        room_id: String,
        /// The message body (1..=16384 UTF-8 bytes).
        message: String,
        /// Message format: `plain` (default) or `markdown`.
        #[arg(long)]
        format: Option<String>,
        // Backticks would render literally in clap `--help`.
        #[allow(clippy::doc_markdown)]
        /// Reply target event id (blake3:<hex>).
        #[arg(long = "reply-to")]
        reply_to: Option<String>,
        // Backticks would render literally in clap `--help`.
        #[allow(clippy::doc_markdown)]
        /// Peer to dial, repeatable: <ENDPOINT_ID>[@<ip:port>] (else discovery).
        #[arg(long = "peer")]
        peers: Vec<String>,
        /// Best-effort connect timeout as <int>{ms|s|m}, e.g. 5s.
        #[arg(long, default_value = crate::message::DEFAULT_SEND_TIMEOUT)]
        timeout: String,
        /// Use the loopback/CI network stack instead of real-network discovery.
        #[arg(long, hide = true)]
        loopback: bool,
    },
    /// Stream the room timeline, receiving and displaying signed messages live.
    Tail {
        // Backticks would render literally in clap `--help`.
        #[allow(clippy::doc_markdown)]
        /// The room id printed by `room create` (blake3:<hex>).
        room_id: String,
        // Backticks would render literally in clap `--help`.
        #[allow(clippy::doc_markdown)]
        /// Peer to dial, repeatable: <ENDPOINT_ID>[@<ip:port>] (else discovery).
        #[arg(long = "peer")]
        peers: Vec<String>,
        /// Historical rows to render on startup.
        #[arg(long, default_value_t = crate::message::DEFAULT_TAIL_LIMIT)]
        limit: u32,
        /// Use the loopback/CI network stack instead of real-network discovery.
        #[arg(long, hide = true)]
        loopback: bool,
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
            RoomAction::Invite {
                room_id,
                invitee,
                role,
                expires,
            } => {
                let room_id: RoomId = room_id
                    .parse()
                    .map_err(|_| anyhow!("invalid room id (expected `blake3:<hex>`)"))?;
                // `invite` validates --invitee/--role/--expires before any IO, so a
                // bad invocation leaves the store untouched.
                let summary = invite::invite(&home, &room_id, &invitee, &role, expires.as_deref())?;
                invite::print_invite(&summary);
            }
            RoomAction::Send {
                room_id,
                message,
                format,
                reply_to,
                peers,
                timeout,
                loopback,
            } => {
                let room_id: RoomId = room_id
                    .parse()
                    .map_err(|_| anyhow!("invalid room id (expected `blake3:<hex>`)"))?;
                // Parse the timeout before any IO so a bad value writes nothing.
                let timeout = message::parse_timeout(&timeout)?;
                // The online command runs in a scoped runtime; the rest of `run`
                // stays synchronous (spec IR-0105 D2).
                let summary = runtime()?.block_on(message::send(
                    &home,
                    &room_id,
                    &message,
                    format.as_deref(),
                    reply_to.as_deref(),
                    &peers,
                    timeout,
                    loopback,
                ))?;
                message::print_send(&summary);
            }
            RoomAction::Tail {
                room_id,
                peers,
                limit,
                loopback,
            } => {
                let room_id: RoomId = room_id
                    .parse()
                    .map_err(|_| anyhow!("invalid room id (expected `blake3:<hex>`)"))?;
                runtime()?.block_on(message::tail(&home, &room_id, &peers, limit, loopback))?;
            }
        },
    }
    Ok(())
}

/// Build the scoped multi-thread Tokio runtime that hosts the two online commands
/// (`room send`, `room tail`). The offline commands never touch it (spec D2).
fn runtime() -> Result<tokio::runtime::Runtime> {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("could not start the async runtime for an online command")
}
