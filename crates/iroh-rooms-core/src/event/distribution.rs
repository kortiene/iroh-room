//! Pure `member.key_distribution` and rotation-bearing `member.removed` event
//! assembly (#191 step 6, spec `content-key-rotation.md` D4/D5/D6).
//!
//! This is the single byte-exact place a `member.key_distribution` event is
//! assembled, and the place a `member.removed` gains its optional inline
//! rotation payload. Like the sibling builders, the functions here are
//! deterministic in their inputs: the caller injects `prev_events`,
//! `created_at`, and the per-recipient wrapping is driven by the crypto crate
//! (which draws fresh ephemerals/nonces). The only RNG in `core` stays inside
//! [`SigningKey::generate`](super::keys::SigningKey::generate) and the crypto
//! crate's `wrap_room_key`.

use iroh_rooms_crypto::{room_key_commitment, wrap_room_key, CryptoError, RoomKey};

use super::binding::DeviceBinding;
use super::constants::SCHEMA_VERSION;
use super::content::{Content, EventType, MemberKeyDistribution, MemberRemoved, WrappedKeyEntry};
use super::ids::{EventId, RoomId};
use super::keys::{DeviceKey, IdentityKey, SigningKey};
use super::signed::{self, SignedEvent};
use super::wire::WireEvent;

/// Why a distribution payload could not be built. Local authoring error only.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum DistributionError {
    /// The recipient set is empty. A distribution must target at least one
    /// remaining Active member.
    EmptyRecipients,
    /// The crypto crate refused to wrap for one recipient (bad device key).
    WrapFailure,
}

impl core::fmt::Display for DistributionError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::EmptyRecipients => f.write_str("distribution_empty_recipients"),
            Self::WrapFailure => f.write_str("distribution_wrap_failure"),
        }
    }
}

impl std::error::Error for DistributionError {}

impl From<CryptoError> for DistributionError {
    fn from(_: CryptoError) -> Self {
        Self::WrapFailure
    }
}

/// Build the `MemberKeyDistribution` payload for `new_epoch`.
///
/// Wraps `room_key` for every recipient device using `SUITE_V1` (spec D3/D3a),
/// computes the D5 commitment, and returns the typed content. The caller is
/// responsible for ensuring the recipient list excludes the removed member and
/// includes only currently-Active members' bound devices.
///
/// The `room_id` passed here MUST be the same room the event will be signed
/// for; the commitment binds `new_epoch_be8 || room_id || room_key`.
///
/// # Errors
/// [`DistributionError::EmptyRecipients`] if `recipients` is empty.
/// [`DistributionError::WrapFailure`] if wrapping fails for any recipient.
pub fn build_key_distribution_content(
    room_id: &RoomId,
    new_epoch: u64,
    room_key: &RoomKey,
    recipients: &[DeviceKey],
) -> Result<MemberKeyDistribution, DistributionError> {
    if recipients.is_empty() {
        return Err(DistributionError::EmptyRecipients);
    }
    let key_commitment = room_key_commitment(room_key, room_id.as_bytes(), new_epoch);
    let mut wrapped_keys = Vec::with_capacity(recipients.len());
    for device in recipients {
        let wrapped = wrap_room_key(room_key, device.as_bytes(), room_id.as_bytes(), new_epoch)?;
        wrapped_keys.push((
            *device,
            WrappedKeyEntry {
                ephemeral_public: wrapped.ephemeral_public,
                nonce: wrapped.nonce,
                ciphertext: wrapped.ciphertext,
            },
        ));
    }
    Ok(MemberKeyDistribution {
        new_epoch,
        key_commitment,
        wrapped_keys,
    })
}

/// Assemble and sign a standalone `member.key_distribution` event (spec D4).
///
/// This is used for post-removal/initial key distribution in rooms where the
/// inline `member.removed.rotation` payload is not appropriate (e.g. a
/// proactive rotation, or a join-time catch-up distribution). It is signed by
/// the admin's device secret and carries the same payload shape as the inline
/// rotation.
///
/// # Errors
/// [`DistributionError::EmptyRecipients`] or [`DistributionError::WrapFailure`].
#[allow(clippy::too_many_arguments)] // mirrors build_member_removed; each arg distinct
pub fn build_member_key_distribution(
    admin_identity_secret: &SigningKey,
    admin_device_secret: &SigningKey,
    room_id: &RoomId,
    new_epoch: u64,
    room_key: &RoomKey,
    recipients: &[DeviceKey],
    prev_events: &[EventId],
    created_at: u64,
) -> Result<WireEvent, DistributionError> {
    let sender_id = admin_identity_secret.identity_key();
    let device_id = admin_device_secret.device_key();
    let distribution = build_key_distribution_content(room_id, new_epoch, room_key, recipients)?;

    let event = SignedEvent {
        schema_version: SCHEMA_VERSION,
        room_id: *room_id,
        sender_id,
        device_id,
        event_type: EventType::MemberKeyDistribution,
        created_at,
        prev_events: prev_events.to_vec(),
        content: Content::MemberKeyDistribution(distribution),
    };
    let csb = event.to_csb();
    let sig = signed::sign_csb(&csb, admin_device_secret);
    Ok(WireEvent::seal(csb, sig))
}

