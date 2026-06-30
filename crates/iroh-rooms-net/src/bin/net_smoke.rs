//! `net-smoke` — a two-mode harness for the full-mesh QUIC event transport
//! (spec §5 step 10 / G7). It drives one [`Node`] (a [`NetTransport`] + a real
//! [`SyncEngine`]) and is used for both the local loopback demo and the Gate-A
//! two-machine real-network run (spec §7.3).
//!
//! ```text
//! net-smoke listen [--real]
//! net-smoke dial <ENDPOINT_ID> [--addr <IP:PORT>] [--real] [--reject]
//! ```
//!
//! * `listen` binds as the room host (seed 1), publishes the room genesis, prints
//!   its dialable address, and reports peer connection-state transitions.
//! * `dial <ENDPOINT_ID>` connects to a listener and waits to receive the genesis
//!   `WireEvent` over the ALPN (AC1), printing the time-to-first-event.
//! * `--reject` dials with a **non-member** key (seed 99) to demonstrate the
//!   accept-gate refusing an unknown endpoint (AC2): the dialer observes
//!   `Unauthorized`, never `Connected`.
//! * `--real` uses the n0 DNS + relay stack for a cross-NAT run (Gate A); the
//!   default is loopback (`RelayMode::Disabled`).

use std::net::SocketAddr;
use std::str::FromStr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context, Result};
use iroh::{EndpointAddr, EndpointId};
use iroh_rooms_core::store::EventStore;
use iroh_rooms_core::sync::{SyncConfig, SyncEngine};
use iroh_rooms_net::{
    demo, AllowlistAdmission, NetConfig, NetMode, Node, PeerConnState, TracingAudit, DEFAULT_TICK,
};

/// Wait budget for connect / first-event in the smoke run.
const WAIT: Duration = Duration::from_secs(15);

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_target(false)
        .init();

    let args: Vec<String> = std::env::args().skip(1).collect();
    let cmd = args.first().map(String::as_str);
    let real = args.iter().any(|a| a == "--real");
    let reject = args.iter().any(|a| a == "--reject");
    let mode = if real {
        NetMode::RealNetwork
    } else {
        NetMode::Loopback
    };

    match cmd {
        Some("listen") => run_listen(mode).await,
        Some("dial") => {
            let id_str = args
                .get(1)
                .filter(|s| !s.starts_with("--"))
                .ok_or_else(|| anyhow!("dial requires <ENDPOINT_ID>"))?;
            let host_id = EndpointId::from_str(id_str)
                .map_err(|e| anyhow!("invalid endpoint id {id_str:?}: {e}"))?;
            let addr = flag_value(&args, "--addr")
                .map(|s| SocketAddr::from_str(&s).context("invalid --addr"))
                .transpose()?;
            run_dial(mode, host_id, addr, reject).await
        }
        _ => {
            eprintln!(
                "usage:\n  net-smoke listen [--real]\n  net-smoke dial <ENDPOINT_ID> [--addr <IP:PORT>] [--real] [--reject]"
            );
            bail!("unrecognized command");
        }
    }
}

/// Bind as the room host, publish the genesis, and report transitions forever.
async fn run_listen(mode: NetMode) -> Result<()> {
    let host = demo::Participant::new(1);
    let expected_dialer = demo::Participant::new(2);
    let (room, genesis_id, genesis_bytes) = demo::genesis(&host);

    // The host admits itself and the expected dialer (seed 2); a `--reject` dialer
    // (seed 99) is deliberately absent → the accept-gate refuses it.
    let admission = demo::allowlist(&[&host, &expected_dialer]);
    let node = spawn_node(host.iroh_secret(), admission, room, mode).await?;

    node.publish(genesis_bytes)
        .await
        .context("publish room genesis")?;

    let addr = node.endpoint_addr()?;
    println!("== net-smoke listener ==");
    println!("mode        : {mode:?}");
    println!("endpoint id : {}", node.id());
    print_addr_hints(&addr);
    println!("room genesis: {genesis_id}");
    println!("waiting for dialers (Ctrl-C to stop)...");

    // Report connection-state transitions as they happen.
    let mut events = node.conn_events();
    loop {
        match events.recv().await {
            Ok(ev) => println!(
                "[state] {} : {} -> {}",
                ev.device,
                ev.from.label(),
                ev.to.label()
            ),
            Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                println!("[state] (lagged {n})");
            }
            Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
        }
    }
    Ok(())
}

