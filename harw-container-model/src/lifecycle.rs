//! Instance lifecycle.

use serde::{Deserialize, Serialize};

/// Where an instance is in its life. Only moves forward.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Lifecycle {
    /// Asked for, nothing exists yet.
    Requested,
    /// Created, not started.
    Created,
    /// Running.
    Running,
    /// The main process ended; the instance still exists.
    Exited,
    /// Removed after it ended.
    Reaped,
    /// The engine no longer knows it (daemon wiped, node gone).
    Lost,
}

impl Lifecycle {
    /// Whether nothing more can happen to the instance.
    #[must_use]
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Reaped | Self::Lost)
    }

    /// Whether `self -> next` is a legal step: strictly forward along
    /// `Requested -> Created -> Running -> Exited -> Reaped`, skipping allowed
    /// (a fast job can go `Created -> Exited`), and `Lost` from any
    /// non-terminal state.
    #[must_use]
    pub const fn can_advance_to(self, next: Self) -> bool {
        if self.is_terminal() {
            return false;
        }
        match next {
            Self::Lost => true,
            Self::Requested => false,
            _ => (next as u8) > (self as u8),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lifecycle_only_moves_forward_and_lost_is_reachable_until_terminal() {
        use Lifecycle::{Created, Exited, Lost, Reaped, Requested, Running};
        assert!(Requested.can_advance_to(Created));
        assert!(Created.can_advance_to(Exited), "a fast job skips Running");
        assert!(Running.can_advance_to(Lost));
        assert!(!Running.can_advance_to(Created));
        assert!(!Exited.can_advance_to(Requested));
        assert!(!Reaped.can_advance_to(Lost), "terminal");
        assert!(!Lost.can_advance_to(Reaped), "terminal");
        assert!(Reaped.is_terminal() && Lost.is_terminal() && !Exited.is_terminal());
    }
}
