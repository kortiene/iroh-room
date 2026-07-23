//! Strict per-kind content-body validation (spec #152 / source:
//! `content-and-moderation-event-schemas.md` §4 D3–D4, §5).
//!
//! Validation is strict and closed: unknown keys, missing required keys, wrong
//! types, over-cap values, bad enums, and disallowed empty strings all reject
//! with [`crate::Reject::InvalidContent`]. Cross-field rules against the envelope
//! author (the `sender_id`) are enforced statelessly (spec §5 sub-steps 5e–5g);
//! stateful checks (causal existence, role) are deferred to the authorization
//! layer and are out of scope here.

use crate::cbor::CborValue;
use crate::content::body::ContentEventBody;
use crate::content::registry::ContentKind;
use crate::error::Reject;
use crate::ids::LEN;

// --- Reused v1 caps (spec §4 D8 + source §2 constants) ----------------------
/// Max UTF-8 bytes of a `message.text` / `message.edited` body.
pub const MAX_MESSAGE_BODY_BYTES: usize = 16_384;
/// Max bytes of a `file.shared` name.
pub const MAX_FILE_NAME_BYTES: usize = 255;
/// Max bytes of a `file.shared` `mime_type`.
pub const MAX_MIME_TYPE_BYTES: usize = 255;
/// Max size of an importable file (100 MiB).
pub const MAX_SHARED_FILE_BYTES: u64 = 104_857_600;
/// Max asserted `providers` on a `file.shared`.
pub const MAX_FILE_PROVIDERS: usize = 16;
/// Max bytes of an `agent.status` label.
pub const MAX_STATUS_LABEL_BYTES: usize = 64;
/// Max bytes of an `agent.status` message.
pub const MAX_STATUS_MESSAGE_BYTES: usize = 4_096;
/// Max `related_artifact_ids` entries.
pub const MAX_ARTIFACT_REFS: usize = 16;
// --- New v2 caps (source §4 D8) ---------------------------------------------
/// Max `message.text.mentions` entries.
pub const MAX_MENTIONS: usize = 64;
/// Max bytes of a `message.reaction.emoji`.
pub const MAX_REACTION_EMOJI_BYTES: usize = 64;
/// Max bytes of a moderation `reason`.
pub const MAX_MOD_REASON_BYTES: usize = 1_024;
/// Max evidence refs in a moderation event.
pub const MAX_EVIDENCE_REFS: usize = 16;

/// Strictly validate a decoded content body (spec #152). Dispatches by kind.
///
/// # Errors
/// Returns [`crate::Reject::InvalidContent`] for any schema violation, and
/// [`crate::Reject::UnknownContentKind`] is impossible here (the envelope
/// already rejected unknown kinds).
pub fn validate_body(body: &ContentEventBody) -> Result<(), Reject> {
    let entries = body.body.as_map().ok_or(Reject::InvalidContent)?;
    let mut fields = Fields::new(entries);
    match body.kind {
        ContentKind::MessageText => validate_message_text(&mut fields),
        ContentKind::MessageReaction => validate_message_reaction(&mut fields),
        ContentKind::MessageEdited => validate_message_edited(&mut fields),
        ContentKind::FileShared => validate_file_shared(&mut fields),
        ContentKind::AgentStatus => validate_agent_status(&mut fields),
        ContentKind::ModerationBlock => validate_moderation_block(body, &mut fields),
        ContentKind::ModerationReport => validate_moderation_report(body, &mut fields),
        ContentKind::ModerationRemove => validate_moderation_remove(body, &mut fields),
    }?;
    fields.finish()
}

/// A strict field reader. Tracks which keys have been consumed so that
/// [`Self::finish`] rejects any leftover (unknown) key (the §6.4 closed-registry
/// discipline, applied per-kind).
struct Fields<'a> {
    entries: &'a [(String, CborValue)],
    seen: std::collections::HashSet<&'a str>,
}

impl<'a> Fields<'a> {
    fn new(entries: &'a [(String, CborValue)]) -> Self {
        Self {
            entries,
            seen: std::collections::HashSet::new(),
        }
    }

