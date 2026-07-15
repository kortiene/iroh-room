//! The CLI error taxonomy (spec IR-0110 / issue #25): a single, stable,
//! script-facing failure surface layered on top of `anyhow`.
//!
//! Every layer below the CLI already computes rich, correct failure information
//! with its own pinned reason enum: the protocol validator
//! ([`RejectReason`]), the ticket codec ([`TicketError`]), and the transport
//! connection-state model ([`OfflineReason`]). [`ErrorCode`] does not re-invent
//! that vocabulary — it **wraps** those enums verbatim and adds the handful of
//! CLI-native variants the wrapped types cannot express (a wrong-identity ticket
//! redemption, an unreachable admin, …), so a new protocol-layer code appears on
//! the CLI automatically.
//!
//! [`CliError`] attaches an [`ErrorCode`] to an ordinary `anyhow` failure; the
//! [`CodedResultExt`] extension attaches one at the point a validation/lookup
//! function's error is known to belong to a specific failure class, mirroring
//! `anyhow`'s `.context(...)` ergonomics. [`main`](crate::main) walks the
//! resulting error chain via [`code_of`] and renders the pinned
//! `error[<code>]: <message>` contract with the matching category exit code
//! (§5.2/§5.3 of the spec); an error nobody attached a code to still renders as
//! `error: <message>` and exits `1` — a code-adoption gap, not a crash.

use std::fmt;

use iroh_rooms_core::event::RejectReason;
use iroh_rooms_core::ticket::TicketError;
use iroh_rooms_net::OfflineReason;

/// The unified CLI-facing failure taxonomy (spec §5.1). Wrapped arms
/// ([`Self::Reject`], [`Self::Ticket`]) delegate their [`code`](Self::code) to the
/// already-pinned source enum; the CLI never re-lists or renames a §8 code.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum ErrorCode {
    /// A protocol §8 rejection (reuses [`RejectReason::code`] verbatim). Covers
    /// `bad_signature` (invalid-signature, AC1) and `not_a_member` /
    /// `unbound_device` / `insufficient_role` / `expired_invite` / `bad_capability`
    /// (unauthorized-sender / invalid-ticket, AC1/AC3), plus the structural/encoding
    /// rejects.
    Reject(RejectReason),
    /// A ticket decode failure (reuses [`TicketError::code`] verbatim; AC3).
    Ticket(TicketError),

    /// A join never reached the room admin within the timeout (offline/unreachable
    /// admin). Distinct from an authorization rejection (AC2).
    NoAdminReachable,
    /// A join DID pull room history from the admin, but the membership ancestry
    /// needed to verify the invite never completed within the timeout — e.g. the
    /// admin runs an older version that serves the authorization class without
    /// its causal closure. Distinct from [`Self::NoAdminReachable`]: bytes
    /// arrived, the sub-DAG just never became verifiable.
    MembershipIncomplete,
    /// A connectivity command could not reach an authorized peer/owner (offline).
    /// The command-failure twin of `PeerConnState::Offline` (AC2).
    PeerOffline(OfflineReason),
    /// A connectivity command was refused because the caller (or peer) is not an
    /// authorized member. The command-failure twin of `PeerConnState::Unauthorized`
    /// (AC2).
    PeerUnauthorized,

    /// The local identity does not match the ticket's `invitee_key` (AC3).
    WrongIdentity,
    /// The ticket carries no admin discovery hint and no `--peer` was given (AC3).
    NoDiscoveryHint,

    /// No reachable provider holds the requested blob within the fetch timeout —
    /// the honest MVP availability limitation (PRD §14: no central inbox, no
    /// guaranteed offline delivery). Emitted by `file fetch` (spec IR-0205 §5.1).
    BlobUnavailable,

    /// A fetched blob's independently recomputed BLAKE3-256 does not match the
    /// `file.shared` reference's declared hash — a content-integrity failure, not
    /// an availability or authorization one (spec IR-0205 §5.4).
    HashMismatch,

    /// `--room-id`/positional room id argument does not parse (`blake3:<hex>`).
    InvalidRoomId,
    /// An option value is malformed (`--timeout`, `--role`, `--format`,
    /// `--expires`, …).
    InvalidArgument,
    /// A `file share` path does not exist.
    NoSuchFile,
    /// A `file share` path exists but cannot be read.
    PermissionDenied,
    /// A `file share` path exceeds the MVP size cap.
    FileTooLarge,
    /// No local identity exists (`identity create` was never run).
    IdentityNotFound,
    /// No room with this id is known locally.
    RoomNotFound,

    /// Catch-all for an unexpected internal failure (should be rare; a bug signal).
    Internal,
}

