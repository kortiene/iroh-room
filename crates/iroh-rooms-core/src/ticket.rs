//! The out-of-band room invite **ticket**: the copy-pasteable secret carrier that
//! travels alongside an on-log `member.invited` event (spec IR-0103 D4).
//!
//! A [`RoomInviteTicket`] carries everything a joiner needs that is **not** on the
//! event log: the capability **secret** (whose BLAKE3 hash is the only invite
//! artifact written to the log — Spike §6/§7, AC3), plus the room id, invite id,
//! bound invitee key, role, optional expiry, the inviter identity (provenance),
//! and discovery hints (the admin's `device_id` / `EndpointId`).
//!
//! ## Why this lives in `core`
//!
//! Placing the ticket here (not the CLI) lets the sibling `room join` flow decode
//! it without duplicating the codec, exactly as
//! [`build_member_invited`](crate::event::build_member_invited) is shared. The
//! type is a plain value object that happens to hold a secret, so its [`Debug`] is
//! **redacted** (the secret is masked) — the only place the secret is rendered is
//! the deliberate [`Display`] token.
//!
//! ## Token encoding
//!
//! ```text
//! roomtkt1<base32-lowercase-nopad( version(1B) ‖ canonical-CBOR(body) ‖ blake3_checksum(4B) )>
//! ```
//!
//! * The body is the deterministic-CBOR map of the fields, reusing the landed core
//!   codec ([`crate::event::cbor`]) — the same canonical profile the rest of the
//!   protocol uses, so no new CBOR dependency and byte-exact round-trips.
//! * `version = 1` allows forward-compatible format changes.
//! * A 4-byte BLAKE3 checksum makes a truncated/garbled paste fail closed in
//!   [`FromStr`].
//! * HRP `roomtkt` + separator `1` mirrors the `roomtkt1…` form shown in
//!   `docs/getting-started.md`. Base32 (RFC 4648, lowercase, no padding) keeps the
//!   token compact and copy-paste-safe; the `1` separator can never appear in the
//!   base32 body (its alphabet is `a-z2-7`), so the prefix is unambiguous.

use core::fmt;
use core::str::FromStr;

use crate::event::capability_hash;
use crate::event::cbor::{self, CborValue};
use crate::event::constants::{DIGEST_LEN, PUBLIC_KEY_LEN, SHORT_ID_LEN};
use crate::event::ids::RoomId;
use crate::event::keys::{DeviceKey, IdentityKey};

/// Human-readable prefix + bech32-style separator that opens every token.
const TICKET_PREFIX: &str = "roomtkt1";
/// On-token format version (the first decoded byte).
const TICKET_VERSION: u8 = 1;
/// Truncated-BLAKE3 checksum length, in bytes, appended after the CBOR body.
const TICKET_CHECKSUM_LEN: usize = 4;

/// A failure decoding a [`RoomInviteTicket`] from its text token. Every variant is
/// a **fail-closed** outcome — a malformed or corrupted ticket never decodes to a
/// usable capability.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum TicketError {
    /// The token did not start with the `roomtkt1` prefix.
    BadPrefix,
    /// The body was not valid lowercase base32 (RFC 4648, no padding).
    BadBase32,
    /// The decoded payload was too short to hold a version byte and checksum.
    Truncated,
    /// The version byte was not a supported value.
    UnsupportedVersion(u8),
    /// The trailing checksum did not match the payload (corrupted/garbled paste).
    BadChecksum,
    /// The CBOR body was not canonical, or a field was missing/mistyped/out of range.
    MalformedBody,
}

impl fmt::Display for TicketError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BadPrefix => write!(f, "not an invite ticket (missing `{TICKET_PREFIX}` prefix)"),
            Self::BadBase32 => f.write_str("invite ticket is not valid base32"),
            Self::Truncated => f.write_str("invite ticket is truncated"),
            Self::UnsupportedVersion(v) => {
                write!(f, "unsupported invite-ticket version {v}")
            }
            Self::BadChecksum => {
                f.write_str("invite ticket failed its checksum (corrupted on copy-paste?)")
            }
            Self::MalformedBody => f.write_str("invite ticket body is malformed"),
        }
    }
}

impl std::error::Error for TicketError {}