    fn get(&mut self, key: &str) -> Option<&'a CborValue> {
        let v = self.entries.iter().find(|(k, _)| k == key).map(|(_, v)| v);
        if v.is_some() {
            self.seen.insert(
                self.entries
                    .iter()
                    .find(|(k, _)| k == key)
                    .map_or("", |(k, _)| k.as_str()),
            );
        }
        v
    }

    fn require_text(&mut self, key: &str) -> Result<&'a str, Reject> {
        self.get(key)
            .and_then(|v| v.as_text())
            .ok_or(Reject::InvalidContent)
    }

    fn require_uint(&mut self, key: &str) -> Result<u64, Reject> {
        self.get(key)
            .and_then(super::super::cbor::CborValue::as_uint)
            .ok_or(Reject::InvalidContent)
    }

    fn require_bytes(&mut self, key: &str) -> Result<&'a [u8], Reject> {
        self.get(key)
            .and_then(|v| v.as_bytes())
            .ok_or(Reject::InvalidContent)
    }

    fn opt_text(&mut self, key: &str) -> Option<&'a str> {
        self.get(key).and_then(|v| v.as_text())
    }

    fn opt_uint(&mut self, key: &str) -> Option<u64> {
        self.get(key)
            .and_then(super::super::cbor::CborValue::as_uint)
    }

    fn opt_bytes(&mut self, key: &str) -> Option<&'a [u8]> {
        self.get(key).and_then(|v| v.as_bytes())
    }

    /// Reject if any key was not consumed (unknown key → reject, never ignore).
    fn finish(&self) -> Result<(), Reject> {
        for (k, _) in self.entries {
            if !self.seen.contains(k.as_str()) {
                return Err(Reject::InvalidContent);
            }
        }
        Ok(())
    }
}

fn require_fixed<const N: usize>(bytes: &[u8]) -> Result<&[u8; N], Reject> {
    bytes
        .try_into()
        .map_err(|_| Reject::InvalidContent)
        .map(|s: &[u8; N]| s)
}

fn require_text_cap(s: &str, cap: usize) -> Result<(), Reject> {
    if s.is_empty() || s.len() > cap {
        return Err(Reject::InvalidContent);
    }
    if s.chars().any(char::is_control) {
        return Err(Reject::InvalidContent);
    }
    Ok(())
}

fn require_enum(s: &str, allowed: &[&str]) -> Result<(), Reject> {
    if allowed.contains(&s) {
        Ok(())
    } else {
        Err(Reject::InvalidContent)
    }
}

fn require_bstr_array_cap(arr: &[CborValue], cap: usize) -> Result<(), Reject> {
    if arr.is_empty() || arr.len() > cap {
        return Err(Reject::InvalidContent);
    }
    for item in arr {
        if item.as_bytes().is_none() {
            return Err(Reject::InvalidContent);
        }
    }
    Ok(())
}

// --- Per-kind validators ----------------------------------------------------

fn validate_message_text(f: &mut Fields<'_>) -> Result<(), Reject> {
    let body = f.require_text("body")?;
    if body.len() > MAX_MESSAGE_BODY_BYTES {
        return Err(Reject::InvalidContent);
    }
    if let Some(fmt) = f.opt_text("format") {
        require_enum(fmt, &["plain", "markdown"])?;
    }
    if let Some(reply) = f.opt_bytes("in_reply_to") {
        let _ = require_fixed::<LEN>(reply)?;
    }
    if let Some(mentions) = f.get("mentions").and_then(|v| v.as_array()) {
        require_bstr_array_cap(mentions, MAX_MENTIONS)?;
        for m in mentions {
            let _ = require_fixed::<LEN>(m.as_bytes().ok_or(Reject::InvalidContent)?)?;
        }
    }
    if let Some(thread) = f.opt_bytes("thread_id") {
        let _ = require_fixed::<16>(thread)?;
    }
    Ok(())
}

fn validate_message_reaction(f: &mut Fields<'_>) -> Result<(), Reject> {
    let _ = require_fixed::<LEN>(f.require_bytes("target")?)?;
    let emoji = f.require_text("emoji")?;
    require_text_cap(emoji, MAX_REACTION_EMOJI_BYTES)?;
    if let Some(op) = f.opt_text("op") {
        require_enum(op, &["add", "remove"])?;
    }
    Ok(())
}

fn validate_message_edited(f: &mut Fields<'_>) -> Result<(), Reject> {
    let _ = require_fixed::<LEN>(f.require_bytes("target")?)?;
    let new_body = f.require_text("new_body")?;
    if new_body.len() > MAX_MESSAGE_BODY_BYTES {
        return Err(Reject::InvalidContent);
    }
    if let Some(fmt) = f.opt_text("format") {
        require_enum(fmt, &["plain", "markdown"])?;
    }
    Ok(())
}

