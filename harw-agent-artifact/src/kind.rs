//! [`PayloadKind`]: what a payload in the table is.

use std::fmt;

use crate::error::ArtifactError;

/// Maximum length in bytes of an [`PayloadKind::Other`] name.
pub const MAX_KIND_NAME_LEN: usize = 64;

/// Wire tag of [`PayloadKind::Other`]; followed by a u16 name length and the name.
pub(crate) const TAG_OTHER: u8 = 0xFF;

/// What a payload is. The derived order (variant order = wire tag order,
/// then name for `Other`) is the first sort key of the payload table.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum PayloadKind {
    /// The resolved instruction text (wire tag 1).
    Instructions,
    /// An embedded skill file (wire tag 2).
    Skill,
    /// A knowledge file the agent may read (wire tag 3).
    Knowledge,
    /// A bound context program (wire tag 4).
    ContextProgram,
    /// A prompt or output template (wire tag 5).
    Template,
    /// Anything else, named (wire tag `0xFF`, then a u16 name length and
    /// the name). The name is 1..=64 bytes of `[a-z0-9._-]` and must not
    /// collide with a built-in kind name. Sorts after every built-in kind.
    Other(String),
}

impl PayloadKind {
    /// Stable lowercase name, used in `Display` and in diagnostics.
    #[must_use]
    pub fn as_str(&self) -> &str {
        match self {
            Self::Instructions => "instructions",
            Self::Skill => "skill",
            Self::Knowledge => "knowledge",
            Self::ContextProgram => "context-program",
            Self::Template => "template",
            Self::Other(name) => name,
        }
    }

    /// Wire tag; `TAG_OTHER` for [`PayloadKind::Other`], whose name follows
    /// the tag on the wire.
    pub(crate) fn tag(&self) -> u8 {
        match self {
            Self::Instructions => 1,
            Self::Skill => 2,
            Self::Knowledge => 3,
            Self::ContextProgram => 4,
            Self::Template => 5,
            Self::Other(_) => TAG_OTHER,
        }
    }

    /// Built-in kind for a wire tag; `None` for `TAG_OTHER` and unknown tags.
    pub(crate) fn from_builtin_tag(tag: u8) -> Option<Self> {
        match tag {
            1 => Some(Self::Instructions),
            2 => Some(Self::Skill),
            3 => Some(Self::Knowledge),
            4 => Some(Self::ContextProgram),
            5 => Some(Self::Template),
            _ => None,
        }
    }

    /// Checks the rules for an `Other` name. Built-in kinds always pass.
    ///
    /// # Errors
    /// [`ArtifactError::InvalidKind`] when an `Other` name is empty, too
    /// long, uses characters outside `[a-z0-9._-]` or equals a built-in name.
    pub(crate) fn validate(&self) -> Result<(), ArtifactError> {
        let Self::Other(name) = self else {
            return Ok(());
        };
        let invalid = |why: &str| ArtifactError::InvalidKind {
            detail: format!("{name:?}: {why}"),
        };
        if name.is_empty() {
            return Err(invalid("empty name"));
        }
        if name.len() > MAX_KIND_NAME_LEN {
            return Err(invalid("name longer than 64 bytes"));
        }
        let allowed = |c: char| c.is_ascii_lowercase() || c.is_ascii_digit() || "._-".contains(c);
        if !name.chars().all(allowed) {
            return Err(invalid("name must use only [a-z0-9._-]"));
        }
        let builtin = [
            "skill",
            "knowledge",
            "instructions",
            "context-program",
            "template",
        ];
        if builtin.contains(&name.as_str()) {
            return Err(invalid("name collides with a built-in kind"));
        }
        Ok(())
    }
}

impl fmt::Display for PayloadKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}