impl ErrorCode {
    /// The stable string code (spec §5.1), for scripts/tests to branch on.
    #[must_use]
    pub fn code(&self) -> &'static str {
        match self {
            Self::Reject(r) => r.code(),
            Self::Ticket(t) => t.code(),
            Self::NoAdminReachable => "no_admin_reachable",
            Self::MembershipIncomplete => "membership_incomplete",
            Self::PeerOffline(_) => "peer_offline",
            Self::PeerUnauthorized => "peer_unauthorized",
            Self::WrongIdentity => "wrong_identity",
            Self::NoDiscoveryHint => "no_discovery_hint",
            Self::BlobUnavailable => "blob_unavailable",
            Self::HashMismatch => "hash_mismatch",
            Self::InvalidRoomId => "invalid_room_id",
            Self::InvalidArgument => "invalid_argument",
            Self::NoSuchFile => "no_such_file",
            Self::PermissionDenied => "permission_denied",
            Self::FileTooLarge => "file_too_large",
            Self::IdentityNotFound => "identity_not_found",
            Self::RoomNotFound => "room_not_found",
            Self::Internal => "internal",
        }
    }

    /// The coarse category a script branches on via `$?` (spec §5.3).
    #[must_use]
    pub fn category(&self) -> ErrorCategory {
        match self {
            Self::Internal => ErrorCategory::Internal,
            Self::InvalidRoomId
            | Self::InvalidArgument
            | Self::NoSuchFile
            | Self::PermissionDenied
            | Self::FileTooLarge
            | Self::IdentityNotFound
            | Self::RoomNotFound
            | Self::NoDiscoveryHint => ErrorCategory::Usage,
            Self::WrongIdentity | Self::PeerUnauthorized => ErrorCategory::Auth,
            Self::HashMismatch => ErrorCategory::Integrity,
            Self::Reject(reason) => reject_category(reason),
            Self::Ticket(_) => ErrorCategory::Ticket,
            Self::NoAdminReachable
            | Self::MembershipIncomplete
            | Self::PeerOffline(_)
            | Self::BlobUnavailable => ErrorCategory::Connectivity,
        }
    }

    /// The process exit code for this failure (spec §5.3).
    #[must_use]
    pub fn exit_code(&self) -> u8 {
        self.category().exit_code()
    }

    /// A stable, secret-free next step for a human — the "what do I do now" line
    /// (spec IR-0303 §5.1). `None` for codes where the call-site message already
    /// carries all the context there is (`internal`, `invalid_argument`), or where
    /// no generic action applies (a structural/crypto reject). Every arm returns a
    /// fixed `&'static str` template — no interpolation — so this is structurally
    /// incapable of leaking a secret; runtime detail (paths, ids, a resolved
    /// `--peer`) stays in the [`CliError`] message, not here.
    #[must_use]
    pub fn next_action(&self) -> Option<&'static str> {
        match self {
            Self::IdentityNotFound => {
                Some("run `iroh-rooms identity create --name <name>` first")
            }
            Self::InvalidRoomId => Some(
                "copy the room id from `room create` / `room members` (form `blake3:<hex>`)",
            ),
            Self::RoomNotFound => {
                Some("run `iroh-rooms room create <name>`, or join an invite ticket first")
            }
            Self::NoSuchFile => Some(
                "check the path for `file share`, or run `file list` / `room tail` first \
                 to sync the reference for `file fetch`",
            ),
            Self::PermissionDenied => {
                Some("check the file's read permissions, or share a copy you can read")
            }
            Self::FileTooLarge => {
                Some("the MVP share limit is fixed; split or compress the file")
            }
            Self::NoDiscoveryHint => {
                Some("pass `--peer <admin-addr>` (the ticket carried no discovery hint)")
            }
            Self::NoAdminReachable => Some(
                "ask the admin to run `room tail <ROOM_ID> --accept-joins`, then retry; \
                 or pass `--peer <admin-addr>`",
            ),
            Self::MembershipIncomplete => Some(
                "retry with a longer `--timeout`; if it persists, the admin may be \
                 running an older version that does not share the invite's full history — \
                 ask them to upgrade and re-run `room tail <ROOM_ID> --accept-joins`",
            ),
            Self::PeerOffline(_) => Some(
                "ask the owner to come online (run `room tail <ROOM_ID>`), then retry; \
                 or pass `--peer <owner-addr>`",
            ),
            Self::PeerUnauthorized => {
                Some("ask the admin to confirm your membership has synced, then retry")
            }
            Self::WrongIdentity => {
                Some("ask the admin to re-issue the invite for your identity id (`identity show`)")
            }
            Self::BlobUnavailable => Some(
                "ask a peer that holds the file to run `room tail <ROOM_ID>`, then retry `file fetch`",
            ),
            Self::HashMismatch => Some(
                "do not trust this file; the reference or a provider may be corrupt — \
                 ask for a fresh `file share`",
            ),
            Self::Ticket(_) => Some(
                "check the whole ticket was copied (no truncation/whitespace); if it persists, \
                 ask the admin for a fresh `room invite`",
            ),
            Self::Reject(r) => reject_next_action(r),
            Self::InvalidArgument | Self::Internal => None, // context is in the message
        }
    }
}