/// Dial a listener and wait to receive the genesis event (or to be rejected).
async fn run_dial(
    mode: NetMode,
    host_id: EndpointId,
    addr: Option<SocketAddr>,
    reject: bool,
) -> Result<()> {
    // The host's deterministic identity (seed 1) lets us recompute the room id and
    // genesis id without any out-of-band exchange beyond the endpoint id.
    let host = demo::Participant::new(1);
    let (room, genesis_id, _) = demo::genesis(&host);

    let me = if reject {
        demo::Participant::new(99) // not in the host's allowlist
    } else {
        demo::Participant::new(2)
    };
    // The dialer admits the host (so its own accept side would work too).
    let admission = demo::allowlist(&[&me, &host]);
    let node = spawn_node(me.iroh_secret(), admission, room, mode).await?;

    let mut dial_addr = EndpointAddr::new(host_id);
    if let Some(socket) = addr {
        dial_addr = dial_addr.with_ip_addr(socket);
    }

    println!("== net-smoke dialer ==");
    println!("mode        : {mode:?}");
    println!("endpoint id : {}", node.id());
    println!("dialing host: {host_id}");
    if reject {
        println!("(reject demo: dialing with a NON-member key)");
    }

    let started = Instant::now();
    node.connect_to(dial_addr);

    if reject {
        node.wait_for_state(host_id, PeerConnState::Unauthorized, WAIT)
            .await
            .context("expected the accept-gate to reject this non-member dialer")?;
        println!(
            "[reject] host refused admission as expected after {:?}; state = unauthorized",
            started.elapsed()
        );
        return node.shutdown().await;
    }

    node.wait_for_state(host_id, PeerConnState::Connected, WAIT)
        .await
        .context("connect to host")?;
    println!("[connect] established in {:?}", started.elapsed());

    node.wait_until_contains(genesis_id, WAIT)
        .await
        .context("receive genesis WireEvent")?;
    println!(
        "[event] received signed genesis {genesis_id} over the ALPN in {:?}",
        started.elapsed()
    );
    println!("AC1 demonstrated: a signed WireEvent crossed the custom ALPN.");

    for (device, state) in node.peer_states() {
        println!("[final] {device} : {}", state.label());
    }
    node.shutdown().await
}

/// Build an in-memory engine for `room` and spawn a [`Node`] over it.
async fn spawn_node(
    secret: iroh::SecretKey,
    admission: AllowlistAdmission,
    room: iroh_rooms_core::event::ids::RoomId,
    mode: NetMode,
) -> Result<Node> {
    let store = EventStore::open_in_memory().context("open in-memory event store")?;
    let engine =
        SyncEngine::open(store, room, SyncConfig::default()).context("open sync engine")?;
    let cfg = NetConfig {
        mode,
        ..NetConfig::default()
    };
    Node::spawn(
        secret,
        Arc::new(admission),
        Arc::new(TracingAudit),
        engine,
        cfg,
        DEFAULT_TICK,
    )
    .await
}

/// Print the dialable hints from an [`EndpointAddr`] (id + relay/direct addrs).
fn print_addr_hints(addr: &EndpointAddr) {
    if addr.addrs.is_empty() {
        println!("addr hints  : (none yet; for --real, discovery/relay may take a moment)");
    } else {
        for ta in &addr.addrs {
            println!("addr hint   : {ta}");
        }
    }
}

/// Read the value following a `--flag` in the argument list.
fn flag_value(args: &[String], flag: &str) -> Option<String> {
    args.iter()
        .position(|a| a == flag)
        .and_then(|i| args.get(i + 1))
        .cloned()
}
