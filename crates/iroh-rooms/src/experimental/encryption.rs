//! **Experimental (unstable API).** Encrypted-content suite parameters and
//! plaintext caps.
//!
//! These constants bound `content.encrypted` envelopes and wrapped room keys
//! (spec `content-key-rotation.md`). The parser in `iroh-rooms-core` always
//! compiles them; this re-export is behind the `experimental` feature because
//! v0.1 ships room content as plaintext. The list is explicit — there is no
//! glob.

pub use iroh_rooms_core::event::encryption::{
    ENCRYPTED_AAD_CONTEXT, ENCRYPTED_NONCE_LEN, ENCRYPTED_SUITE_V1, ENCRYPTED_TAG_LEN,
    MAX_ENCRYPTED_AGENT_STATUS_PLAINTEXT, MAX_ENCRYPTED_FILE_SHARED_PLAINTEXT,
    MAX_ENCRYPTED_MESSAGE_TEXT_PLAINTEXT, MAX_ENCRYPTED_PIPE_CLOSED_PLAINTEXT,
    MAX_ENCRYPTED_PIPE_OPENED_PLAINTEXT, WRAPPED_KEY_CIPHERTEXT_LEN, WRAPPED_KEY_NONCE_LEN,
};