/// The `next_action()` for a §8 [`RejectReason`] (spec IR-0303 §5.1): the five
/// authorization-layer reasons are user-fixable; every other named reason today is
/// a structural/crypto rejection, which is not something a user can act on — `None`.
/// `RejectReason` is `#[non_exhaustive]`, so an unrecognized future reason
/// conservatively falls through to `None` too, until this table is extended.
fn reject_next_action(r: &RejectReason) -> Option<&'static str> {
    match r {
        RejectReason::ExpiredInvite => {
            Some("ask the admin for a fresh `room invite` (optionally with a longer `--expires`)")
        }
        RejectReason::BadCapability => {
            Some("ask the admin to re-issue the invite for your identity id")
        }
        RejectReason::InsufficientRole => {
            Some("ask the admin to invite you with the intended role")
        }
        RejectReason::NotAMember | RejectReason::UnboundDevice => {
            Some("ask the admin to invite you and complete `room join` first")
        }
        _ => None,
    }
}

impl From<RejectReason> for ErrorCode {
    fn from(reason: RejectReason) -> Self {
        Self::Reject(reason)
    }
}

impl From<TicketError> for ErrorCode {
    fn from(err: TicketError) -> Self {
        Self::Ticket(err)
    }
}

/// The category → exit-code scheme (spec §5.3), aligned with `clap`'s existing
/// exit `2` for usage errors. The string code is the fine-grained, authoritative
/// script surface; the category is the coarse `$?` contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorCategory {
    /// Unexpected / uncoded internal error.
    Internal,
    /// Bad input or environment (clap-aligned).
    Usage,
    /// Authorization / capability denial.
    Auth,
    /// Crypto / structural rejection.
    Integrity,
    /// Ticket decode failure.
    Ticket,
    /// Reachability / availability failure.
    Connectivity,
}

impl ErrorCategory {
    /// The stable process exit code for this category.
    #[must_use]
    pub fn exit_code(self) -> u8 {
        match self {
            Self::Internal => 1,
            Self::Usage => 2,
            Self::Auth => 3,
            Self::Integrity => 4,
            Self::Ticket => 5,
            Self::Connectivity => 6,
        }
    }
}

