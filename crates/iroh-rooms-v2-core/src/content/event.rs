//! The concrete exact-byte content-event envelope, signature verifier, and
//! pure per-device chain validator (issue #152 §3.3–§3.5).
//!
//! The cryptographic trust boundary is the exact canonical body bytes
//! (`body_csb`). The [`EventId`] is recomputed as
//! `BLAKE3(CONTENT_EVENT || body_csb)` and the one detached Ed25519 signature
//! verifies under `CONTENT_EVENT || body_csb` using the **in-body `device_id`**
//! — never `author_id` and never an out-of-body signer (spec §3.3/§3.4/D3).
//!
//! Verification order (spec §7):
//! 1. canonical-decode the exact `body_csb` (`non_canonical_encoding` on fault);
//! 2. recompute + retain the [`EventId`] (claimed-ID comparison deferred to OQ-2);
//! 3. strict body + kind-specific validation (schema faults before crypto);
//! 4. verify `Ed25519(device_id, CONTENT_EVENT || body_csb, signature)`;
//! 5. promote to the non-forgeable [`VerifiedContentEvent`].
//!
//! Signature verification proves control of `device_id` only; it does NOT prove
//! that the device belongs to `author_id` or is currently authorized. Device
//! ownership, active/revoked status, and role are a separate governance
//! authorization stage and are out of scope here (spec §3.4/D3).

use crate::content::body::ContentEventBody;
use crate::domain::{self, CONTENT_EVENT};
use crate::error::Reject;
use crate::ids::{CommunityId, DeviceId, EventId, PrincipalId};
use crate::keys::{verify_device, Signature, SigningKey};

/// An untrusted content event: exact canonical body bytes (`body_csb`) plus one
/// detached Ed25519 signature. This is the wire/storage shape a receiver
/// presents to the verifier.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContentEvent {
    /// Exact canonical CBOR bytes of the [`ContentEventBody`] (the trust
    /// boundary). Retained verbatim; never re-serialized before verification.
    pub body_csb: Vec<u8>,
    /// One 64-byte detached Ed25519 signature over `CONTENT_EVENT || body_csb`.
    pub signature: Signature,
}

impl ContentEvent {
    /// Construct from exact body bytes and a signature.
    #[must_use]
    pub fn new(body_csb: Vec<u8>, signature: Signature) -> Self {
        Self {
            body_csb,
            signature,
        }
    }
}

/// A signature-verified content event. Retains the recomputed [`EventId`], the
/// exact received `body_csb`, the signature, and the decoded body. It is the
/// only value accepted as predecessor proof by [`validate_device_chain_link`].
///
/// Cannot be constructed through public fields: [`verify_content_event`] is the
/// sole promotion path from an untrusted [`ContentEvent`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedContentEvent {
    id: EventId,
    body_csb: Vec<u8>,
    signature: Signature,
    body: ContentEventBody,
}

impl VerifiedContentEvent {
    /// The recomputed content-event id (`BLAKE3(CONTENT_EVENT || body_csb)`).
    #[must_use]
    pub fn id(&self) -> EventId {
        self.id
    }

    /// The exact canonical body bytes that were verified (the trust boundary).
    #[must_use]
    pub fn body_csb(&self) -> &[u8] {
        &self.body_csb
    }

    /// The verified signature.
    #[must_use]
    pub fn signature(&self) -> &Signature {
        &self.signature
    }

    /// The decoded, strictly-validated body.
    #[must_use]
    pub fn body(&self) -> &ContentEventBody {
        &self.body
    }

    /// The community this event belongs to.
    #[must_use]
    pub fn community_id(&self) -> CommunityId {
        self.body.community_id
    }

    /// The device whose key signed this event.
    #[must_use]
    pub fn device_id(&self) -> DeviceId {
        self.body.device_id
    }

    /// The authoring principal identity.
    #[must_use]
    pub fn author_id(&self) -> PrincipalId {
        self.body.author_id
    }

    /// The per-`(community_id, device_id)` sequence number.
    #[must_use]
    pub fn device_seq(&self) -> u64 {
        self.body.device_seq
    }

    /// The named predecessor event id, if any.
    #[must_use]
    pub fn prev_device_event(&self) -> Option<EventId> {
        self.body.prev_device_event
    }
}

