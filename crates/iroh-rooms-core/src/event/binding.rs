//! Device-binding certificates (Event Protocol §1).
//!
//! Events are signed by `device_id` but authorized against `sender_id`, so every
//! device is attested by its identity key via a detached signature carried in
//! `room.created`, `member.joined`, and `member.removed` content:
//!
//! ```text
//! binding_msg = BIND_CONTEXT ‖ room_id(32) ‖ sender_id(32) ‖ device_id(32)
//! binding_sig = Ed25519_sign(identity_secret, binding_msg)
//! accept iff Ed25519_verify(sender_id, binding_msg, binding_sig)
//! ```
//!
//! Verifying a binding is **self-contained crypto** (no external state) and is
//! therefore in scope for this stateless layer.

use super::cbor::CborValue;
use super::constants::{BIND_CONTEXT, PUBLIC_KEY_LEN, SIGNATURE_LEN};
use super::ids::RoomId;
use super::keys::{DeviceKey, IdentityKey, Signature, SigningKey};
use super::reject::RejectReason;

/// `DeviceBinding = { "identity_key": bstr[32], "device_key": bstr[32], "sig": bstr[64] }`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceBinding {
    /// The attesting identity key (`sender_id`).
    pub identity_key: IdentityKey,
    /// The attested device key (`device_id`).
    pub device_key: DeviceKey,
    /// `Ed25519_sign(identity_secret, binding_msg)`.
    pub sig: Signature,
}

/// Build the binding message for a `(room_id, identity_key, device_key)` triple.
#[must_use]
pub fn binding_message(
    room_id: &RoomId,
    identity_key: &IdentityKey,
    device_key: &DeviceKey,
) -> Vec<u8> {
    let mut msg = Vec::with_capacity(BIND_CONTEXT.len() + 3 * PUBLIC_KEY_LEN);
    msg.extend_from_slice(BIND_CONTEXT);
    msg.extend_from_slice(room_id.as_bytes());
    msg.extend_from_slice(identity_key.as_bytes());
    msg.extend_from_slice(device_key.as_bytes());
    msg
}

impl DeviceBinding {
    /// Author a device binding: the identity key signs over the binding message.
    #[must_use]
    pub fn create(room_id: &RoomId, identity_secret: &SigningKey, device_key: DeviceKey) -> Self {
        let identity_key = identity_secret.identity_key();
        let msg = binding_message(room_id, &identity_key, &device_key);
        let sig = identity_secret.sign(&msg);
        Self {
            identity_key,
            device_key,
            sig,
        }
    }

    /// Verify the binding signature for `room_id`. Self-contained; no state.
    ///
    /// # Errors
    /// Returns [`RejectReason::InvalidContent`] if the signature does not verify
    /// under `identity_key`.
    pub fn verify(&self, room_id: &RoomId) -> Result<(), RejectReason> {
        let msg = binding_message(room_id, &self.identity_key, &self.device_key);
        self.identity_key
            .verify_binding(&msg, &self.sig)
            .map_err(|_| RejectReason::InvalidContent)
    }

    /// Encode to its canonical CBOR map value.
    #[must_use]
    pub fn to_cbor(&self) -> CborValue {
        CborValue::Map(vec![
            (
                "identity_key".to_owned(),
                CborValue::Bytes(self.identity_key.as_bytes().to_vec()),
            ),
            (
                "device_key".to_owned(),
                CborValue::Bytes(self.device_key.as_bytes().to_vec()),
            ),
            (
                "sig".to_owned(),
                CborValue::Bytes(self.sig.as_bytes().to_vec()),
            ),
        ])
    }

    /// Strictly parse a `DeviceBinding` from a CBOR map value: exactly the three
    /// keys, each a byte string of the correct length, no extras.
    ///
    /// # Errors
    /// Returns [`RejectReason::InvalidContent`] on any structural mismatch.
    pub fn from_cbor(value: &CborValue) -> Result<Self, RejectReason> {
        let entries = value.as_map().ok_or(RejectReason::InvalidContent)?;
        if entries.len() != 3 {
            return Err(RejectReason::InvalidContent);
        }
        let mut identity_key: Option<IdentityKey> = None;
        let mut device_key: Option<DeviceKey> = None;
        let mut sig: Option<Signature> = None;
        for (key, val) in entries {
            match key.as_str() {
                "identity_key" => {
                    identity_key =
                        Some(IdentityKey::from_bytes(fixed_bytes::<PUBLIC_KEY_LEN>(val)?));
                }
                "device_key" => {
                    device_key = Some(DeviceKey::from_bytes(fixed_bytes::<PUBLIC_KEY_LEN>(val)?));
                }
                "sig" => {
                    sig = Some(Signature::from_bytes(fixed_bytes::<SIGNATURE_LEN>(val)?));
                }
                _ => return Err(RejectReason::InvalidContent),
            }
        }
        Ok(Self {
            identity_key: identity_key.ok_or(RejectReason::InvalidContent)?,
            device_key: device_key.ok_or(RejectReason::InvalidContent)?,
            sig: sig.ok_or(RejectReason::InvalidContent)?,
        })
    }
}

/// Extract a fixed-length byte array from a CBOR byte string, or fail closed.
fn fixed_bytes<const N: usize>(value: &CborValue) -> Result<[u8; N], RejectReason> {
    let bytes = value.as_bytes().ok_or(RejectReason::InvalidContent)?;
    <[u8; N]>::try_from(bytes).map_err(|_| RejectReason::InvalidContent)
}