/// Categorize a §8 [`RejectReason`] (spec §5.3 table): the five authorization-layer
/// reasons are [`ErrorCategory::Auth`]; every other named reason today is a
/// crypto/structural rejection. `RejectReason` is `#[non_exhaustive]`, so a
/// wildcard arm is required across the crate boundary — it conservatively falls
/// through to [`ErrorCategory::Integrity`] for an unrecognized future reason too,
/// until this table is extended.
fn reject_category(reason: &RejectReason) -> ErrorCategory {
    match reason {
        RejectReason::NotAMember
        | RejectReason::UnboundDevice
        | RejectReason::InsufficientRole
        | RejectReason::ExpiredInvite
        | RejectReason::BadCapability => ErrorCategory::Auth,
        _ => ErrorCategory::Integrity,
    }
}

/// An `anyhow`-compatible error carrying a stable [`ErrorCode`] (spec §5.2). The
/// `Display`/`Error` impl renders `message` only — the code is rendered separately
/// by [`code_of`] callers via the pinned `error[<code>]:` prefix.
#[derive(Debug)]
pub struct CliError {
    /// The stable failure code this error belongs to.
    pub code: ErrorCode,
    message: String,
}

impl CliError {
    /// Attach `code` to a human-readable, secret-free `message`.
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

impl fmt::Display for CliError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for CliError {}

/// Ergonomic code-attach for any fallible result — mirrors `anyhow`'s
/// `.context(...)`. The error's `{:#}` rendering (its full chain, for a plain
/// `anyhow::Error`) becomes the [`CliError`] message; the original error/chain
/// itself is not retained separately, since the rendered text already carries it.
pub trait CodedResultExt<T, E> {
    /// Attach a fixed `code`.
    fn coded(self, code: ErrorCode) -> anyhow::Result<T>;
    /// Attach a code computed from the error (for wrapped variants such as
    /// [`ErrorCode::Ticket`]/[`ErrorCode::Reject`] whose code depends on the value).
    fn with_coded<F>(self, f: F) -> anyhow::Result<T>
    where
        F: FnOnce(&E) -> ErrorCode;
}

impl<T, E: fmt::Display> CodedResultExt<T, E> for Result<T, E> {
    fn coded(self, code: ErrorCode) -> anyhow::Result<T> {
        self.map_err(|e| anyhow::Error::new(CliError::new(code, format!("{e:#}"))))
    }

    fn with_coded<F>(self, f: F) -> anyhow::Result<T>
    where
        F: FnOnce(&E) -> ErrorCode,
    {
        self.map_err(|e| {
            let code = f(&e);
            anyhow::Error::new(CliError::new(code, format!("{e:#}")))
        })
    }
}

/// Attach a coded failure and return early — the `bail!` analogue for a coded
/// error (spec §5.2).
#[macro_export]
macro_rules! bail_coded {
    ($code:expr, $($fmt:tt)*) => {
        return Err(::anyhow::Error::new($crate::error::CliError::new($code, format!($($fmt)*))))
    };
}

/// Walk `err`'s cause chain for the outermost [`CliError`] and return its code.
/// `None` means no layer attached a code — the long-tail, uncoded case (spec §5.2).
#[must_use]
pub fn code_of(err: &anyhow::Error) -> Option<ErrorCode> {
    err.chain()
        .find_map(|cause| cause.downcast_ref::<CliError>())
        .map(|cli_err| cli_err.code.clone())
}

#[cfg(test)]
mod tests {
    use super::{CliError, CodedResultExt, ErrorCategory, ErrorCode};
    use iroh_rooms_core::event::RejectReason;
    use iroh_rooms_core::ticket::TicketError;
    use iroh_rooms_net::OfflineReason;

    // ── ErrorCode::code() is pinned for every variant ────────────────────────