/// Verify an untrusted [`ContentEvent`] end-to-end and promote it to a
/// [`VerifiedContentEvent`] (spec §7).
///
/// Recomputes the [`EventId`] from the exact `body_csb`, runs full strict
/// body + kind-specific validation, then verifies the one Ed25519 signature
/// under the decoded `device_id`.
///
/// # Errors
/// - [`Reject::NonCanonicalEncoding`] — `body_csb` is not canonical CBOR.
/// - [`Reject::UnknownVersion`] / [`Reject::UnknownContentKind`] — version/kind.
/// - [`Reject::InvalidContent`] — any schema violation (incl. references cap).
/// - [`Reject::BadSignature`] — the signature does not verify under `device_id`.
pub fn verify_content_event(event: &ContentEvent) -> Result<VerifiedContentEvent, Reject> {
    // (2) Recompute + retain the id from the exact received bytes. A claimed-ID
    //     comparison is deferred until OQ-2 defines an envelope field for it.
    let id = EventId::from_content_event_csb(&event.body_csb);
    // (3) Strict body + kind-specific validation. This canonical-decodes the
    //     exact bytes (rejecting non-canonical encoding) and applies the full
    //     §6.4 schema before any cryptographic check.
    let body = ContentEventBody::decode_from_csb(&event.body_csb)?;
    // (4) Verify the one signature under the in-body device key over the frozen
    //     content-event domain + exact body bytes.
    let msg = domain::signing_message(CONTENT_EVENT, &event.body_csb);
    verify_device(&body.device_id, &msg, &event.signature).map_err(|_| Reject::BadSignature)?;
    // (5) Promote.
    Ok(VerifiedContentEvent {
        id,
        body_csb: event.body_csb.clone(),
        signature: event.signature,
        body,
    })
}

/// Locally build + sign a [`ContentEvent`] from a body and a device signing key
/// (spec §6 / §8 step 6).
///
/// Performs full strict body validation (round-trip through canonical CBOR) and
/// rejects if the signing key's derived [`DeviceId`] does not equal
/// `body.device_id` — the body is never silently rewritten to match the key.
/// The signature is over `CONTENT_EVENT || body_csb`.
///
/// # Errors
/// - [`Reject::InvalidContent`] / other body rejects — the body is not valid.
/// - [`Reject::BadSignature`] — the signing key does not match `body.device_id`.
pub fn seal_content_event(
    body: &ContentEventBody,
    key: &SigningKey,
) -> Result<ContentEvent, Reject> {
    // Strict validation of the locally-built body before signing.
    let csb = body.encode_canonical();
    ContentEventBody::decode_from_csb(&csb)?;
    // The signing key MUST be the device named in the body.
    if key.device_id() != body.device_id {
        return Err(Reject::BadSignature);
    }
    let msg = domain::signing_message(CONTENT_EVENT, &csb);
    let signature = key.sign(&msg);
    Ok(ContentEvent {
        body_csb: csb,
        signature,
    })
}

