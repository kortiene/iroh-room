#!/usr/bin/env python3
"""Independent SUITE_V1 reference for the golden vectors in `suite_v1.rs`.

This script derives every pinned byte string in the Rust golden-vector tests
from a SECOND implementation stack (OpenSSL via the `cryptography` package for
Ed25519 / X25519 / HKDF-SHA-256 / AES-256-GCM, plus a hand-rolled
Edwards->Montgomery birational map), so the Rust crate
(`curve25519-dalek` + RustCrypto) must agree byte-for-byte with an
implementation that shares none of its code (spec `content-key-rotation.md`
D3: "two implementations must agree byte-for-byte").

Run: python3 suite_v1_reference.py
It prints the vectors as Rust-ready hex constants and asserts internal
consistency (both DH directions, wrap round-trip).
"""

import hashlib
from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PrivateKey
from cryptography.hazmat.primitives.asymmetric.x25519 import (
    X25519PrivateKey,
    X25519PublicKey,
)
from cryptography.hazmat.primitives.ciphers.aead import AESGCM
from cryptography.hazmat.primitives.kdf.hkdf import HKDF
from cryptography.hazmat.primitives.hashes import SHA256

P = 2**255 - 19
D = (-121665 * pow(121666, -1, P)) % P  # ed25519 curve constant d

SUITE_V1 = 0x01
WRAP_INFO_CONTEXT = b"iroh-rooms/content-key-wrap/v1"

# --- fixed golden inputs (non-secret test patterns, spec section 7 step 2) ---
ROOM_KEY = bytes(range(0x00, 0x20))          # 000102..1f
RECIPIENT_SEED = bytes(range(0x20, 0x40))    # 202122..3f
EPHEMERAL_SECRET = bytes(range(0x40, 0x60))  # 404142..5f
ROOM_ID = bytes(range(0x60, 0x80))           # 606162..7f
KEY_EPOCH = 7
WRAP_NONCE = bytes(range(0x80, 0x8C))        # 12 bytes 80..8b
CONTENT_NONCE = bytes(range(0x90, 0x9C))     # 12 bytes 90..9b
CONTENT_PLAINTEXT = b"iroh-rooms #191 SUITE_V1 content golden vector"
CONTENT_AAD = (
    b"iroh-rooms #191 golden AAD "
    b"(schema|room|sender|device|type|created|prev|inner|epoch|suite)"
)


def ed_pub_to_montgomery_u(ed_pub: bytes) -> bytes:
    """Edwards y -> Montgomery u = (1+y)/(1-y) mod p (RFC 7748 birational map)."""
    y = int.from_bytes(ed_pub, "little") & ((1 << 255) - 1)
    assert y < P, "non-canonical y"
    u = (1 + y) * pow(1 - y, -1, P) % P
    return u.to_bytes(32, "little")


def is_valid_edwards_point(compressed: bytes) -> bool:
    """True iff the compressed bytes decompress to a curve point (dalek rules)."""
    y = int.from_bytes(compressed, "little") & ((1 << 255) - 1)
    if y >= P:
        return False
    # x^2 = (y^2 - 1) / (d*y^2 + 1); valid iff that value is a square mod p.
    num = (y * y - 1) % P
    den = (D * y * y + 1) % P
    x2 = num * pow(den, -1, P) % P
    # p == 5 (mod 8): sqrt candidate x2^((p+3)/8), possibly times sqrt(-1).
    x = pow(x2, (P + 3) // 8, P)
    if (x * x) % P == x2:
        return True
    if (x * x) % P == (P - x2) % P:
        return True
    return x2 == 0


def find_invalid_point() -> bytes:
    """Smallest canonical y (sign bit 0) that fails Edwards decompression."""
    for y in range(2, 200):
        candidate = y.to_bytes(32, "little")
        if not is_valid_edwards_point(candidate):
            return candidate
    raise AssertionError("no invalid point found in range")


def hkdf_wrap_key(shared: bytes, recipient_device: bytes) -> bytes:
    info = WRAP_INFO_CONTEXT + KEY_EPOCH.to_bytes(8, "big") + recipient_device
    return HKDF(algorithm=SHA256(), length=32, salt=ROOM_ID, info=info).derive(shared)


def main() -> None:
    # Recipient device identity: Ed25519 from seed (OpenSSL implementation).
    recipient_ed = Ed25519PrivateKey.from_private_bytes(RECIPIENT_SEED)
    recipient_ed_pub = recipient_ed.public_key().public_bytes_raw()

    # Ed25519 -> X25519, both sides, independently of dalek:
    #   public: birational map on the Edwards y coordinate;
    #   secret: RFC 8032 expansion, lower half of SHA-512(seed).
    recipient_x_pub = ed_pub_to_montgomery_u(recipient_ed_pub)
    recipient_x_secret = hashlib.sha512(RECIPIENT_SEED).digest()[:32]

    # Ephemeral wrap channel (D3a): X25519 with a fixed "ephemeral" secret.
    eph = X25519PrivateKey.from_private_bytes(EPHEMERAL_SECRET)
    eph_pub = eph.public_key().public_bytes_raw()
    shared_sender = eph.exchange(X25519PublicKey.from_public_bytes(recipient_x_pub))
    shared_recipient = X25519PrivateKey.from_private_bytes(
        recipient_x_secret
    ).exchange(X25519PublicKey.from_public_bytes(eph_pub))
    assert shared_sender == shared_recipient, "DH directions disagree"
    assert shared_sender != bytes(32), "all-zero shared secret"

    # Wrap: HKDF-SHA-256 then AES-256-GCM (D3).
    wrap_key = hkdf_wrap_key(shared_sender, recipient_ed_pub)
    wrap_aad = ROOM_ID + KEY_EPOCH.to_bytes(8, "big") + recipient_ed_pub
    wrapped = AESGCM(wrap_key).encrypt(WRAP_NONCE, ROOM_KEY, wrap_aad)
    assert len(wrapped) == 48
    assert AESGCM(wrap_key).decrypt(WRAP_NONCE, wrapped, wrap_aad) == ROOM_KEY

    # Content AEAD: AES-256-GCM under the room key itself (D2/D3).
    content_ct = AESGCM(ROOM_KEY).encrypt(CONTENT_NONCE, CONTENT_PLAINTEXT, CONTENT_AAD)

    invalid_point = find_invalid_point()

    print(f'RECIPIENT_ED_PUB: "{recipient_ed_pub.hex()}"')
    print(f'RECIPIENT_X_PUB: "{recipient_x_pub.hex()}"')
    print(f'RECIPIENT_X_SECRET: "{recipient_x_secret.hex()}"')
    print(f'EPHEMERAL_PUB: "{eph_pub.hex()}"')
    print(f'SHARED: "{shared_sender.hex()}"')
    print(f'WRAP_KEY: "{wrap_key.hex()}"')
    print(f'WRAPPED_CT: "{wrapped.hex()}"')
    print(f'CONTENT_CT: "{content_ct.hex()}"')
    print(f'INVALID_POINT: "{invalid_point.hex()}"')


if __name__ == "__main__":
    main()