/// An out-of-band, key-bound room invite ticket (spec IR-0103 D4).
///
/// The capability **secret** lives only here and in nothing written to the event
/// log. Round-trips byte-exactly through [`Display`] → [`FromStr`].
#[derive(Clone, PartialEq, Eq)]
pub struct RoomInviteTicket {
    /// The room this invite authorizes joining.
    pub room_id: RoomId,
    /// The invite handle, matching the on-log `member.invited.invite_id`.
    pub invite_id: [u8; SHORT_ID_LEN],
    /// The out-of-band capability secret. Its hash (and only its hash) is on the log.
    pub capability_secret: [u8; SHORT_ID_LEN],
    /// The identity key this invite is bound to (only this key may join with it).
    pub invitee_key: IdentityKey,
    /// The invited role (`member` | `agent`).
    pub role: String,
    /// Optional expiry (ms since the Unix epoch); `None` ⇒ no expiry.
    pub expires_at: Option<u64>,
    /// The inviting admin's identity (`sender_id`), for provenance.
    pub inviter_identity: IdentityKey,
    /// Discovery hints — MVP carries the admin's `device_id` (`EndpointId`).
    pub discovery: Vec<DeviceKey>,
}

impl RoomInviteTicket {
    /// Recompute the capability hash this ticket's secret implies (Event Protocol
    /// §7): `BLAKE3-256(INVITE_CONTEXT ‖ room_id ‖ invite_id ‖ secret)`.
    ///
    /// AC4: this MUST equal the `capability_hash` carried by the matching on-log
    /// `member.invited` event.
    #[must_use]
    pub fn capability_hash(&self) -> [u8; DIGEST_LEN] {
        capability_hash(&self.room_id, &self.invite_id, &self.capability_secret)
    }

    /// The deterministic-CBOR body map (everything but the version/checksum frame).
    fn to_cbor_body(&self) -> CborValue {
        let mut entries = vec![
            (
                "room_id".to_owned(),
                CborValue::Bytes(self.room_id.as_bytes().to_vec()),
            ),
            (
                "invite_id".to_owned(),
                CborValue::Bytes(self.invite_id.to_vec()),
            ),
            (
                "secret".to_owned(),
                CborValue::Bytes(self.capability_secret.to_vec()),
            ),
            (
                "invitee".to_owned(),
                CborValue::Bytes(self.invitee_key.as_bytes().to_vec()),
            ),
            ("role".to_owned(), CborValue::Text(self.role.clone())),
            (
                "inviter".to_owned(),
                CborValue::Bytes(self.inviter_identity.as_bytes().to_vec()),
            ),
            (
                "discovery".to_owned(),
                CborValue::Array(
                    self.discovery
                        .iter()
                        .map(|d| CborValue::Bytes(d.as_bytes().to_vec()))
                        .collect(),
                ),
            ),
        ];
        if let Some(expiry) = self.expires_at {
            entries.push(("expires_at".to_owned(), CborValue::Uint(expiry)));
        }
        CborValue::Map(entries)
    }

    /// Parse a decoded CBOR body map back into a ticket.
    fn from_cbor_body(value: &CborValue) -> Result<Self, TicketError> {
        let entries = value.as_map().ok_or(TicketError::MalformedBody)?;
        let room_id = RoomId::from_bytes(fixed::<DIGEST_LEN>(entries, "room_id")?);
        let invite_id = fixed::<SHORT_ID_LEN>(entries, "invite_id")?;
        let capability_secret = fixed::<SHORT_ID_LEN>(entries, "secret")?;
        let invitee_key = IdentityKey::from_bytes(fixed::<PUBLIC_KEY_LEN>(entries, "invitee")?);
        let role = get(entries, "role")
            .and_then(CborValue::as_text)
            .ok_or(TicketError::MalformedBody)?
            .to_owned();
        let inviter_identity =
            IdentityKey::from_bytes(fixed::<PUBLIC_KEY_LEN>(entries, "inviter")?);
        let discovery = device_array(entries, "discovery")?;
        // `expires_at` is optional; present-but-mistyped is a malformation.
        let expires_at = match get(entries, "expires_at") {
            Some(v) => Some(v.as_uint().ok_or(TicketError::MalformedBody)?),
            None => None,
        };
        Ok(Self {
            room_id,
            invite_id,
            capability_secret,
            invitee_key,
            role,
            expires_at,
            inviter_identity,
            discovery,
        })
    }
}

