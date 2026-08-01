#!/usr/bin/env python3
"""Independent D2 content-AAD reference for the golden vectors in
`encrypted_write_path.rs` (#191 step 4).

This script derives every pinned byte string of the R2 encrypted-content
write path from a SECOND implementation stack — OpenSSL via the
`cryptography` package (Ed25519 / AES-256-GCM), the third-party `blake3`
package (room_id / event_id), and a hand-rolled canonical-CBOR emitter that
mirrors the deterministic profile of `iroh-rooms-core/src/event/cbor.rs`
(shortest-form heads; map keys sorted bytewise over their encoded form,
i.e. length-first then bytewise) — so the Rust implementation must agree
byte-for-byte with code that shares none of its source (the spec section 5
golden-vector discipline, mirroring `suite_v1_reference.py`).

Derived and printed as Rust-ready hex constants:

- the normative D2 AAD (`ENCRYPTED_AAD_CONTEXT` + canonical-CBOR 10-array);
- the sealed ciphertext (AES-256-GCM under the golden room key);
- the full event CSB (canonical-CBOR 8-field map) and `event_id`
  (BLAKE3-256 of the CSB);
- the Ed25519 device signature over `EVENT_CONTEXT || CSB` (spec section 5:
  "pin ... exact bytes and signature").

The golden cast shares the step-3 envelope fixtures (`encrypted_envelope.rs`):
identity seed 01*32, device seed 02*32, room nonce 00..0f, room created_at
1_750_000_000_000, event created_at 1_750_000_006_000, parent 11*32,
key_epoch 7, room key 00..1f, nonce a0..ab, body "Hello room"/"plain" —
so the step-4 vector differs from the step-3 placeholder-AAD pin only in
the AAD and therefore the ciphertext.

Run: python3 encrypted_aad_reference.py   (needs: pip install cryptography blake3)
It prints the pins and asserts internal consistency (seal/open round-trip,
signature self-verification).
"""

import blake3
from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PrivateKey
from cryptography.hazmat.primitives.ciphers.aead import AESGCM

# --- protocol constants (constants.rs, verbatim) -----------------------------
EVENT_CONTEXT = b"iroh-rooms:event:v1"
ROOMID_CONTEXT = b"iroh-rooms:room-id:v1"
ENCRYPTED_AAD_CONTEXT = b"iroh-rooms:content-aad:v1"
SCHEMA_VERSION = 1
SUITE_V1 = 0x01

# --- fixed golden inputs (the step-3 golden cast) ----------------------------
IDENTITY_SEED = bytes([0x01] * 32)
DEVICE_SEED = bytes([0x02] * 32)
ROOM_NONCE = bytes(range(0x00, 0x10))
ROOM_CREATED_AT = 1_750_000_000_000
ENCRYPTED_CREATED_AT = 1_750_000_006_000
PARENT_ID = bytes([0x11] * 32)
KEY_EPOCH = 7
ROOM_KEY = bytes(range(0x00, 0x20))
NONCE = bytes(range(0xA0, 0xAC))
INNER_TYPE = "message.text"
EVENT_TYPE = "content.encrypted"
BODY = "Hello room"
FORMAT = "plain"


# --- canonical CBOR (deterministic profile of event/cbor.rs) -----------------
def head(major: int, arg: int) -> bytes:
    """Shortest-form CBOR item head."""
    if arg <= 0x17:
        return bytes([(major << 5) | arg])
    if arg <= 0xFF:
        return bytes([(major << 5) | 0x18, arg])
    if arg <= 0xFFFF:
        return bytes([(major << 5) | 0x19]) + arg.to_bytes(2, "big")
    if arg <= 0xFFFF_FFFF:
        return bytes([(major << 5) | 0x1A]) + arg.to_bytes(4, "big")
    return bytes([(major << 5) | 0x1B]) + arg.to_bytes(8, "big")


def uint(n: int) -> bytes:
    return head(0, n)