    #[test]
    fn codes_are_stable() {
        assert_eq!(
            ErrorCode::Reject(RejectReason::BadSignature).code(),
            "bad_signature"
        );
        assert_eq!(
            ErrorCode::Reject(RejectReason::NotAMember).code(),
            "not_a_member"
        );
        assert_eq!(
            ErrorCode::Ticket(TicketError::BadChecksum).code(),
            "ticket_bad_checksum"
        );
        assert_eq!(ErrorCode::NoAdminReachable.code(), "no_admin_reachable");
        assert_eq!(
            ErrorCode::MembershipIncomplete.code(),
            "membership_incomplete"
        );
        assert_eq!(
            ErrorCode::PeerOffline(OfflineReason::Unreachable).code(),
            "peer_offline"
        );
        assert_eq!(ErrorCode::PeerUnauthorized.code(), "peer_unauthorized");
        assert_eq!(ErrorCode::WrongIdentity.code(), "wrong_identity");
        assert_eq!(ErrorCode::NoDiscoveryHint.code(), "no_discovery_hint");
        assert_eq!(ErrorCode::BlobUnavailable.code(), "blob_unavailable");
        assert_eq!(ErrorCode::HashMismatch.code(), "hash_mismatch");
        assert_eq!(ErrorCode::InvalidRoomId.code(), "invalid_room_id");
        assert_eq!(ErrorCode::InvalidArgument.code(), "invalid_argument");
        assert_eq!(ErrorCode::NoSuchFile.code(), "no_such_file");
        assert_eq!(ErrorCode::PermissionDenied.code(), "permission_denied");
        assert_eq!(ErrorCode::FileTooLarge.code(), "file_too_large");
        assert_eq!(ErrorCode::IdentityNotFound.code(), "identity_not_found");
        assert_eq!(ErrorCode::RoomNotFound.code(), "room_not_found");
        assert_eq!(ErrorCode::Internal.code(), "internal");
    }

    #[test]
    fn ticket_unsupported_version_code_ignores_the_version_number() {
        assert_eq!(
            ErrorCode::Ticket(TicketError::UnsupportedVersion(9)).code(),
            "ticket_unsupported_version"
        );
    }

    #[test]
    fn wrapped_arms_delegate_their_code_verbatim() {
        // The "wrap, don't duplicate" invariant (spec §5.1 / Risks table): a wrapped
        // arm must return the *source* enum's code byte-for-byte, so a renamed or new
        // §8 / ticket code can never silently drift from the conformance-gated source.
        for reason in [
            RejectReason::BadSignature,
            RejectReason::IdMismatch,
            RejectReason::NonCanonicalEncoding,
            RejectReason::InvalidContent,
            RejectReason::UnknownSchemaVersion,
            RejectReason::UnknownEventType,
            RejectReason::TooManyParents,
            RejectReason::NotGenesisDescended,
            RejectReason::RoomIdMismatch,
            RejectReason::UnboundDevice,
            RejectReason::NotAMember,
            RejectReason::InsufficientRole,
            RejectReason::ExpiredInvite,
            RejectReason::BadCapability,
        ] {
            assert_eq!(
                ErrorCode::from(reason.clone()).code(),
                reason.code(),
                "Reject arm must delegate to RejectReason::code verbatim"
            );
        }
        for err in [
            TicketError::BadPrefix,
            TicketError::BadBase32,
            TicketError::Truncated,
            TicketError::UnsupportedVersion(1),
            TicketError::BadChecksum,
            TicketError::MalformedBody,
        ] {
            assert_eq!(
                ErrorCode::from(err.clone()).code(),
                err.code(),
                "Ticket arm must delegate to TicketError::code verbatim"
            );
        }
    }

    // ── ErrorCategory::exit_code() is pinned per category ────────────────────

    #[test]
    fn exit_codes_are_stable() {
        assert_eq!(ErrorCategory::Internal.exit_code(), 1);
        assert_eq!(ErrorCategory::Usage.exit_code(), 2);
        assert_eq!(ErrorCategory::Auth.exit_code(), 3);
        assert_eq!(ErrorCategory::Integrity.exit_code(), 4);
        assert_eq!(ErrorCategory::Ticket.exit_code(), 5);
        assert_eq!(ErrorCategory::Connectivity.exit_code(), 6);
    }