/// Assemble and sign a `member.removed` event with an inline rotation payload
/// (spec D4/D5).
///
/// This is the rotation-aware removal path: the removed member is excluded from
/// `recipients`, and the remaining Active members receive wrapped keys for
/// `new_epoch`. The rotation payload is omitted when `new_epoch`/`room_key` are
/// `None`, in which case this delegates to [`removed::build_member_removed`].
///
/// # Errors
/// [`DistributionError::EmptyRecipients`] if a rotation is requested but the
/// recipient list is empty. [`DistributionError::WrapFailure`] if wrapping
/// fails.
#[allow(clippy::too_many_arguments)]
pub fn build_member_removed_with_rotation(
    admin_identity_secret: &SigningKey,
    admin_device_secret: &SigningKey,
    room_id: &RoomId,
    subject: &IdentityKey,
    subject_device: Option<DeviceKey>,
    reason: Option<&str>,
    device_binding: Option<DeviceBinding>,
    new_epoch: Option<u64>,
    room_key: Option<&RoomKey>,
    recipients: &[DeviceKey],
    prev_events: &[EventId],
    created_at: u64,
) -> Result<WireEvent, DistributionError> {
    // The removed subject must never receive the new epoch key (G1). Filter
    // its device from the caller-provided recipient list; if the list is
    // empty after filtering and a rotation was requested, fail closed.
    let recipients: Vec<DeviceKey> = recipients
        .iter()
        .copied()
        .filter(|d| Some(*d) != subject_device)
        .collect();
    let rotation = match (new_epoch, room_key) {
        (Some(epoch), Some(key)) => Some(build_key_distribution_content(
            room_id,
            epoch,
            key,
            &recipients,
        )?),
        _ => None,
    };

    let sender_id = admin_identity_secret.identity_key();
    let device_id = admin_device_secret.device_key();
    let content = Content::MemberRemoved(MemberRemoved {
        member_id: *subject,
        removed_by: sender_id,
        reason: reason.map(ToOwned::to_owned),
        device_binding,
        rotation,
    });
    let event = SignedEvent {
        schema_version: SCHEMA_VERSION,
        room_id: *room_id,
        sender_id,
        device_id,
        event_type: EventType::MemberRemoved,
        created_at,
        prev_events: prev_events.to_vec(),
        content,
    };
    let csb = event.to_csb();
    let sig = signed::sign_csb(&csb, admin_device_secret);
    Ok(WireEvent::seal(csb, sig))
}

#[cfg(test)]
mod tests {
    use super::{
        build_key_distribution_content, build_member_key_distribution,
        build_member_removed_with_rotation, DistributionError,
    };
    use crate::event::content::Content;
    use crate::event::ids::{EventId, RoomId};
    use crate::event::keys::SigningKey;
    use crate::event::signed;
    use crate::event::validate::{validate_wire_bytes, ValidationContext};
    use iroh_rooms_crypto::RoomKey;

    const ADMIN_IDENTITY_SEED: [u8; 32] = [0x01; 32];
    const ADMIN_DEVICE_SEED: [u8; 32] = [0x02; 32];
    const RECIPIENT_SEED: [u8; 32] = [0x03; 32];
    const SUBJECT_SEED: [u8; 32] = [0x04; 32];
    const ROOM_NONCE: [u8; 16] = [
        0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d, 0x0e,
        0x0f,
    ];
    const CREATED_AT: u64 = 1_750_000_000_000;

    fn admin_keys() -> (SigningKey, SigningKey) {
        (
            SigningKey::from_seed(&ADMIN_IDENTITY_SEED),
            SigningKey::from_seed(&ADMIN_DEVICE_SEED),
        )
    }

    fn fixture_room_id() -> RoomId {
        let admin = SigningKey::from_seed(&ADMIN_IDENTITY_SEED);
        signed::derive_room_id(&admin.identity_key(), &ROOM_NONCE, CREATED_AT)
    }

