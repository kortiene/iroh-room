//! Diagnostic-only network-state classification (spec IR-0303 §5.3): direct-vs-relay
//! path classification and relay-url extraction, read from iroh's live transport
//! state.
//!
//! Ported (verbatim match-arm logic) from `crates/spike-nat`
//! (`probe.rs::classify_remote_info` / `report.rs::PathType`) — both crates pin the
//! identical `iroh = "=1.0.1"`, so the `RemoteInfo`/`TransportAddr`/
//! `TransportAddrUsage` surface is byte-for-byte the same; this is a mechanical
//! copy, not a version adaptation.
//!
//! **Advisory only, never a trust input** (mirrors [`crate::state::OfflineReason`]):
//! a [`PathType`] is a read-only observation of iroh's transport state for a human
//! to self-diagnose a P2P failure (PRD §18.1 "clear connection state" / §18.5 "hide
//! networking details unless needed"); it never feeds an authorization decision.

use iroh::endpoint::{RemoteInfo, TransportAddrUsage};
use iroh::TransportAddr;

/// The path type a connection has settled on — read from iroh's `remote_info`
/// *active* transport-address set, **never** inferred from latency or RTT (iroh
/// 1.0.1 has no `ConnectionType` watcher; project memory "iroh 1.0.1 has no
/// `ConnectionType` watcher").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PathType {
    /// A direct, hole-punched UDP path (an active IP transport address).
    Direct,
    /// A relay-only path (an active relay transport address, no active direct one).
    Relay,
    /// Direct and relay both active — a transitional state.
    Mixed,
    /// No usable path resolved (neither direct nor relay active).
    None,
}

impl PathType {
    /// The lowercase label the CLI's `diag:` block renders (spec §5.3).
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Direct => "direct",
            Self::Relay => "relay",
            Self::Mixed => "mixed",
            Self::None => "none",
        }
    }

    /// Whether a direct, hole-punched path was achieved. Only a *pure* direct path
    /// counts: [`Self::Mixed`] still has an active relay path, so it is not yet
    /// fully hole-punched (spec §5.3 "settle nuance").
    #[must_use]
    pub fn is_hole_punched(self) -> bool {
        matches!(self, Self::Direct)
    }
}

/// Classify a peer's live path from iroh's *active* transport-address set (spec
/// §5.3). `None` (no `remote_info` yet — the link has not settled, or the peer is
/// unknown) classifies honestly as [`PathType::None`] rather than blocking or
/// guessing. Ported verbatim from `spike-nat::probe::classify_remote_info`
/// (`probe.rs:464-496`).
#[must_use]
pub fn classify_remote_info(info: Option<&RemoteInfo>) -> (PathType, Option<String>) {
    let Some(info) = info else {
        return (PathType::None, None);
    };
    let mut has_direct = false;
    let mut has_relay = false;
    let mut active_relay: Option<String> = None;
    let mut any_relay: Option<String> = None;
    for addr in info.addrs() {
        let active = matches!(addr.usage(), TransportAddrUsage::Active);
        match addr.addr() {
            TransportAddr::Ip(_) => has_direct |= active,
            TransportAddr::Relay(url) => {
                let url = url.to_string();
                if active {
                    has_relay = true;
                    active_relay.get_or_insert(url.clone());
                }
                any_relay.get_or_insert(url);
            }
            // `Custom` and any future non-exhaustive variant are neither a direct
            // IP path nor a relay path for classification purposes.
            _ => {}
        }
    }
    let path_type = match (has_direct, has_relay) {
        (true, true) => PathType::Mixed,
        (true, false) => PathType::Direct,
        (false, true) => PathType::Relay,
        (false, false) => PathType::None,
    };
    (path_type, active_relay.or(any_relay))
}

#[cfg(test)]
mod tests {
    use super::{classify_remote_info, PathType};

    #[test]
    fn labels_are_stable() {
        assert_eq!(PathType::Direct.label(), "direct");
        assert_eq!(PathType::Relay.label(), "relay");
        assert_eq!(PathType::Mixed.label(), "mixed");
        assert_eq!(PathType::None.label(), "none");
    }

    #[test]
    fn only_a_pure_direct_path_is_hole_punched() {
        assert!(PathType::Direct.is_hole_punched());
        assert!(!PathType::Relay.is_hole_punched());
        assert!(!PathType::Mixed.is_hole_punched());
        assert!(!PathType::None.is_hole_punched());
    }

    #[test]
    fn classify_remote_info_none_is_no_path() {
        // `None` from `Endpoint::remote_info` means iroh has no record of the peer
        // yet (unsettled link, or unknown peer) — no path, no relay url. `RemoteInfo`
        // has no public constructor (its fields are private to the `iroh` crate), so
        // only the `None` branch is unit-testable here; the Direct/Relay/Mixed arms
        // are exercised end-to-end by the `--verbose` diagnostics CLI tests, mirroring
        // `spike-nat::probe::tests::classify_remote_info_none_is_no_path`.
        let (path_type, relay_url) = classify_remote_info(None);
        assert_eq!(
            path_type,
            PathType::None,
            "unknown peer must classify as no path"
        );
        assert!(relay_url.is_none(), "unknown peer has no relay url");
    }
}