def bstr(b: bytes) -> bytes:
    return head(2, len(b)) + b


def tstr(s: str) -> bytes:
    e = s.encode()
    return head(3, len(e)) + e


def array(items: list[bytes]) -> bytes:
    return head(4, len(items)) + b"".join(items)


def cmap(entries: dict[str, bytes]) -> bytes:
    """Canonical map: keys sorted bytewise over their encoded text form
    (shortest-form head means length-first, then bytewise)."""
    enc = sorted((tstr(k), v) for k, v in entries.items())
    return head(5, len(enc)) + b"".join(k + v for k, v in enc)


# --- key material ------------------------------------------------------------
identity_secret = Ed25519PrivateKey.from_private_bytes(IDENTITY_SEED)
device_secret = Ed25519PrivateKey.from_private_bytes(DEVICE_SEED)
sender_id = identity_secret.public_key().public_bytes_raw()
device_id = device_secret.public_key().public_bytes_raw()

# --- room_id (Event Protocol section 5) --------------------------------------
room_id = blake3.blake3(
    ROOMID_CONTEXT + sender_id + ROOM_NONCE + ROOM_CREATED_AT.to_bytes(8, "big")
).digest()

# --- plaintext body: canonical CBOR of the golden message.text ---------------
plaintext = cmap({"body": tstr(BODY), "format": tstr(FORMAT)})

# --- normative D2 AAD: context || canonical-CBOR 10-array --------------------
aad = ENCRYPTED_AAD_CONTEXT + array(
    [
        uint(SCHEMA_VERSION),
        bstr(room_id),
        bstr(sender_id),
        bstr(device_id),
        tstr(EVENT_TYPE),
        uint(ENCRYPTED_CREATED_AT),
        array([bstr(PARENT_ID)]),
        tstr(INNER_TYPE),
        uint(KEY_EPOCH),
        uint(SUITE_V1),
    ]
)

# --- seal (AES-256-GCM; ciphertext = body || 16-byte tag) --------------------
ciphertext = AESGCM(ROOM_KEY).encrypt(NONCE, plaintext, aad)
assert AESGCM(ROOM_KEY).decrypt(NONCE, ciphertext, aad) == plaintext
assert len(ciphertext) == len(plaintext) + 16

# --- envelope content map + CSB (8-field map) + id + signature ---------------
content = cmap(
    {
        "inner_type": tstr(INNER_TYPE),
        "key_epoch": uint(KEY_EPOCH),
        "suite": uint(SUITE_V1),
        "nonce": bstr(NONCE),
        "ciphertext": bstr(ciphertext),
    }
)
csb = cmap(
    {
        "schema_version": uint(SCHEMA_VERSION),
        "room_id": bstr(room_id),
        "sender_id": bstr(sender_id),
        "device_id": bstr(device_id),
        "event_type": tstr(EVENT_TYPE),
        "created_at": uint(ENCRYPTED_CREATED_AT),
        "prev_events": array([bstr(PARENT_ID)]),
        "content": content,
    }
)
event_id = blake3.blake3(csb).digest()
signature = device_secret.sign(EVENT_CONTEXT + csb)
device_secret.public_key().verify(signature, EVENT_CONTEXT + csb)

print(f'const GOLDEN_ROOM_ID_HEX: &str = "{room_id.hex()}";')
print(f'const GOLDEN_AAD_HEX: &str = "{aad.hex()}";')
print(f'const GOLDEN_PLAINTEXT_HEX: &str = "{plaintext.hex()}";')
print(f'const GOLDEN_CIPHERTEXT_HEX: &str = "{ciphertext.hex()}";')
print(f'const GOLDEN_R2_CSB_HEX: &str = "{csb.hex()}";')
print(f'const GOLDEN_R2_EVENT_ID_HEX: &str = "{event_id.hex()}";')
print(f'const GOLDEN_R2_SIGNATURE_HEX: &str = "{signature.hex()}";')
