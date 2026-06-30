//! [`PipeRegistry`] — the owner's table of locally-open pipes (spec §6.5.1).
//!
//! It maps `pipe_id → OpenPipe { opened, target }`, where `target` is the **real**
//! loopback forward address. The target lives **only** here — never on the log
//! (`pipe.opened.target_hint` is advisory and never trusted, spec §6.1), so a
//! connector cannot redirect the owner's TCP connection. Loopback-target
//! enforcement (D6 / PRD §13.2.3) lives at insert time.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Mutex;

use iroh_rooms_core::event::content::PipeOpened;

use super::error::PipeError;

/// A locally-open pipe: its governing announcement plus the real loopback target.
#[derive(Debug, Clone)]
pub struct OpenPipe {
    /// The signed announcement (the same bytes published on the log).
    pub opened: PipeOpened,
    /// The real loopback forward target (owner-local; never on the log).
    pub target: SocketAddr,
}

/// Whether `addr` is a loopback address (`127.0.0.0/8` or `::1`) — the only target
/// the prototype will forward to (D6 / PRD §13.2.3).
#[must_use]
pub fn is_loopback_target(addr: &SocketAddr) -> bool {
    addr.ip().is_loopback()
}

/// The owner's registry of open pipes (`pipe_id → OpenPipe`).
#[derive(Debug, Default)]
pub struct PipeRegistry {
    pipes: Mutex<HashMap<[u8; 16], OpenPipe>>,
}

impl PipeRegistry {
    /// An empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Register an open pipe, enforcing the loopback-target rule (D6).
    ///
    /// # Errors
    /// [`PipeError::NonLoopbackTarget`] if `target` is not a loopback address.
    pub fn insert(&self, opened: PipeOpened, target: SocketAddr) -> Result<(), PipeError> {
        if !is_loopback_target(&target) {
            return Err(PipeError::NonLoopbackTarget(target));
        }
        let pipe_id = opened.pipe_id;
        self.pipes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(pipe_id, OpenPipe { opened, target });
        Ok(())
    }

    /// The real loopback target for `pipe_id`, if this node owns an open pipe for it.
    /// `None` means "not an open pipe here" — the handler treats that as a `closed`
    /// reject (fail-closed; the target removal on close is itself a denial).
    #[must_use]
    pub fn target(&self, pipe_id: &[u8; 16]) -> Option<SocketAddr> {
        self.pipes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(pipe_id)
            .map(|p| p.target)
    }

    /// Remove an open pipe (on close / owner exit). Returns the removed entry.
    pub fn remove(&self, pipe_id: &[u8; 16]) -> Option<OpenPipe> {
        self.pipes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(pipe_id)
    }

    /// Whether `pipe_id` is currently registered (locally open) here.
    #[must_use]
    pub fn contains(&self, pipe_id: &[u8; 16]) -> bool {
        self.pipes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .contains_key(pipe_id)
    }

    /// The `pipe_id`s currently open here, for the watcher / `pipe list`.
    #[must_use]
    pub fn open_ids(&self) -> Vec<[u8; 16]> {
        self.pipes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .keys()
            .copied()
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::{is_loopback_target, PipeRegistry};
    use iroh_rooms_core::event::content::PipeOpened;
    use iroh_rooms_core::event::keys::{DeviceKey, IdentityKey};
    use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr};

    fn opened(pipe_id: [u8; 16]) -> PipeOpened {
        PipeOpened {
            pipe_id,
            owner_id: IdentityKey::from_bytes([0x01; 32]),
            owner_endpoint: DeviceKey::from_bytes([0x02; 32]),
            kind: "tcp".to_owned(),
            label: "dev".to_owned(),
            target_hint: "localhost:3000".to_owned(),
            alpn: "/iroh-rooms/pipe/1".to_owned(),
            allowed_members: vec![IdentityKey::from_bytes([0x10; 32])],
            expires_at: None,
        }
    }

    fn loopback(port: u16) -> SocketAddr {
        SocketAddr::from((Ipv4Addr::LOCALHOST, port))
    }

    #[test]
    fn loopback_v4_and_v6_are_accepted() {
        assert!(is_loopback_target(&loopback(3000)));
        assert!(is_loopback_target(&SocketAddr::from((
            Ipv6Addr::LOCALHOST,
            3000
        ))));
        // 127.x.y.z is all loopback.
        assert!(is_loopback_target(&SocketAddr::from((
            Ipv4Addr::new(127, 4, 5, 6),
            80
        ))));
    }

    #[test]
    fn non_loopback_targets_are_rejected() {
        assert!(!is_loopback_target(&SocketAddr::from((
            Ipv4Addr::new(10, 0, 0, 1),
            22
        ))));
        assert!(!is_loopback_target(&SocketAddr::from((
            Ipv4Addr::UNSPECIFIED,
            80
        ))));
        assert!(!is_loopback_target(&SocketAddr::from((
            Ipv4Addr::new(192, 168, 1, 2),
            443
        ))));
    }

    #[test]
    fn insert_rejects_non_loopback_target() {
        let reg = PipeRegistry::new();
        let err = reg
            .insert(
                opened([0xaa; 16]),
                SocketAddr::from((Ipv4Addr::new(8, 8, 8, 8), 53)),
            )
            .expect_err("a non-loopback target must be rejected");
        assert!(err.to_string().starts_with("non_loopback_target:"));
        assert!(!reg.contains(&[0xaa; 16]));
    }

    #[test]
    fn insert_then_target_and_remove() {
        let reg = PipeRegistry::new();
        reg.insert(opened([0xbb; 16]), loopback(3000))
            .expect("insert");
        assert!(reg.contains(&[0xbb; 16]));
        assert_eq!(reg.target(&[0xbb; 16]), Some(loopback(3000)));
        assert_eq!(reg.open_ids(), vec![[0xbb; 16]]);
        let removed = reg.remove(&[0xbb; 16]).expect("removed");
        assert_eq!(removed.target, loopback(3000));
        assert!(!reg.contains(&[0xbb; 16]));
        assert_eq!(reg.target(&[0xbb; 16]), None);
    }

    #[test]
    fn open_ids_returns_all_registered_pipes() {
        let reg = PipeRegistry::new();
        reg.insert(opened([0x11; 16]), loopback(4001))
            .expect("insert a");
        reg.insert(opened([0x22; 16]), loopback(4002))
            .expect("insert b");
        reg.insert(opened([0x33; 16]), loopback(4003))
            .expect("insert c");
        let mut ids = reg.open_ids();
        ids.sort_unstable(); // HashMap order is not deterministic
        assert_eq!(ids, vec![[0x11; 16], [0x22; 16], [0x33; 16]]);
        assert_eq!(reg.open_ids().len(), 3);
    }

    #[test]
    fn insert_same_pipe_id_twice_replaces_the_target() {
        // The registry uses HashMap::insert which silently replaces. The second
        // loopback target must win; the first must be unreachable afterward.
        let reg = PipeRegistry::new();
        reg.insert(opened([0xcc; 16]), loopback(5001))
            .expect("first insert");
        reg.insert(opened([0xcc; 16]), loopback(5002))
            .expect("second insert");
        assert_eq!(
            reg.target(&[0xcc; 16]),
            Some(loopback(5002)),
            "second insert must overwrite the first target"
        );
        assert_eq!(reg.open_ids(), vec![[0xcc; 16]], "still only one entry");
    }
}