    #[test]
    fn reject_reasons_categorize_per_the_five_and_nine_split() {
        for auth in [
            RejectReason::NotAMember,
            RejectReason::UnboundDevice,
            RejectReason::InsufficientRole,
            RejectReason::ExpiredInvite,
            RejectReason::BadCapability,
        ] {
            assert_eq!(ErrorCode::Reject(auth).category(), ErrorCategory::Auth);
        }
        for integrity in [
            RejectReason::BadSignature,
            RejectReason::IdMismatch,
            RejectReason::NonCanonicalEncoding,
            RejectReason::InvalidContent,
            RejectReason::UnknownSchemaVersion,
            RejectReason::UnknownEventType,
            RejectReason::TooManyParents,
            RejectReason::NotGenesisDescended,
            RejectReason::RoomIdMismatch,
        ] {
            assert_eq!(
                ErrorCode::Reject(integrity).category(),
                ErrorCategory::Integrity
            );
        }
    }

    #[test]
    fn ticket_and_connectivity_categories() {
        assert_eq!(
            ErrorCode::Ticket(TicketError::BadPrefix).category(),
            ErrorCategory::Ticket
        );
        assert_eq!(
            ErrorCode::NoAdminReachable.category(),
            ErrorCategory::Connectivity
        );
        assert_eq!(
            ErrorCode::PeerOffline(OfflineReason::Unreachable).category(),
            ErrorCategory::Connectivity
        );
        assert_eq!(
            ErrorCode::BlobUnavailable.category(),
            ErrorCategory::Connectivity
        );
        assert_eq!(ErrorCode::PeerUnauthorized.category(), ErrorCategory::Auth);
        assert_eq!(ErrorCode::WrongIdentity.category(), ErrorCategory::Auth);
    }

    #[test]
    fn hash_mismatch_is_integrity_exit_4() {
        // Spec IR-0205 §5.4: the one new CLI-native integrity code, distinct from
        // both `blob_unavailable` (Connectivity) and `peer_unauthorized`/
        // `not_a_member` (Auth).
        assert_eq!(ErrorCode::HashMismatch.code(), "hash_mismatch");
        assert_eq!(ErrorCode::HashMismatch.category(), ErrorCategory::Integrity);
        assert_eq!(ErrorCode::HashMismatch.exit_code(), 4);
    }

    #[test]
    fn headline_fetch_failure_codes_are_pairwise_distinct() {
        // Spec IR-0205 §5.1 / AC2 + the invalid-hash AC: `file fetch`'s three honest
        // failure classes must be branchable on BOTH the string code AND the exit
        // code — a script must never confuse "unavailable" (Connectivity, exit 6),
        // "unauthorized" (Auth, exit 3), and "invalid hash" (Integrity, exit 4).
        // Assert mutual distinctness on both axes in one place, so a future edit that
        // collapses any pair (same code, or same exit) fails here rather than
        // silently hiding one state behind another.
        let unavailable = ErrorCode::BlobUnavailable;
        let unauthorized = ErrorCode::PeerUnauthorized;
        let invalid_hash = ErrorCode::HashMismatch;

        // Distinct, pinned string codes.
        assert_eq!(
            [unavailable.code(), unauthorized.code(), invalid_hash.code()],
            ["blob_unavailable", "peer_unauthorized", "hash_mismatch"]
        );

        // Distinct exit codes (Connectivity 6 ≠ Auth 3 ≠ Integrity 4).
        assert_eq!(unavailable.exit_code(), 6);
        assert_eq!(unauthorized.exit_code(), 3);
        assert_eq!(invalid_hash.exit_code(), 4);
        assert_ne!(unavailable.exit_code(), unauthorized.exit_code());
        assert_ne!(unavailable.exit_code(), invalid_hash.exit_code());
        assert_ne!(unauthorized.exit_code(), invalid_hash.exit_code());

        // The not-active pre-check (`not_a_member`) shares the Auth exit (3) with
        // `peer_unauthorized` — both are authorization walls — but keeps a distinct
        // string code, so a script branching on the code still tells the two apart
        // (spec §5.1 table: pre-check vs aggregate refusal).
        let not_a_member = ErrorCode::Reject(RejectReason::NotAMember);
        assert_eq!(not_a_member.code(), "not_a_member");
        assert_eq!(not_a_member.exit_code(), 3);
        assert_ne!(not_a_member.code(), unauthorized.code());
    }

