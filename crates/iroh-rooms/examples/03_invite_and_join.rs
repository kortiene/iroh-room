//! Step 3 of `docs/getting-started.md`: redeem an invite ticket over the
//! network (the joiner half of `iroh-rooms room join`).
//!
//! Requires: `--features experimental`. The admin side (authoring the
//! `member.invited` event and the copy-pasteable `RoomInviteTicket`) is pure
//! offline authoring — see `offline_author_and_validate.rs`. This example
//! covers only the online half: bootstrap the membership sub-DAG from the
//! admin, author + self-validate a `member.joined`, and publish it.
//!
//! Compile-only in CI (`cargo build -p iroh-rooms --examples --features
//! experimental`) — running it for real needs a second, already-running
//! admin peer (mirroring `docs/getting-started.md` Step 3's two terminals),
//! so `ADMIN_ENDPOINT` below is a placeholder to substitute.
//!
//! This is a trimmed illustration of the full `iroh-rooms room join`
//! orchestration (timeouts, coded errors, `--display-name`, …); see
//! `crates/iroh-rooms-cli/src/join.rs` for the production version.

#[cfg(feature = "experimental")]
use std::sync::Arc;
#[cfg(feature = "experimental")]
use std::time::Duration;

#[cfg(feature = "experimental")]
use iroh_rooms::events::{validate_wire_bytes, ValidationContext};
#[cfg(feature = "experimental")]
use iroh_rooms::experimental::session::{
    AllowlistAdmission, EndpointAddr, EndpointId, NetConfig, Node, SecretKey, DEFAULT_TICK,
};
#[cfg(feature = "experimental")]
use iroh_rooms::experimental::store::EventStore;
#[cfg(feature = "experimental")]
use iroh_rooms::experimental::sync::{SyncConfig, SyncEngine};
#[cfg(feature = "experimental")]
use iroh_rooms::identity::{DeviceBinding, SigningKey};
#[cfg(feature = "experimental")]
use iroh_rooms::room::{build_member_joined, RoomInviteTicket};

/// Substitute the admin's `EndpointId` (printed by their node as `listening:
/// <id>`) — see `docs/getting-started.md` Step 3.
#[cfg(feature = "experimental")]
const ADMIN_ENDPOINT: &str = "<PASTE_ADMIN_ENDPOINT_ID_HERE>";
/// Substitute the `roomtkt1…` ticket the admin produced for you.
#[cfg(feature = "experimental")]
const TICKET: &str = "<PASTE_INVITE_TICKET_HERE>";

#[cfg(feature = "experimental")]
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let ticket: RoomInviteTicket = TICKET.trim().parse()?;
    let joiner_identity = SigningKey::generate();
    let joiner_device = SigningKey::generate();
    anyhow::ensure!(
        joiner_identity.identity_key() == ticket.invitee_key,
        "this ticket is bound to a different identity than the one generated above"
    );

    // Admit only the admin's device (bound from the ticket's discovery hint);
    // everyone else is rejected until the fold learns otherwise.
    let mut admission = AllowlistAdmission::new();
    for dev in &ticket.discovery {
        admission = admission.bind_device(
            EndpointId::from_bytes(dev.as_bytes())?,
            ticket.inviter_identity,
        );
    }
    admission = admission.set_active(ticket.inviter_identity);

    let store = EventStore::open_in_memory()?;
    let engine = SyncEngine::open(store, ticket.room_id, SyncConfig::default())
        .map_err(|e| anyhow::anyhow!("could not open sync engine: {e}"))?;
    let secret_key = SecretKey::from_bytes(&joiner_device.to_seed());
    let node = Node::spawn(
        secret_key,
        Arc::new(admission),
        Arc::new(iroh_rooms::experimental::session::TracingAudit),
        engine,
        NetConfig::default(),
        DEFAULT_TICK,
    )
    .await?;

    let admin_id: EndpointId = ADMIN_ENDPOINT.parse()?;
    node.connect_to(EndpointAddr::new(admin_id));
    node.wait_for_state(
        admin_id,
        iroh_rooms::experimental::session::PeerConnState::Connected,
        Duration::from_secs(10),
    )
    .await?;

    // The real `room join` polls until it locally resolves as `Invited` (the
    // membership sub-DAG is never windowed, so this always converges once
    // connected); this trimmed example just gives the anti-entropy tick a
    // moment to pull it.
    tokio::time::sleep(Duration::from_secs(1)).await;
    let heads = node.heads().await?;

    let binding = DeviceBinding::create(
        &ticket.room_id,
        &joiner_identity,
        joiner_device.device_key(),
    );
    let wire = build_member_joined(
        &joiner_identity,
        &joiner_device,
        &ticket.room_id,
        &ticket.invite_id,
        &ticket.capability_secret,
        &ticket.role,
        binding,
        None,
        &heads,
        now_ms(),
    );

    let ctx = ValidationContext::for_room(ticket.room_id);
    validate_wire_bytes(&wire.to_bytes(), &ctx).map_err(|reason| {
        anyhow::anyhow!("freshly built member.joined failed validation: {reason:?}")
    })?;
    node.publish(wire.to_bytes()).await?;

    tokio::time::sleep(Duration::from_millis(500)).await; // flush grace
    node.shutdown().await?;
    println!("joined room {}", ticket.room_id);
    Ok(())
}

#[cfg(feature = "experimental")]
fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

#[cfg(not(feature = "experimental"))]
fn main() {
    eprintln!("this example requires `--features experimental` (see the module doc header)");
}
