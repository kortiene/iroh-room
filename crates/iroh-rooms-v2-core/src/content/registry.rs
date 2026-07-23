//! The closed v2 content-kind registry (spec #152 / source:
//! `content-and-moderation-event-schemas.md` §4 D1).
//!
//! The registry is closed: an unknown kind is rejected as
//! [`crate::Reject::UnknownContentKind`] (the §6.4 rule). Extending it requires a
//! `schema_version` bump + registry amendment (the v1 forward-compat rule).

use crate::error::Reject;

/// The registered v2 content kinds (source: §4 D1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ContentKind {
    /// Text message body (`message.text`).
    MessageText,
    /// Emoji reaction on a target event (`message.reaction`).
    MessageReaction,
    /// Edit of a prior message (`message.edited`).
    MessageEdited,
    /// Content-addressed blob reference (`file.shared`).
    FileShared,
    /// Agent status label + progress (`agent.status`).
    AgentStatus,
    /// Stream/room block of a subject (`moderation.block`).
    ModerationBlock,
    /// Member report of a subject/event (`moderation.report`).
    ModerationReport,
    /// Content tombstone on a target event (`moderation.remove`).
    ModerationRemove,
}

impl ContentKind {
    /// The wire discriminant string (closed registry).
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::MessageText => "message.text",
            Self::MessageReaction => "message.reaction",
            Self::MessageEdited => "message.edited",
            Self::FileShared => "file.shared",
            Self::AgentStatus => "agent.status",
            Self::ModerationBlock => "moderation.block",
            Self::ModerationReport => "moderation.report",
            Self::ModerationRemove => "moderation.remove",
        }
    }

    /// Parse a kind from its wire string.
    ///
    /// # Errors
    /// Returns [`Reject::UnknownContentKind`] for an unregistered kind (§6.4).
    pub fn from_wire(s: &str) -> Result<Self, Reject> {
        match s {
            "message.text" => Ok(Self::MessageText),
            "message.reaction" => Ok(Self::MessageReaction),
            "message.edited" => Ok(Self::MessageEdited),
            "file.shared" => Ok(Self::FileShared),
            "agent.status" => Ok(Self::AgentStatus),
            "moderation.block" => Ok(Self::ModerationBlock),
            "moderation.report" => Ok(Self::ModerationReport),
            "moderation.remove" => Ok(Self::ModerationRemove),
            _ => Err(Reject::UnknownContentKind),
        }
    }
}

impl core::str::FromStr for ContentKind {
    type Err = Reject;
    fn from_str(s: &str) -> Result<Self, Reject> {
        Self::from_wire(s)
    }
}

/// All registered kinds, in registry order.
#[must_use]
pub fn all() -> Vec<ContentKind> {
    [
        ContentKind::MessageText,
        ContentKind::MessageReaction,
        ContentKind::MessageEdited,
        ContentKind::FileShared,
        ContentKind::AgentStatus,
        ContentKind::ModerationBlock,
        ContentKind::ModerationReport,
        ContentKind::ModerationRemove,
    ]
    .to_vec()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_kinds_round_trip() {
        for k in all() {
            assert_eq!(ContentKind::from_wire(k.as_str()).unwrap(), k);
        }
    }

    #[test]
    fn unknown_kind_rejected() {
        assert_eq!(
            ContentKind::from_wire("message.unknown"),
            Err(Reject::UnknownContentKind)
        );
    }
}