fn validate_file_shared(f: &mut Fields<'_>) -> Result<(), Reject> {
    let _ = require_fixed::<16>(f.require_bytes("file_id")?)?;
    let name = f.require_text("name")?;
    require_text_cap(name, MAX_FILE_NAME_BYTES)?;
    let mime = f.require_text("mime_type")?;
    if mime.len() > MAX_MIME_TYPE_BYTES || !is_well_formed_mime(mime) {
        return Err(Reject::InvalidContent);
    }
    let size = f.require_uint("size_bytes")?;
    if size > MAX_SHARED_FILE_BYTES {
        return Err(Reject::InvalidContent);
    }
    let _ = require_fixed::<LEN>(f.require_bytes("blob_hash")?)?;
    if let Some(bf) = f.opt_text("blob_format") {
        require_enum(bf, &["raw", "hash_seq"])?;
    }
    if let Some(providers) = f.get("providers").and_then(|v| v.as_array()) {
        require_bstr_array_cap(providers, MAX_FILE_PROVIDERS)?;
        for p in providers {
            let _ = require_fixed::<LEN>(p.as_bytes().ok_or(Reject::InvalidContent)?)?;
        }
    }
    Ok(())
}

fn validate_agent_status(f: &mut Fields<'_>) -> Result<(), Reject> {
    let status = f.require_text("status")?;
    require_text_cap(status, MAX_STATUS_LABEL_BYTES)?;
    if let Some(msg) = f.opt_text("message") {
        if msg.len() > MAX_STATUS_MESSAGE_BYTES {
            return Err(Reject::InvalidContent);
        }
    }
    if let Some(arts) = f.get("related_artifact_ids").and_then(|v| v.as_array()) {
        require_bstr_array_cap(arts, MAX_ARTIFACT_REFS)?;
        for a in arts {
            let _ = require_fixed::<16>(a.as_bytes().ok_or(Reject::InvalidContent)?)?;
        }
    }
    if let Some(pct) = f.opt_uint("progress_pct") {
        if pct > 100 {
            return Err(Reject::InvalidContent);
        }
    }
    Ok(())
}

fn validate_moderation_block(body: &ContentEventBody, f: &mut Fields<'_>) -> Result<(), Reject> {
    validate_stream_scope(body, f)?;
    let subject = f.require_bytes("subject")?;
    let _ = require_fixed::<LEN>(subject)?;
    let blocked_by = f.require_bytes("blocked_by")?;
    let _ = require_fixed::<LEN>(blocked_by)?;
    // Cross-field: blocked_by == author; subject != author (spec §5 5e).
    if blocked_by != body.author.as_bytes().as_slice()
        || subject == body.author.as_bytes().as_slice()
    {
        return Err(Reject::InvalidContent);
    }
    let scope = f.require_text("scope")?;
    require_enum(scope, &["stream", "room"])?;
    check_scope_stream_consistency(body, scope)?;
    validate_evidence_triple(f)?;
    if let Some(_exp) = f.opt_uint("expires_at") {
        // Stateless: presence + uint only; expiry semantics are deferred.
    }
    Ok(())
}

fn validate_moderation_report(body: &ContentEventBody, f: &mut Fields<'_>) -> Result<(), Reject> {
    validate_stream_scope(body, f)?;
    let subject = f.require_bytes("subject")?;
    let _ = require_fixed::<LEN>(subject)?;
    if let Some(target) = f.opt_bytes("target_event") {
        let _ = require_fixed::<LEN>(target)?;
    }
    let category = f.require_text("category")?;
    require_enum(
        category,
        &["spam", "abuse", "harassment", "malware", "other"],
    )?;
    let reported_by = f.require_bytes("reported_by")?;
    let _ = require_fixed::<LEN>(reported_by)?;
    if reported_by != body.author.as_bytes().as_slice() {
        return Err(Reject::InvalidContent);
    }
    validate_evidence_triple(f)?;
    Ok(())
}