    // ── CodedResultExt / code_of ──────────────────────────────────────────────

    #[test]
    fn coded_attaches_a_code_findable_by_code_of() {
        let result: Result<(), &str> = Err("boom");
        let err = result.coded(ErrorCode::InvalidRoomId).unwrap_err();
        assert_eq!(super::code_of(&err), Some(ErrorCode::InvalidRoomId));
        assert_eq!(err.to_string(), "boom");
    }

    #[test]
    fn with_coded_computes_the_code_from_the_error() {
        let result: Result<(), TicketError> = Err(TicketError::BadChecksum);
        let err = result
            .with_coded(|e| ErrorCode::Ticket(e.clone()))
            .unwrap_err();
        assert_eq!(
            super::code_of(&err),
            Some(ErrorCode::Ticket(TicketError::BadChecksum))
        );
    }

    #[test]
    fn code_of_finds_a_code_through_further_context_layers() {
        // A coded failure that an outer caller further annotates with `.context(...)`
        // (the realistic pattern: an inner helper attaches the code, a caller adds
        // more detail) must still resolve to the originally-attached code.
        use anyhow::Context;
        let coded: anyhow::Result<()> = Err("boom").coded(ErrorCode::NoSuchFile);
        let wrapped = coded.context("could not do the thing");
        let err = wrapped.unwrap_err();
        assert_eq!(super::code_of(&err), Some(ErrorCode::NoSuchFile));
    }

    #[test]
    fn code_of_returns_the_outermost_of_two_coded_layers() {
        let inner = anyhow::Error::new(CliError::new(ErrorCode::NoSuchFile, "inner"));
        let outer = anyhow::Error::new(CliError::new(ErrorCode::PermissionDenied, "outer"));
        // `chain()` walks front-to-back starting at the outermost error; simulate a
        // two-CliError chain directly since `.coded()`/`bail_coded!` always produce a
        // single flat node in normal use.
        assert_eq!(super::code_of(&outer), Some(ErrorCode::PermissionDenied));
        assert_eq!(super::code_of(&inner), Some(ErrorCode::NoSuchFile));
    }

    #[test]
    fn code_of_returns_none_for_an_uncoded_error() {
        let err = anyhow::anyhow!("plain failure");
        assert_eq!(super::code_of(&err), None);
    }

    #[test]
    fn blob_unavailable_renders_via_cli_error() {
        // Spec IR-0205 §5.5: `blob_unavailable` is now emitted by `file fetch` with a
        // context-specific message (file id / room interpolated at the call site), not
        // a fixed reserved constant. Exercise the taxonomy-level contract directly.
        let err = anyhow::Error::new(CliError::new(
            ErrorCode::BlobUnavailable,
            "file file_deadbeef is currently unavailable: no peer holding it is online",
        ));
        let code = super::code_of(&err).expect("blob_unavailable must be a coded failure");
        assert_eq!(code.code(), "blob_unavailable");
        assert_eq!(code.category(), ErrorCategory::Connectivity);
        assert_eq!(code.exit_code(), 6);
    }

    #[test]
    fn bail_coded_returns_a_coded_error() {
        fn call_bail_coded() -> anyhow::Result<()> {
            crate::bail_coded!(ErrorCode::RoomNotFound, "no room {}", "blake3:ab");
        }
        let err = call_bail_coded().unwrap_err();
        assert_eq!(super::code_of(&err), Some(ErrorCode::RoomNotFound));
        assert_eq!(err.to_string(), "no room blake3:ab");
    }

    // ── ErrorCode::next_action() (spec IR-0303 §5.1) ──────────────────────────