impl fmt::Display for RoomInviteTicket {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // payload = version(1B) ‖ canonical-CBOR(body); checksum is over the payload.
        let mut payload = Vec::with_capacity(64);
        payload.push(TICKET_VERSION);
        payload.extend_from_slice(&cbor::encode(&self.to_cbor_body()));
        let checksum = blake3::hash(&payload);
        payload.extend_from_slice(&checksum.as_bytes()[..TICKET_CHECKSUM_LEN]);
        // RFC 4648 base32 is ASCII A–Z2–7; lowercasing is reversible and locale-free.
        let body = data_encoding::BASE32_NOPAD
            .encode(&payload)
            .to_ascii_lowercase();
        write!(f, "{TICKET_PREFIX}{body}")
    }
}

impl FromStr for RoomInviteTicket {
    type Err = TicketError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let body = s
            .strip_prefix(TICKET_PREFIX)
            .ok_or(TicketError::BadPrefix)?;
        let raw = data_encoding::BASE32_NOPAD
            .decode(body.to_ascii_uppercase().as_bytes())
            .map_err(|_| TicketError::BadBase32)?;
        // Must hold at least a version byte and the checksum.
        if raw.len() < 1 + TICKET_CHECKSUM_LEN {
            return Err(TicketError::Truncated);
        }
        let version = raw[0];
        if version != TICKET_VERSION {
            return Err(TicketError::UnsupportedVersion(version));
        }
        let (payload, checksum) = raw.split_at(raw.len() - TICKET_CHECKSUM_LEN);
        let digest = blake3::hash(payload);
        if checksum != &digest.as_bytes()[..TICKET_CHECKSUM_LEN] {
            return Err(TicketError::BadChecksum);
        }
        // payload = version ‖ cbor; the body is everything after the version byte.
        let value =
            cbor::decode_canonical(&payload[1..]).map_err(|_| TicketError::MalformedBody)?;
        Self::from_cbor_body(&value)
    }
}

impl fmt::Debug for RoomInviteTicket {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Redacted: the capability secret is NEVER rendered through Debug.
        f.debug_struct("RoomInviteTicket")
            .field("room_id", &self.room_id)
            .field("invite_id", &hex::encode(self.invite_id))
            .field("capability_secret", &"<redacted>")
            .field("invitee_key", &self.invitee_key)
            .field("role", &self.role)
            .field("expires_at", &self.expires_at)
            .field("inviter_identity", &self.inviter_identity)
            .field("discovery", &self.discovery)
            .finish()
    }
}

// ---------------------------------------------------------------------------
// Small CBOR-body field readers (fail closed on any shape violation).
// ---------------------------------------------------------------------------

fn get<'a>(entries: &'a [(String, CborValue)], key: &str) -> Option<&'a CborValue> {
    entries.iter().find(|(k, _)| k == key).map(|(_, v)| v)
}

fn fixed<const N: usize>(
    entries: &[(String, CborValue)],
    key: &str,
) -> Result<[u8; N], TicketError> {
    let bytes = get(entries, key)
        .and_then(CborValue::as_bytes)
        .ok_or(TicketError::MalformedBody)?;
    <[u8; N]>::try_from(bytes).map_err(|_| TicketError::MalformedBody)
}