fn validate_moderation_remove(body: &ContentEventBody, f: &mut Fields<'_>) -> Result<(), Reject> {
    validate_stream_scope(body, f)?;
    let target = f.require_bytes("target_event")?;
    let _ = require_fixed::<LEN>(target)?;
    let removed_by = f.require_bytes("removed_by")?;
    let _ = require_fixed::<LEN>(removed_by)?;
    if removed_by != body.author.as_bytes().as_slice() {
        return Err(Reject::InvalidContent);
    }
    validate_evidence_triple(f)?;
    Ok(())
}

/// Validate a moderation `stream_id` field (stateless: present ⇒ `bstr[16]`;
/// the scope/room bidirectional check happens in the kind validator).
fn validate_stream_scope(body: &ContentEventBody, f: &mut Fields<'_>) -> Result<(), Reject> {
    if let Some(sid) = f.opt_bytes("stream_id") {
        let _ = require_fixed::<16>(sid)?;
        // If both the envelope and body carry a stream_id, they must be identical
        // (spec §5 sub-step 5g).
        if let Some(env_sid) = body.stream_id {
            if sid != env_sid.as_slice() {
                return Err(Reject::InvalidContent);
            }
        }
    }
    Ok(())
}

/// `scope == room` ⇒ `stream_id` absent; `scope == stream` ⇒ `stream_id` present
/// (spec §5 sub-step 5f, both directions).
fn check_scope_stream_consistency(body: &ContentEventBody, scope: &str) -> Result<(), Reject> {
    match scope {
        "room" => {
            if f_has(body, "stream_id") {
                return Err(Reject::InvalidContent);
            }
        }
        "stream" => {
            if !f_has(body, "stream_id") {
                return Err(Reject::InvalidContent);
            }
        }
        _ => return Err(Reject::InvalidContent),
    }
    Ok(())
}

fn f_has(body: &ContentEventBody, key: &str) -> bool {
    body.body.get(key).is_some()
}

/// Validate the shared audit-evidence triple: `reason` (≤ cap),
/// `evidence_events` (`bstr[32]` ≤ cap), `evidence_blobs` (`bstr[32]` ≤ cap).
fn validate_evidence_triple(f: &mut Fields<'_>) -> Result<(), Reject> {
    if let Some(reason) = f.opt_text("reason") {
        if reason.len() > MAX_MOD_REASON_BYTES {
            return Err(Reject::InvalidContent);
        }
    }
    for key in ["evidence_events", "evidence_blobs"] {
        if let Some(arr) = f.get(key).and_then(|v| v.as_array()) {
            require_bstr_array_cap(arr, MAX_EVIDENCE_REFS)?;
            for item in arr {
                let _ = require_fixed::<LEN>(item.as_bytes().ok_or(Reject::InvalidContent)?)?;
            }
        }
    }
    Ok(())
}