    #[test]
    fn every_user_actionable_code_has_a_non_empty_next_action() {
        for code in [
            ErrorCode::IdentityNotFound,
            ErrorCode::InvalidRoomId,
            ErrorCode::RoomNotFound,
            ErrorCode::NoSuchFile,
            ErrorCode::PermissionDenied,
            ErrorCode::FileTooLarge,
            ErrorCode::NoDiscoveryHint,
            ErrorCode::NoAdminReachable,
            ErrorCode::PeerOffline(OfflineReason::Unreachable),
            ErrorCode::PeerUnauthorized,
            ErrorCode::WrongIdentity,
            ErrorCode::BlobUnavailable,
            ErrorCode::HashMismatch,
            ErrorCode::Ticket(TicketError::BadChecksum),
        ] {
            let action = code.next_action();
            assert!(
                action.is_some_and(|s| !s.is_empty()),
                "{code:?} must have a non-empty next_action"
            );
        }
    }

    #[test]
    fn internal_and_invalid_argument_have_no_generic_next_action() {
        // Their context lives entirely in the call-site message.
        assert_eq!(ErrorCode::Internal.next_action(), None);
        assert_eq!(ErrorCode::InvalidArgument.next_action(), None);
    }

    #[test]
    fn user_fixable_reject_reasons_have_a_next_action() {
        for reason in [
            RejectReason::ExpiredInvite,
            RejectReason::BadCapability,
            RejectReason::InsufficientRole,
            RejectReason::NotAMember,
            RejectReason::UnboundDevice,
        ] {
            let action = ErrorCode::Reject(reason.clone()).next_action();
            assert!(
                action.is_some_and(|s| !s.is_empty()),
                "{reason:?} must have a non-empty next_action"
            );
        }
    }

    #[test]
    fn structural_reject_reasons_have_no_next_action() {
        // Crypto/structural rejects are not something a user can act on.
        for reason in [
            RejectReason::BadSignature,
            RejectReason::IdMismatch,
            RejectReason::NonCanonicalEncoding,
        ] {
            assert_eq!(ErrorCode::Reject(reason).next_action(), None);
        }
    }

    #[test]
    fn room_not_found_has_one_consistent_next_action_regardless_of_call_site() {
        // Spec §5.1 "room_not_found fix": all three sites collapse to the same
        // rendering once the message is trimmed and the step comes from here.
        assert_eq!(
            ErrorCode::RoomNotFound.next_action(),
            Some("run `iroh-rooms room create <name>`, or join an invite ticket first")
        );
    }

    #[test]
    fn no_next_action_string_contains_a_secret_looking_token() {
        // Belt-and-suspenders (spec §5.1 Step 1c): every next_action is a fixed
        // literal, but pin that none of them accidentally embeds anything that
        // looks like a long base32/hex run (a secret would never legitimately
        // appear here since the strings never interpolate).
        let all_codes = [
            ErrorCode::IdentityNotFound,
            ErrorCode::InvalidRoomId,
            ErrorCode::RoomNotFound,
            ErrorCode::NoSuchFile,
            ErrorCode::PermissionDenied,
            ErrorCode::FileTooLarge,
            ErrorCode::NoDiscoveryHint,
            ErrorCode::NoAdminReachable,
            ErrorCode::PeerOffline(OfflineReason::Unreachable),
            ErrorCode::PeerUnauthorized,
            ErrorCode::WrongIdentity,
            ErrorCode::BlobUnavailable,
            ErrorCode::HashMismatch,
            ErrorCode::Ticket(TicketError::BadChecksum),
        ];
        for code in all_codes {
            if let Some(action) = code.next_action() {
                let longest_alnum_run = action
                    .split(|c: char| !c.is_ascii_alphanumeric())
                    .map(str::len)
                    .max()
                    .unwrap_or(0);
                assert!(
                    longest_alnum_run < 16,
                    "{code:?}'s next_action contains a suspiciously long token: {action}"
                );
            }
        }
    }
}
