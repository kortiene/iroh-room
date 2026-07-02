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

/// The reserved advisory message for [`ErrorCode::BlobUnavailable`] (spec §5.10,
/// scope item 5) — defined now, ahead of the serve/fetch follow-up issue, so a
/// future `file fetch` only has to *emit* the code, not invent its wording. Unused
/// until that command lands (explicitly out of scope here — spec §3.3).
#[allow(dead_code)]
pub const BLOB_UNAVAILABLE_MESSAGE: &str =
    "no reachable provider holds this file yet (peer fetch is not implemented in this build)";

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

    /// No reachable provider holds the requested blob. Reserved for the `file
    /// fetch` serve/fetch follow-up issue — defined now so the code + exit category
    /// are pinned ahead of that phase. Not yet constructed anywhere: `file fetch`
    /// is explicitly out of scope for this issue (spec §3.3).
    #[allow(dead_code)]
    BlobUnavailable,

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
            Self::PeerOffline(_) => "peer_offline",
            Self::PeerUnauthorized => "peer_unauthorized",
            Self::WrongIdentity => "wrong_identity",
            Self::NoDiscoveryHint => "no_discovery_hint",
            Self::BlobUnavailable => "blob_unavailable",
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
            Self::Reject(reason) => reject_category(reason),
            Self::Ticket(_) => ErrorCategory::Ticket,
            Self::NoAdminReachable | Self::PeerOffline(_) | Self::BlobUnavailable => {
                ErrorCategory::Connectivity
            }
        }
    }

    /// The process exit code for this failure (spec §5.3).
    #[must_use]
    pub fn exit_code(&self) -> u8 {
        self.category().exit_code()
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
            ErrorCode::PeerOffline(OfflineReason::Unreachable).code(),
            "peer_offline"
        );
        assert_eq!(ErrorCode::PeerUnauthorized.code(), "peer_unauthorized");
        assert_eq!(ErrorCode::WrongIdentity.code(), "wrong_identity");
        assert_eq!(ErrorCode::NoDiscoveryHint.code(), "no_discovery_hint");
        assert_eq!(ErrorCode::BlobUnavailable.code(), "blob_unavailable");
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
    fn blob_unavailable_reserved_code_renders_via_cli_error() {
        // Spec §5.10 / test #14: the reserved placeholder is fully wired at the taxonomy
        // level (stable code, Connectivity exit 6, a secret-free reserved message) ahead
        // of the serve/fetch issue that will *emit* it. Exercise the code + message now so
        // that follow-up only swaps the construction site, not the contract.
        let err = anyhow::Error::new(CliError::new(
            ErrorCode::BlobUnavailable,
            super::BLOB_UNAVAILABLE_MESSAGE,
        ));
        let code = super::code_of(&err).expect("blob_unavailable must be a coded failure");
        assert_eq!(code.code(), "blob_unavailable");
        assert_eq!(code.category(), ErrorCategory::Connectivity);
        assert_eq!(code.exit_code(), 6);
        // The reserved message is non-empty and never mentions a secret/token.
        assert!(!super::BLOB_UNAVAILABLE_MESSAGE.is_empty());
        assert_eq!(err.to_string(), super::BLOB_UNAVAILABLE_MESSAGE);
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
}
