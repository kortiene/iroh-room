//! Strict per-kind content-body validation (spec #152 / source:
//! `content-and-moderation-event-schemas.md` §4 D3–D4, §5).
//!
//! Validation is strict and closed: unknown keys, missing required keys, wrong
//! types, over-cap values, bad enums, and disallowed empty strings all reject
//! with [`crate::Reject::InvalidContent`]. A present optional field of the wrong
//! type is rejected, never treated as absent (spec #152 §2.2 gap #3 / §8 step 3).
//! Cross-field rules against the author identity are enforced statelessly (spec
//! §5 sub-steps 5e–5g); stateful checks (causal existence, role) are deferred to
//! the authorization layer and are out of scope here.
//!
//! Two entry points share one per-kind core:
//! - [`validate_body`] validates the provisional envelope's inner body (kept for
//!   the frozen #153 golden vectors).
//! - [`validate_content`] validates the normative #134 §9.2 body's `content` map
//!   against its `kind` and `author_id`.

use crate::cbor::CborValue;
use crate::content::provisional::ProvisionalContentEventBody;
use crate::content::registry::ContentKind;
use crate::error::Reject;
use crate::ids::{MemberId, LEN};

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

/// Strictly validate the provisional envelope's inner body (spec #152
/// provisional path; kept for the frozen #153 golden vectors). Dispatches by
/// kind against `body.body` using `body.author` for stateless cross-field rules.
///
/// # Errors
/// Returns [`crate::Reject::InvalidContent`] for any schema violation.
pub fn validate_body(body: &ProvisionalContentEventBody) -> Result<(), Reject> {
    validate_kind(body.kind, &body.body, &body.author)
}

/// Strictly validate the normative #134 §9.2 body's `content` map against its
/// `kind` and `author_id` (spec #152). Used by the normative content-event body
/// decode (see [`super::body::ContentEventBody::from_canonical`]).
///
/// # Errors
/// Returns [`crate::Reject::InvalidContent`] for any schema violation.
pub fn validate_content(
    kind: ContentKind,
    content: &CborValue,
    author_id: &MemberId,
) -> Result<(), Reject> {
    validate_kind(kind, content, author_id)
}

/// The shared per-kind dispatch: validate the kind-specific content map entries,
/// enforcing exact known-key closure, required fields, types, caps, and enums.
fn validate_kind(
    kind: ContentKind,
    content: &CborValue,
    author_id: &MemberId,
) -> Result<(), Reject> {
    let entries = content.as_map().ok_or(Reject::InvalidContent)?;
    let mut fields = Fields::new(entries);
    match kind {
        ContentKind::MessageText => validate_message_text(&mut fields),
        ContentKind::MessageReaction => validate_message_reaction(&mut fields),
        ContentKind::MessageEdited => validate_message_edited(&mut fields),
        ContentKind::FileShared => validate_file_shared(&mut fields),
        ContentKind::AgentStatus => validate_agent_status(&mut fields),
        ContentKind::ModerationBlock => validate_moderation_block(author_id, &mut fields),
        ContentKind::ModerationReport => validate_moderation_report(author_id, &mut fields),
        ContentKind::ModerationRemove => validate_moderation_remove(author_id, &mut fields),
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
            // `self.entries` is `&'a [...]` (Copy), so iterating it borrows the
            // slice data with lifetime `'a`, not `self`; the shared ref can
            // therefore be stored in `seen` (which is also keyed on `'a`).
            let key_str = self
                .entries
                .iter()
                .find(|(k, _)| k == key)
                .map_or("", |(k, _)| k.as_str());
            self.seen.insert(key_str);
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
            .and_then(CborValue::as_uint)
            .ok_or(Reject::InvalidContent)
    }

    fn require_bytes(&mut self, key: &str) -> Result<&'a [u8], Reject> {
        self.get(key)
            .and_then(|v| v.as_bytes())
            .ok_or(Reject::InvalidContent)
    }

    /// Read an optional text field. A present value of the wrong type is
    /// rejected (spec #152 §2.2 gap #3); an absent key yields `None`.
    fn opt_text(&mut self, key: &str) -> Result<Option<&'a str>, Reject> {
        match self.get(key) {
            Some(v) => v.as_text().map(Some).ok_or(Reject::InvalidContent),
            None => Ok(None),
        }
    }

    /// Read an optional uint field; a present wrong-typed value rejects.
    fn opt_uint(&mut self, key: &str) -> Result<Option<u64>, Reject> {
        match self.get(key) {
            Some(v) => v.as_uint().map(Some).ok_or(Reject::InvalidContent),
            None => Ok(None),
        }
    }

    /// Read an optional byte-string field; a present wrong-typed value rejects.
    fn opt_bytes(&mut self, key: &str) -> Result<Option<&'a [u8]>, Reject> {
        match self.get(key) {
            Some(v) => v.as_bytes().map(Some).ok_or(Reject::InvalidContent),
            None => Ok(None),
        }
    }

    /// Read an optional array field; a present wrong-typed value rejects.
    fn opt_array(&mut self, key: &str) -> Result<Option<&'a [CborValue]>, Reject> {
        match self.get(key) {
            Some(v) => v.as_array().map(Some).ok_or(Reject::InvalidContent),
            None => Ok(None),
        }
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
    if let Some(fmt) = f.opt_text("format")? {
        require_enum(fmt, &["plain", "markdown"])?;
    }
    if let Some(reply) = f.opt_bytes("in_reply_to")? {
        let _ = require_fixed::<LEN>(reply)?;
    }
    if let Some(mentions) = f.opt_array("mentions")? {
        require_bstr_array_cap(mentions, MAX_MENTIONS)?;
        for m in mentions {
            let _ = require_fixed::<LEN>(m.as_bytes().ok_or(Reject::InvalidContent)?)?;
        }
    }
    if let Some(thread) = f.opt_bytes("thread_id")? {
        let _ = require_fixed::<16>(thread)?;
    }
    Ok(())
}