    fn fixture_heads() -> Vec<EventId> {
        vec![EventId::from_bytes([0xab; 32])]
    }

    fn recipient_device() -> crate::event::keys::DeviceKey {
        SigningKey::from_seed(&RECIPIENT_SEED).device_key()
    }

    #[test]
    fn distribution_content_requires_at_least_one_recipient() {
        let room_id = fixture_room_id();
        let key = RoomKey::from_bytes([0xAA; 32]);
        assert!(matches!(
            build_key_distribution_content(&room_id, 1, &key, &[]).expect_err("empty"),
            DistributionError::EmptyRecipients
        ));
    }

    #[test]
    fn distribution_content_round_trips_commitment_and_wrapped_keys() {
        let room_id = fixture_room_id();
        let key = RoomKey::from_bytes([0xAA; 32]);
        let recipients = vec![recipient_device()];
        let d = build_key_distribution_content(&room_id, 2, &key, &recipients).expect("build");
        assert_eq!(d.new_epoch, 2);
        assert_eq!(
            d.key_commitment,
            iroh_rooms_crypto::room_key_commitment(&key, room_id.as_bytes(), 2)
        );
        assert_eq!(d.wrapped_keys.len(), 1);
        assert_eq!(d.wrapped_keys[0].0, recipient_device());
    }

    #[test]
    fn member_key_distribution_builds_and_validates() {
        let (id, dev) = admin_keys();
        let room_id = fixture_room_id();
        let key = RoomKey::from_bytes([0xBB; 32]);
        let wire = build_member_key_distribution(
            &id,
            &dev,
            &room_id,
            1,
            &key,
            &[recipient_device()],
            &fixture_heads(),
            CREATED_AT,
        )
        .expect("build");
        validate_wire_bytes(&wire.to_bytes(), &ValidationContext::for_room(room_id))
            .expect("fresh distribution must validate");
        let event = signed::SignedEvent::decode(&wire.signed).expect("decode");
        assert_eq!(
            event.event_type,
            crate::event::content::EventType::MemberKeyDistribution
        );
    }

    #[test]
    fn member_removed_with_rotation_builds_and_validates() {
        let (id, dev) = admin_keys();
        let room_id = fixture_room_id();
        let subject = SigningKey::from_seed(&SUBJECT_SEED).identity_key();
        let key = RoomKey::from_bytes([0xCC; 32]);
        let wire = build_member_removed_with_rotation(
            &id,
            &dev,
            &room_id,
            &subject,
            None,
            Some("policy violation"),
            None,
            Some(3),
            Some(&key),
            &[recipient_device()],
            &fixture_heads(),
            CREATED_AT,
        )
        .expect("build");
        validate_wire_bytes(&wire.to_bytes(), &ValidationContext::for_room(room_id))
            .expect("rotation removal must validate");
        let event = signed::SignedEvent::decode(&wire.signed).expect("decode");
        let Content::MemberRemoved(c) = event.content else {
            panic!("expected member.removed");
        };
        assert!(c.rotation.is_some(), "rotation payload must be present");
        let rotation = c.rotation.unwrap();
        assert_eq!(rotation.new_epoch, 3);
        assert_eq!(rotation.wrapped_keys.len(), 1);
    }

    #[test]
    fn member_removed_without_rotation_omits_payload() {
        let (id, dev) = admin_keys();
        let room_id = fixture_room_id();
        let subject = SigningKey::from_seed(&SUBJECT_SEED).identity_key();
        let wire = build_member_removed_with_rotation(
            &id,
            &dev,
            &room_id,
            &subject,
            None,
            None,
            None,
            None,
            None,
            &[],
            &fixture_heads(),
            CREATED_AT,
        )
        .expect("build");
        let event = signed::SignedEvent::decode(&wire.signed).expect("decode");
        let Content::MemberRemoved(c) = event.content else {
            panic!("expected member.removed");
        };
        assert!(c.rotation.is_none());
    }

    #[test]
    fn rotation_requires_recipients() {
        let (id, dev) = admin_keys();
        let room_id = fixture_room_id();
        let subject = SigningKey::from_seed(&SUBJECT_SEED).identity_key();
        let key = RoomKey::from_bytes([0xDD; 32]);
        assert!(matches!(
            build_member_removed_with_rotation(
                &id,
                &dev,
                &room_id,
                &subject,
                None,
                None,
                None,
                Some(1),
                Some(&key),
                &[],
                &fixture_heads(),
                CREATED_AT,
            )
            .expect_err("empty recipients"),
            DistributionError::EmptyRecipients
        ));
    }
}
