//! The closed set of signals the job layer sends on Darwin.
//!
//! Same variant names as `harw_job_linux::SignalKind` (naming symmetry), but
//! the raw numbers are Darwin's (`<sys/signal.h>`): e.g. `SIGUSR1` is 30 and
//! `SIGSTOP` 17 on Darwin, not 10 and 19 as on Linux x86-64.

/// Signals the job layer sends. A closed set: no arbitrary signal numbers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum SignalKind {
    /// `SIGTERM` (15).
    Term,
    /// `SIGKILL` (9).
    Kill,
    /// `SIGINT` (2).
    Int,
    /// `SIGHUP` (1).
    Hup,
    /// `SIGQUIT` (3).
    Quit,
    /// `SIGUSR1` (30 on Darwin).
    Usr1,
    /// `SIGUSR2` (31 on Darwin).
    Usr2,
    /// `SIGSTOP` (17 on Darwin).
    Stop,
    /// `SIGCONT` (19 on Darwin).
    Cont,
}

impl SignalKind {
    /// Raw signal number on Darwin (XNU `<sys/signal.h>`), independent of
    /// the build target, so a report can name it anywhere.
    #[must_use]
    pub const fn as_raw(self) -> i32 {
        match self {
            Self::Hup => 1,
            Self::Int => 2,
            Self::Quit => 3,
            Self::Kill => 9,
            Self::Term => 15,
            Self::Stop => 17,
            Self::Cont => 19,
            Self::Usr1 => 30,
            Self::Usr2 => 31,
        }
    }

    /// Conventional name such as `SIGTERM`.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Term => "SIGTERM",
            Self::Kill => "SIGKILL",
            Self::Int => "SIGINT",
            Self::Hup => "SIGHUP",
            Self::Quit => "SIGQUIT",
            Self::Usr1 => "SIGUSR1",
            Self::Usr2 => "SIGUSR2",
            Self::Stop => "SIGSTOP",
            Self::Cont => "SIGCONT",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::SignalKind;

    #[test]
    fn test_signal_kind_darwin_numbers_and_names() {
        assert_eq!(SignalKind::Term.as_raw(), 15);
        assert_eq!(SignalKind::Kill.as_raw(), 9);
        assert_eq!(SignalKind::Usr1.as_raw(), 30);
        assert_eq!(SignalKind::Stop.as_raw(), 17);
        assert_eq!(SignalKind::Term.name(), "SIGTERM");
        assert_eq!(SignalKind::Cont.name(), "SIGCONT");
    }
}