/// Validate the per-device chain link between a verified predecessor
/// (`previous`) and a verified `current` event (spec §3.5/D4).
///
/// This is a pure relational check over already-verified events; it performs no
/// network, store, or clock access. Intrinsic `(device_seq == 0) == prev is null`
/// shape is enforced during body validation; this function checks the successor
/// relationship.
///
/// - For a genesis event (`current.device_seq == 0`) predecessor data is not
///   consulted; supplying unrelated context is API misuse, not malformed bytes.
/// - For a successor (`current.device_seq > 0`), the named predecessor MUST be
///   supplied: `None` returns [`Reject::MissingDependency`] so a caller may
///   buffer/retry rather than treat the bytes as malformed.
/// - A supplied predecessor must satisfy: same `community_id`, `device_id`, and
///   `author_id`; `current.device_seq == previous.device_seq + 1` (checked, no
///   overflow); and `current.prev_device_event == previous.id()`. Any mismatch
///   returns [`Reject::InvalidContent`].
///
/// # Errors
/// - [`Reject::MissingDependency`] — a nonzero event's predecessor was not
///   supplied.
/// - [`Reject::InvalidContent`] — supplied predecessor data contradicts the
///   required chain invariants (incl. `device_seq` overflow).
pub fn validate_device_chain_link(
    previous: Option<&VerifiedContentEvent>,
    current: &VerifiedContentEvent,
) -> Result<(), Reject> {
    // Genesis events carry no relational predecessor check; their intrinsic
    // (device_seq == 0, null prev) shape was validated at body decode time.
    if current.device_seq() == 0 {
        return Ok(());
    }

    let Some(prev) = previous else {
        // The named predecessor is required but not supplied: defer so callers
        // can buffer out-of-order arrivals without declaring bytes malformed.
        return Err(Reject::MissingDependency);
    };

    // Same community/device/author continuity (spec §3.5 successor checks).
    if current.community_id() != prev.community_id()
        || current.device_id() != prev.device_id()
        || current.author_id() != prev.author_id()
    {
        return Err(Reject::InvalidContent);
    }
    // Checked sequence increment (u64::MAX has no valid successor).
    let expected_seq = prev
        .device_seq()
        .checked_add(1)
        .ok_or(Reject::InvalidContent)?;
    if current.device_seq() != expected_seq {
        return Err(Reject::InvalidContent);
    }
    // Exact predecessor id linkage.
    if current.prev_device_event() != Some(prev.id()) {
        return Err(Reject::InvalidContent);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cbor::CborValue;
    use crate::content::registry::ContentKind;
    use crate::ids::{CommunityId, EventId, StreamId, LEN};
    use crate::keys::SigningKey;

    fn body_at(seq: u64, prev: Option<EventId>, key: &SigningKey) -> ContentEventBody {
        ContentEventBody {
            v: crate::content::body::CONTENT_EVENT_VERSION,
            community_id: CommunityId::from_bytes([0x70; LEN]),
            stream_id: StreamId::from_bytes([0x71; LEN]),
            author_id: key.member_id(),
            device_id: key.device_id(),
            device_seq: seq,
            prev_device_event: prev,
            auth_hint_seq: seq,
            created_at_ms: 1_000_u64.saturating_add(seq),
            kind: ContentKind::MessageText,
            references: Vec::new(),
            content: CborValue::Map(vec![("body".to_owned(), CborValue::Text("hi".to_owned()))]),
        }
    }

    #[test]
    fn seal_and_verify_round_trip() {
        let key = SigningKey::from_seed(&[0x40; LEN]);
        let body = body_at(0, None, &key);
        let event = seal_content_event(&body, &key).expect("seal");
        let verified = verify_content_event(&event).expect("verify");
        assert_eq!(verified.body(), &body);
        assert_eq!(
            verified.id(),
            EventId::from_content_event_csb(&event.body_csb)
        );
        assert_eq!(verified.body_csb(), event.body_csb.as_slice());
    }

    #[test]
    fn signature_is_not_part_of_event_id_preimage() {
        let key = SigningKey::from_seed(&[0x41; LEN]);
        let body = body_at(0, None, &key);
        let event = seal_content_event(&body, &key).expect("seal");
        let id_before = EventId::from_content_event_csb(&event.body_csb);
        // Re-sign the same body with the same key: deterministic Ed25519 yields
        // the same signature, and the ID (derived from body bytes only) is
        // unchanged regardless of the signature.
        let event2 = seal_content_event(&body, &key).expect("seal again");
        assert_eq!(event2.signature, event.signature);
        assert_eq!(EventId::from_content_event_csb(&event2.body_csb), id_before);
        // Mutating ONLY the signature bytes leaves ID recomputation unchanged.
        let mut tampered_sig = *event.signature.as_bytes();
        tampered_sig[0] ^= 0x01;
        let tampered =
            ContentEvent::new(event.body_csb.clone(), Signature::from_bytes(tampered_sig));
        assert_eq!(
            EventId::from_content_event_csb(&tampered.body_csb),
            id_before,
            "signature bytes must not affect the ID"
        );
        // ...but the mutated signature must fail verification.
        assert_eq!(
            verify_content_event(&tampered).err(),
            Some(Reject::BadSignature)
        );
    }

    #[test]
    fn tampered_body_bytes_reject_as_bad_signature() {
        let key = SigningKey::from_seed(&[0x42; LEN]);
        let body = body_at(0, None, &key);
        let event = seal_content_event(&body, &key).expect("seal");
        // Produce a canonical, schema-valid tampered body (change the text) and
        // keep the original signature. The body decodes fine but the signature
        // no longer matches → bad_signature.
        let mut tampered_body = body;
        tampered_body.content = CborValue::Map(vec![(
            "body".to_owned(),
            CborValue::Text("tampered".to_owned()),
        )]);
        let tampered_csb = tampered_body.encode_canonical();
        let tampered = ContentEvent::new(tampered_csb, event.signature);
        assert_eq!(
            verify_content_event(&tampered).err(),
            Some(Reject::BadSignature)
        );
        // The tampered body has a different ID (different preimage).
        assert_ne!(
            EventId::from_content_event_csb(&tampered.body_csb),
            EventId::from_content_event_csb(&event.body_csb)
        );
    }

    #[test]
    fn wrong_device_key_rejects_as_bad_signature() {
        let key = SigningKey::from_seed(&[0x43; LEN]);
        let other = SigningKey::from_seed(&[0x44; LEN]);
        // Body names `key`'s device; the signature is produced by `other` under
        // the content-event domain. Verification under `body.device_id` (= key)
        // must reject as bad_signature.
        let body = body_at(0, None, &key);
        let csb = body.encode_canonical();
        let msg = domain::signing_message(CONTENT_EVENT, &csb);
        let wrong_sig = other.sign(&msg);
        let event = ContentEvent::new(csb, wrong_sig);
        assert_eq!(
            verify_content_event(&event).err(),
            Some(Reject::BadSignature)
        );
        // Sanity: the same body signed by the matching key verifies.
        let good = seal_content_event(&body, &key).expect("seal");
        verify_content_event(&good).expect("matching key verifies");
    }

    #[test]
    fn seal_rejects_key_device_mismatch() {
        let key = SigningKey::from_seed(&[0x45; LEN]);
        let other = SigningKey::from_seed(&[0x46; LEN]);
        let mut body = body_at(0, None, &key);
        body.device_id = other.device_id();
        assert_eq!(
            seal_content_event(&body, &key).err(),
            Some(Reject::BadSignature)
        );
    }

    #[test]
    fn five_event_device_sequence_validates() {
        let key = SigningKey::from_seed(&[0x47; LEN]);
        // Build events 0..=4, each naming the prior verified id as predecessor.
        let mut verified: Vec<VerifiedContentEvent> = Vec::new();
        let mut prev_id: Option<EventId> = None;
        for seq in 0u64..=4 {
            let body = body_at(seq, prev_id, &key);
            let event = seal_content_event(&body, &key).expect("seal");
            let current = verify_content_event(&event).expect("verify");
            // Validate the link against the immediately preceding verified event.
            let predecessor = if seq == 0 { None } else { verified.last() };
            validate_device_chain_link(predecessor, &current).expect("link valid");
            prev_id = Some(current.id());
            verified.push(current);
        }
        assert_eq!(verified.len(), 5);
    }

    #[test]
    fn nonzero_event_without_predecessor_defers() {
        let key = SigningKey::from_seed(&[0x48; LEN]);
        let prev = EventId::from_bytes([0x99; LEN]);
        let body = body_at(1, Some(prev), &key);
        let event = seal_content_event(&body, &key).expect("seal");
        let current = verify_content_event(&event).expect("verify");
        assert_eq!(
            validate_device_chain_link(None, &current).err(),
            Some(Reject::MissingDependency)
        );
    }

    #[test]
    fn sequence_gap_rejects() {
        let key = SigningKey::from_seed(&[0x49; LEN]);
        let body0 = body_at(0, None, &key);
        let e0 = seal_content_event(&body0, &key).unwrap();
        let v0 = verify_content_event(&e0).unwrap();
        // A successor claiming seq 3 (gap) but pointing at v0's id.
        let body3 = body_at(3, Some(v0.id()), &key);
        let e3 = seal_content_event(&body3, &key).unwrap();
        let v3 = verify_content_event(&e3).unwrap();
        assert_eq!(
            validate_device_chain_link(Some(&v0), &v3).err(),
            Some(Reject::InvalidContent)
        );
    }

    #[test]
    fn wrong_predecessor_id_rejects() {
        let key = SigningKey::from_seed(&[0x4a; LEN]);
        let wrong_prev = EventId::from_bytes([0xee; LEN]);
        let body = body_at(1, Some(wrong_prev), &key);
        let event = seal_content_event(&body, &key).unwrap();
        let current = verify_content_event(&event).unwrap();
        // Supply an unrelated verified predecessor.
        let body0 = body_at(0, None, &key);
        let e0 = seal_content_event(&body0, &key).unwrap();
        let v0 = verify_content_event(&e0).unwrap();
        assert_eq!(
            validate_device_chain_link(Some(&v0), &current).err(),
            Some(Reject::InvalidContent)
        );
    }

    #[test]
    fn overflow_after_max_seq_rejects() {
        // A predecessor at device_seq = u64::MAX has no valid successor (its
        // checked +1 overflows). The link validator must reject a claimed
        // successor without panicking or wrapping.
        let key = SigningKey::from_seed(&[0x4b; LEN]);
        let prev_id = EventId::from_bytes([0x11; LEN]);

        let mut prev_body = body_at(u64::MAX, Some(prev_id), &key);
        // body_at sets device_seq from the arg; force the maximum explicitly.
        prev_body.device_seq = u64::MAX;
        let pe = seal_content_event(&prev_body, &key).unwrap();
        let pv = verify_content_event(&pe).unwrap();

        // A "successor" also claiming u64::MAX (there is no u64::MAX+1) naming
        // the predecessor by id: the checked increment overflows → reject.
        let mut cur_body = body_at(u64::MAX, Some(pv.id()), &key);
        cur_body.device_seq = u64::MAX;
        let ce = seal_content_event(&cur_body, &key).unwrap();
        let cv = verify_content_event(&ce).unwrap();
        assert_eq!(
            validate_device_chain_link(Some(&pv), &cv).err(),
            Some(Reject::InvalidContent),
            "successor of a u64::MAX predecessor must reject without panic/wrap"
        );
    }
}
