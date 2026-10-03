//! Instance lifecycle.

use serde::{Deserialize, Serialize};

/// Where an instance is in its life. There is deliberately no `Ord`: the
/// order of the variants is not a ranking (a `max` over observations must
/// never let `Lost` outrank `Exited`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Lifecycle {
    /// Asked for, nothing exists yet.
    Requested,
    /// Created, not started.
    Created,
    /// Running.
    Running,
    /// The main process ended; the instance still exists. Its exit code must
    /// survive until it is recorded.
    Exited,
    /// Removed after it ended (or discarded before it ever ran).
    Reaped,
    /// The engine did not know it at the last observation. Only an
    /// observation: callers must not report `Lost` for an unreachable
    /// engine, and a later positive observation resumes the instance.
    Lost,
}

impl Lifecycle {
    /// Whether nothing more can happen to the instance.
    #[must_use]
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Reaped)
    }

    /// Whether `self -> next` is a legal step. Explicit table:
    /// `Requested -> Created`; `Created -> Running | Exited | Reaped | Lost`;
    /// `Running -> Exited | Lost`; `Exited -> Reaped | Lost`;
    /// `Lost -> Running | Exited | Reaped` (a positive observation resumes).
    /// `Exited` is never skipped on the way to `Reaped` from `Running`, so
    /// the exit code cannot be lost.
    #[must_use]
    pub const fn can_advance_to(self, next: Self) -> bool {
        use Lifecycle::{Created, Exited, Lost, Reaped, Requested, Running};
        matches!(
            (self, next),
            (Requested, Created)
                | (Created, Running | Exited | Reaped | Lost)
                | (Running, Exited | Lost)
                | (Exited, Reaped | Lost)
                | (Lost, Running | Exited | Reaped)
        )
    }
}

#[cfg(test)]
mod tests {
    use super::Lifecycle::{self, Created, Exited, Lost, Reaped, Requested, Running};

    const ALL: [Lifecycle; 6] = [Requested, Created, Running, Exited, Reaped, Lost];

    #[test]
    fn the_complete_transition_matrix_is_pinned() {
        let allowed = [
            (Requested, Created),
            (Created, Running),
            (Created, Exited),
            (Created, Reaped),
            (Created, Lost),
            (Running, Exited),
            (Running, Lost),
            (Exited, Reaped),
            (Exited, Lost),
            (Lost, Running),
            (Lost, Exited),
            (Lost, Reaped),
        ];
        for from in ALL {
            for to in ALL {
                assert_eq!(
                    from.can_advance_to(to),
                    allowed.contains(&(from, to)),
                    "{from:?} -> {to:?}"
                );
            }
        }
    }

    #[test]
    fn exit_codes_cannot_be_skipped_and_only_reaped_is_final() {
        assert!(!Running.can_advance_to(Reaped));
        assert!(!Requested.can_advance_to(Exited));
        assert!(Reaped.is_terminal());
        assert!(!Lost.is_terminal() && !Exited.is_terminal());
        for to in ALL {
            assert!(!Reaped.can_advance_to(to));
        }
    }
}