fn device_array(entries: &[(String, CborValue)], key: &str) -> Result<Vec<DeviceKey>, TicketError> {
    let items = get(entries, key)
        .and_then(CborValue::as_array)
        .ok_or(TicketError::MalformedBody)?;
    items
        .iter()
        .map(|item| {
            let bytes = item.as_bytes().ok_or(TicketError::MalformedBody)?;
            let arr =
                <[u8; PUBLIC_KEY_LEN]>::try_from(bytes).map_err(|_| TicketError::MalformedBody)?;
            Ok(DeviceKey::from_bytes(arr))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{RoomInviteTicket, TicketError, TICKET_CHECKSUM_LEN, TICKET_PREFIX};
    use crate::event::capability_hash;
    use crate::event::ids::RoomId;
    use crate::event::keys::SigningKey;
    use core::str::FromStr;

    fn sample(expires_at: Option<u64>) -> RoomInviteTicket {
        let invitee = SigningKey::from_seed(&[0x04; 32]).identity_key();
        let admin = SigningKey::from_seed(&[0x01; 32]).identity_key();
        let device = SigningKey::from_seed(&[0x02; 32]).device_key();
        RoomInviteTicket {
            room_id: RoomId::from_bytes([0x11; 32]),
            invite_id: [0xda; 16],
            capability_secret: [0x5e; 16],
            invitee_key: invitee,
            role: "member".to_owned(),
            expires_at,
            inviter_identity: admin,
            discovery: vec![device],
        }
    }

    #[test]
    fn round_trips_with_expiry() {
        let t = sample(Some(1_750_000_086_400_000));
        let s = t.to_string();
        assert!(
            s.starts_with(TICKET_PREFIX),
            "token must carry the HRP: {s}"
        );
        let parsed = RoomInviteTicket::from_str(&s).expect("token must round-trip");
        assert_eq!(parsed, t);
    }

    #[test]
    fn round_trips_without_expiry() {
        let t = sample(None);
        let parsed = RoomInviteTicket::from_str(&t.to_string()).expect("round-trip");
        assert_eq!(parsed, t);
        assert_eq!(parsed.expires_at, None);
    }

    #[test]
    fn round_trips_with_multiple_discovery_hints() {
        let mut t = sample(None);
        t.discovery
            .push(SigningKey::from_seed(&[0x09; 32]).device_key());
        let parsed = RoomInviteTicket::from_str(&t.to_string()).expect("round-trip");
        assert_eq!(parsed.discovery, t.discovery);
    }

    #[test]
    fn capability_hash_matches_event_derivation() {
        let t = sample(None);
        // AC4: the ticket's recomputed hash equals the canonical §7 derivation.
        assert_eq!(
            t.capability_hash(),
            capability_hash(&t.room_id, &t.invite_id, &t.capability_secret)
        );
    }

    #[test]
    fn token_body_is_lowercase_base32() {
        let s = sample(None).to_string();
        let body = s.strip_prefix(TICKET_PREFIX).unwrap();
        assert!(
            body.chars().all(|c| matches!(c, 'a'..='z' | '2'..='7')),
            "base32 body must be lowercase a-z2-7: {body}"
        );
    }

    #[test]
    fn rejects_missing_prefix() {
        assert_eq!(
            RoomInviteTicket::from_str("nothello"),
            Err(TicketError::BadPrefix)
        );
    }

    #[test]
    fn rejects_corrupted_checksum() {
        let s = sample(None).to_string();
        // Flip the last base32 char (part of the checksum region).
        let mut chars: Vec<char> = s.chars().collect();
        let last = chars.len() - 1;
        chars[last] = if chars[last] == 'a' { 'b' } else { 'a' };
        let corrupted: String = chars.into_iter().collect();
        assert!(
            matches!(
                RoomInviteTicket::from_str(&corrupted),
                Err(TicketError::BadChecksum | TicketError::MalformedBody | TicketError::Truncated)
            ),
            "a corrupted token must fail closed"
        );
    }

    #[test]
    fn rejects_truncated_token() {
        // Just the prefix and a stray byte → not enough for version + checksum.
        assert!(RoomInviteTicket::from_str(&format!("{TICKET_PREFIX}aa")).is_err());
    }

    #[test]
    fn rejects_non_base32_body() {
        // '1', '8', '0', '9' are not in the RFC 4648 base32 alphabet.
        assert_eq!(
            RoomInviteTicket::from_str(&format!("{TICKET_PREFIX}1809")),
            Err(TicketError::BadBase32)
        );
    }

    #[test]
    fn debug_redacts_the_secret() {
        let t = sample(None);
        let dbg = format!("{t:?}");
        assert!(dbg.contains("<redacted>"), "Debug must mask the secret");
        assert!(
            !dbg.contains(&hex::encode(t.capability_secret)),
            "Debug must not contain the raw secret hex"
        );
    }

    // ── version gating ───────────────────────────────────────────────────────

    #[test]
    fn rejects_unsupported_version() {
        // Craft a well-formed token whose version byte is 2 (not 1).
        let valid = sample(None).to_string();
        let body = valid.strip_prefix(TICKET_PREFIX).unwrap();
        let mut raw = data_encoding::BASE32_NOPAD
            .decode(body.to_ascii_uppercase().as_bytes())
            .unwrap();
        // Overwrite the version byte then recompute the 4-byte trailing checksum.
        raw[0] = 2;
        let payload_end = raw.len() - TICKET_CHECKSUM_LEN;
        let new_checksum = blake3::hash(&raw[..payload_end]);
        raw[payload_end..].copy_from_slice(&new_checksum.as_bytes()[..TICKET_CHECKSUM_LEN]);
        let crafted = format!(
            "{}{}",
            TICKET_PREFIX,
            data_encoding::BASE32_NOPAD
                .encode(&raw)
                .to_ascii_lowercase()
        );
        assert_eq!(
            RoomInviteTicket::from_str(&crafted),
            Err(TicketError::UnsupportedVersion(2))
        );
    }

    // ── edge-case round-trips ────────────────────────────────────────────────

    #[test]
    fn empty_discovery_hints_round_trip() {
        let mut t = sample(None);
        t.discovery = vec![];
        let parsed = RoomInviteTicket::from_str(&t.to_string()).expect("round-trip with no hints");
        assert_eq!(parsed.discovery, Vec::<_>::new());
    }

    // ── key-binding (AC2) at the ticket level ────────────────────────────────

    #[test]
    fn different_invitees_produce_different_tokens_and_binding_survives() {
        // AC2: each invitee key produces a distinct token, and the bound key
        // round-trips exactly.
        let alice = SigningKey::from_seed(&[0x04; 32]).identity_key();
        let bob = SigningKey::from_seed(&[0x05; 32]).identity_key();
        let tok_alice = {
            let mut t = sample(None);
            t.invitee_key = alice;
            t.to_string()
        };
        let tok_bob = {
            let mut t = sample(None);
            t.invitee_key = bob;
            t.to_string()
        };
        assert_ne!(
            tok_alice, tok_bob,
            "distinct invitees must produce distinct tokens"
        );
        // The parsed ticket carries the exact key it was built with.
        let parsed = RoomInviteTicket::from_str(&tok_alice).unwrap();
        assert_eq!(parsed.invitee_key, alice);
        assert_ne!(parsed.invitee_key, bob);
    }

    // ── capability_hash input isolation (AC4 substrate) ──────────────────────

    #[test]
    fn capability_hash_depends_on_secret() {
        let mut t1 = sample(None);
        t1.capability_secret = [0xaa; 16];
        let mut t2 = sample(None);
        t2.capability_secret = [0xbb; 16];
        assert_ne!(
            t1.capability_hash(),
            t2.capability_hash(),
            "different secrets must yield different capability hashes"
        );
    }

    #[test]
    fn capability_hash_depends_on_room_id() {
        let mut t1 = sample(None);
        t1.room_id = RoomId::from_bytes([0x11; 32]);
        let mut t2 = sample(None);
        t2.room_id = RoomId::from_bytes([0x22; 32]);
        assert_ne!(
            t1.capability_hash(),
            t2.capability_hash(),
            "different room_ids must yield different capability hashes"
        );
    }

    #[test]
    fn capability_hash_depends_on_invite_id() {
        let mut t1 = sample(None);
        t1.invite_id = [0xaa; 16];
        let mut t2 = sample(None);
        t2.invite_id = [0xbb; 16];
        assert_ne!(
            t1.capability_hash(),
            t2.capability_hash(),
            "different invite_ids must yield different capability hashes"
        );
    }

    // ── ticket/event hash consistency ────────────────────────────────────────

    #[test]
    fn ticket_hash_matches_standalone_capability_hash() {
        // AC4: RoomInviteTicket::capability_hash() delegates to the same
        // canonical derivation used by the event builder — same inputs,
        // same output, no intermediate divergence.
        let t = sample(Some(1_750_000_086_400_000));
        assert_eq!(
            t.capability_hash(),
            capability_hash(&t.room_id, &t.invite_id, &t.capability_secret)
        );
    }
}