fn validate_message_reaction(f: &mut Fields<'_>) -> Result<(), Reject> {
    let _ = require_fixed::<LEN>(f.require_bytes("target")?)?;
    let emoji = f.require_text("emoji")?;
    require_text_cap(emoji, MAX_REACTION_EMOJI_BYTES)?;
    if let Some(op) = f.opt_text("op")? {
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
    if let Some(fmt) = f.opt_text("format")? {
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
    if let Some(bf) = f.opt_text("blob_format")? {
        require_enum(bf, &["raw", "hash_seq"])?;
    }
    if let Some(providers) = f.opt_array("providers")? {
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
    if let Some(msg) = f.opt_text("message")? {
        if msg.len() > MAX_STATUS_MESSAGE_BYTES {
            return Err(Reject::InvalidContent);
        }
    }
    if let Some(arts) = f.opt_array("related_artifact_ids")? {
        require_bstr_array_cap(arts, MAX_ARTIFACT_REFS)?;
        for a in arts {
            let _ = require_fixed::<16>(a.as_bytes().ok_or(Reject::InvalidContent)?)?;
        }
    }
    if let Some(pct) = f.opt_uint("progress_pct")? {
        if pct > 100 {
            return Err(Reject::InvalidContent);
        }
    }
    Ok(())
}

fn validate_moderation_block(author_id: &MemberId, f: &mut Fields<'_>) -> Result<(), Reject> {
    validate_stream_scope(f)?;
    let subject = f.require_bytes("subject")?;
    let _ = require_fixed::<LEN>(subject)?;
    let blocked_by = f.require_bytes("blocked_by")?;
    let _ = require_fixed::<LEN>(blocked_by)?;
    // Cross-field: blocked_by == author; subject != author (spec §5 5e).
    if blocked_by != author_id.as_bytes().as_slice() || subject == author_id.as_bytes().as_slice() {
        return Err(Reject::InvalidContent);
    }
    let scope = f.require_text("scope")?;
    require_enum(scope, &["stream", "room"])?;
    check_scope_stream_consistency(f, scope)?;
    validate_evidence_triple(f)?;
    if let Some(_exp) = f.opt_uint("expires_at")? {
        // Stateless: presence + uint only; expiry semantics are deferred.
    }
    Ok(())
}

fn validate_moderation_report(author_id: &MemberId, f: &mut Fields<'_>) -> Result<(), Reject> {
    validate_stream_scope(f)?;
    let subject = f.require_bytes("subject")?;
    let _ = require_fixed::<LEN>(subject)?;
    if let Some(target) = f.opt_bytes("target_event")? {
        let _ = require_fixed::<LEN>(target)?;
    }
    let category = f.require_text("category")?;
    require_enum(
        category,
        &["spam", "abuse", "harassment", "malware", "other"],
    )?;
    let reported_by = f.require_bytes("reported_by")?;
    let _ = require_fixed::<LEN>(reported_by)?;
    if reported_by != author_id.as_bytes().as_slice() {
        return Err(Reject::InvalidContent);
    }
    validate_evidence_triple(f)?;
    Ok(())
}

fn validate_moderation_remove(author_id: &MemberId, f: &mut Fields<'_>) -> Result<(), Reject> {
    validate_stream_scope(f)?;
    let target = f.require_bytes("target_event")?;
    let _ = require_fixed::<LEN>(target)?;
    let removed_by = f.require_bytes("removed_by")?;
    let _ = require_fixed::<LEN>(removed_by)?;
    if removed_by != author_id.as_bytes().as_slice() {
        return Err(Reject::InvalidContent);
    }
    validate_evidence_triple(f)?;
    Ok(())
}

/// Validate a moderation `stream_id` field within the content map (stateless:
/// present ⇒ `bstr[16]`). The normative §9.2 body carries a separate required
/// top-level 32-byte `stream_id`; this per-kind field is the moderation
/// stream-scope selector and is validated only for width here.
fn validate_stream_scope(f: &mut Fields<'_>) -> Result<(), Reject> {
    if let Some(sid) = f.opt_bytes("stream_id")? {
        let _ = require_fixed::<16>(sid)?;
    }
    Ok(())
}

/// `scope == room` ⇒ content-map `stream_id` absent; `scope == stream` ⇒ present
/// (spec §5 sub-step 5f, both directions), against the per-kind content map.
fn check_scope_stream_consistency(f: &mut Fields<'_>, scope: &str) -> Result<(), Reject> {
    let has_stream_id = f.entries.iter().any(|(k, _)| k == "stream_id");
    match scope {
        "room" => {
            if has_stream_id {
                return Err(Reject::InvalidContent);
            }
        }
        "stream" => {
            if !has_stream_id {
                return Err(Reject::InvalidContent);
            }
        }
        _ => return Err(Reject::InvalidContent),
    }
    Ok(())
}

/// Validate the shared audit-evidence triple: `reason` (≤ cap),
/// `evidence_events` (`bstr[32]` ≤ cap), `evidence_blobs` (`bstr[32]` ≤ cap).
fn validate_evidence_triple(f: &mut Fields<'_>) -> Result<(), Reject> {
    if let Some(reason) = f.opt_text("reason")? {
        if reason.len() > MAX_MOD_REASON_BYTES {
            return Err(Reject::InvalidContent);
        }
    }
    for key in ["evidence_events", "evidence_blobs"] {
        if let Some(arr) = f.opt_array(key)? {
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
    use crate::content::provisional::ProvisionalContentEventBody;
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

    // A tiny helper to build a provisional content body with a kind + raw body map.
    fn body(
        kind: ContentKind,
        author: MemberId,
        room: RoomId,
        entries: Vec<(String, CborValue)>,
    ) -> ProvisionalContentEventBody {
        ProvisionalContentEventBody {
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
        let b = body(
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

    #[test]
    fn present_wrong_typed_optional_rejected() {
        // A present `format` with a non-text value must reject (spec §2.2 gap #3),
        // not be silently treated as absent.
        let (_, a, r) = author();
        let b = body(
            ContentKind::MessageText,
            a,
            r,
            vec![
                ("body".into(), CborValue::Text("hi".into())),
                ("format".into(), CborValue::Uint(7)),
            ],
        );
        assert_eq!(validate_body(&b).err(), Some(Reject::InvalidContent));
    }
}