/// A minimal well-formedness check for `type/subtype` media types (mirrors v1).
fn is_well_formed_mime(mime: &str) -> bool {
    let Some((ty, sub)) = mime.split_once('/') else {
        return false;
    };
    !ty.is_empty()
        && !sub.is_empty()
        && ty
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == '_')
        && sub
            .split(';')
            .next()
            .unwrap_or(sub)
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == '_' || c == '+')
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::content::body::ContentEventBody;
    use crate::ids::RoomId;
    use crate::keys::SigningKey;
    use crate::MemberId;

    fn author() -> (SigningKey, MemberId, RoomId) {
        let k = SigningKey::from_seed(&[0x55; LEN]);
        (
            k.clone_shallow(),
            k.member_id(),
            RoomId::from_bytes([0x50; LEN]),
        )
    }

    // A tiny helper to build a content body with a kind + raw body map.
    fn body(
        kind: ContentKind,
        author: MemberId,
        room: RoomId,
        entries: Vec<(String, CborValue)>,
    ) -> ContentEventBody {
        ContentEventBody {
            schema_version: 2,
            room_id: room,
            author,
            kind,
            version: 1,
            stream_id: None,
            body: CborValue::Map(entries.into_iter().collect()),
        }
    }

    // `SigningKey` isn't Clone; expose a builder from the seed.
    impl SigningKey {
        fn clone_shallow(&self) -> Self {
            SigningKey::from_seed(&self.to_seed())
        }
    }

    #[test]
    fn message_text_valid() {
        let (_, a, r) = author();
        let b = body(
            ContentKind::MessageText,
            a,
            r,
            vec![("body".into(), CborValue::Text("hi".into()))],
        );
        assert!(validate_body(&b).is_ok());
    }

    #[test]
    fn message_text_over_cap_rejected() {
        let (_, a, r) = author();
        let big = "x".repeat(MAX_MESSAGE_BODY_BYTES + 1);
        let b = body(
            ContentKind::MessageText,
            a,
            r,
            vec![("body".into(), CborValue::Text(big))],
        );
        assert_eq!(validate_body(&b).err(), Some(Reject::InvalidContent));
    }

    #[test]
    fn message_text_unknown_key_rejected() {
        let (_, a, r) = author();
        let b = body(
            ContentKind::MessageText,
            a,
            r,
            vec![
                ("body".into(), CborValue::Text("hi".into())),
                ("bogus".into(), CborValue::Uint(1)),
            ],
        );
        assert_eq!(validate_body(&b).err(), Some(Reject::InvalidContent));
    }

    #[test]
    fn reaction_valid() {
        let (_, a, r) = author();
        let b = body(
            ContentKind::MessageReaction,
            a,
            r,
            vec![
                ("target".into(), CborValue::Bytes(vec![0xab; LEN])),
                ("emoji".into(), CborValue::Text("+1".into())),
            ],
        );
        assert!(validate_body(&b).is_ok());
    }

    #[test]
    fn reaction_missing_required_rejected() {
        let (_, a, r) = author();
        let b = body(
            ContentKind::MessageReaction,
            a,
            r,
            vec![("emoji".into(), CborValue::Text("+1".into()))],
        );
        assert_eq!(validate_body(&b).err(), Some(Reject::InvalidContent));
    }

    #[test]
    fn moderation_block_cross_field_mismatch_rejected() {
        let (_, a, r) = author();
        // blocked_by != author.
        let b = body(
            ContentKind::ModerationBlock,
            a,
            r,
            vec![
                ("subject".into(), CborValue::Bytes(vec![0xee; LEN])),
                ("blocked_by".into(), CborValue::Bytes(vec![0xff; LEN])),
                ("scope".into(), CborValue::Text("room".into())),
            ],
        );
        assert_eq!(validate_body(&b).err(), Some(Reject::InvalidContent));
    }

    #[test]
    fn moderation_block_scope_room_with_stream_id_rejected() {
        let (_, a, r) = author();
        let mut b = body(
            ContentKind::ModerationBlock,
            a,
            r,
            vec![
                ("subject".into(), CborValue::Bytes(vec![0xee; LEN])),
                ("blocked_by".into(), CborValue::Bytes(a.as_bytes().to_vec())),
                ("scope".into(), CborValue::Text("room".into())),
                ("stream_id".into(), CborValue::Bytes(vec![0u8; 16])),
            ],
        );
        b.stream_id = Some([0u8; 16]);
        assert_eq!(validate_body(&b).err(), Some(Reject::InvalidContent));
    }

    #[test]
    fn file_shared_invalid_mime_rejected() {
        let (_, a, r) = author();
        let b = body(
            ContentKind::FileShared,
            a,
            r,
            vec![
                ("file_id".into(), CborValue::Bytes(vec![0u8; 16])),
                ("name".into(), CborValue::Text("f.bin".into())),
                ("mime_type".into(), CborValue::Text("notmime".into())),
                ("size_bytes".into(), CborValue::Uint(10)),
                ("blob_hash".into(), CborValue::Bytes(vec![0u8; LEN])),
            ],
        );
        assert_eq!(validate_body(&b).err(), Some(Reject::InvalidContent));
    }

    #[test]
    fn evidence_over_cap_rejected() {
        let (_, a, r) = author();
        let too_many: Vec<CborValue> = (0..=MAX_EVIDENCE_REFS)
            .map(|_| CborValue::Bytes(vec![0u8; LEN]))
            .collect();
        let b = body(
            ContentKind::ModerationReport,
            a,
            r,
            vec![
                ("subject".into(), CborValue::Bytes(vec![0xee; LEN])),
                ("category".into(), CborValue::Text("spam".into())),
                (
                    "reported_by".into(),
                    CborValue::Bytes(a.as_bytes().to_vec()),
                ),
                ("evidence_events".into(), CborValue::Array(too_many)),
            ],
        );
        assert_eq!(validate_body(&b).err(), Some(Reject::InvalidContent));
    }
}
