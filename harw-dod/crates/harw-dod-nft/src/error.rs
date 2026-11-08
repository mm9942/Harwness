//! Fehler dieser Crate: inhaltsfrei, damit ein geloggter Fehler weder eine
//! cgroup-Kennung noch einen Kettennamen noch Kernel-Antwortbytes preisgibt.
//!
//! # Verantwortungsbereich
//! [`NftError`] ist der eine Fehlertyp von `harw-dod-nft`. Er kennt vier
//! Gruppen:
//!
//! 1. **Eingabeprüfung** ([`crate::CgroupRule::new`],
//!    [`crate::NftNetlink::release_chains`]): [`NftError::InvalidKernelId`],
//!    [`NftError::InvalidLevel`] und [`NftError::InvalidChainSuffix`]. Sie
//!    entstehen, bevor irgendein Byte kodiert wird.
//! 2. **Kodier-Invarianten** ([`NftError::Encoding`]): ein Attribut sprengt
//!    sein 16-Bit-Längenfeld, der Batch sprengt [`crate::MAX_BATCH_LEN`] oder
//!    ein Zeichenkettenattribut enthält ein NUL. Für geprüfte Eingaben ist das
//!    unerreichbar (die Tests belegen die Obergrenzen); der Kodierer meldet es
//!    trotzdem typisiert statt zu paniken.
//! 3. **Transport** (Socket-Aufrufe): [`NftError::Unsupported`],
//!    [`NftError::Socket`] (mit [`SocketOp`] und der Fehlernummer),
//!    [`NftError::PartialSend`].
//! 4. **Kernel-Antwort**: [`NftError::PermissionDenied`] (`EPERM` gegen
//!    `BATCH_BEGIN`), [`NftError::Unsupported`] (`EOPNOTSUPP` gegen
//!    `BATCH_BEGIN`), [`NftError::NotFound`] (`ENOENT`),
//!    [`NftError::AlreadyExists`] (`EEXIST`), [`NftError::NoBufferSpace`]
//!    (`ENOBUFS`), [`NftError::Rejected`] (jede andere Fehlernummer, auch
//!    `EPERM`/`EOPNOTSUPP` gegen eine Objektnachricht),
//!    [`NftError::NoReply`] (Antworten fehlen) und
//!    [`NftError::MalformedReply`] (Antwort nicht wohlgeformt). Welche
//!    Nachricht der Fehler trifft, entscheidet die Deutung (Modul `reply`,
//!    „Fehlernummern“).
//!
//! # Inhaltsfrei
//! Keine Variante trägt die Kernel-ID, die Ebene, den Kettennamen, Bytes aus
//! dem Batch oder Bytes aus einer Kernel-Antwort. Gründe sind feste
//! `&'static str`, Fehlernummern sind Zahlen. Der Warden loggt Fehler auf
//! `error`-Stufe; cgroup-Daten gehören dort nicht hin.
//!
//! `Display`, `Debug` und `std::error::Error` sind von Hand geschrieben (kein
//! `harw-macros`, kein `thiserror`), damit die Crate ohne interne
//! Abhängigkeit auskommt (kleinste TCB-Fläche im Warden).
//!
//! # Abbildung auf `std::io`
//! [`NftError::kind`] ordnet jede Variante einer [`std::io::ErrorKind`] zu;
//! `From<NftError> for std::io::Error` nutzt das. So bildet `harw-warden`
//! einen Fehler ohne eigene Tabelle auf `WardenError::Io` ab.
//!
//! # Exportierte Typen
//! [`NftError`], [`SocketOp`] und der Alias [`NftResult`].
//!
//! # Nebenläufigkeit
//! Reine `Copy`-Daten ohne innere Veränderlichkeit: `Send + Sync`.
//!
//! # Examples
//! ```rust
//! use harw_dod_nft::{CgroupRule, NftError};
//!
//! let err = CgroupRule::new(0, 2, "6a6f62").err();
//! assert_eq!(err, Some(NftError::InvalidKernelId));
//! assert_eq!(
//!     NftError::InvalidKernelId.to_string(),
//!     "cgroup kernel id must not be zero"
//! );
//! assert_eq!(
//!     std::io::Error::from(NftError::PermissionDenied).kind(),
//!     std::io::ErrorKind::PermissionDenied
//! );
//! ```

use std::fmt;
use std::io;

use crate::uapi::{EACCES, EAFNOSUPPORT, ENOBUFS, EPERM, EPROTONOSUPPORT};

/// Der Socket-Aufruf, der scheiterte (für [`NftError::Socket`]).
///
/// # Description
/// Benennt den Schritt, nie Adressen oder Daten.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SocketOp {
    /// `socket(AF_NETLINK, SOCK_RAW | SOCK_CLOEXEC, NETLINK_NETFILTER)`.
    Open,
    /// `bind` auf die vom Kernel vergebene Port-ID.
    Bind,
    /// `setsockopt(SO_RCVTIMEO)`.
    SetTimeout,
    /// `sendto` des Batches an den Kernel.
    Send,
    /// `recvfrom` einer Antwort.
    Receive,
}

impl fmt::Display for SocketOp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Open => "open",
            Self::Bind => "bind",
            Self::SetTimeout => "timeout setup",
            Self::Send => "send",
            Self::Receive => "receive",
        })
    }
}

/// Fehler dieser Crate.
///
/// # Description
/// Siehe Moduldoku: Eingabeprüfung, Kodier-Invarianten, Transport und
/// Kernel-Antwort, alle inhaltsfrei.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NftError {
    /// Die Kernel-ID der cgroup ist `0`. Keine cgroup hat diese ID; eine
    /// Regel dafür träfe nie und wäre eine stille Nicht-Isolation.
    InvalidKernelId,

    /// Die cgroup-Ebene liegt außerhalb von `1..=`[`crate::MAX_SOCKET_LEVEL`].
    /// Ebene `0` ist die Wurzel-cgroup — eine Regel dafür nähme dem ganzen
    /// Host das Netz; der Kernel nähme sie an. Oberhalb von 255 lehnt der
    /// Kernel die Regel mit `ERANGE` ab.
    InvalidLevel,

    /// Der Kettensuffix ist kein zulässiger Kleinbuchstaben-Hex-Stamm (leer,
    /// ungerade Länge, Zeichen außerhalb `0-9a-f` oder länger als
    /// [`crate::MAX_CHAIN_SUFFIX_LEN`]).
    InvalidChainSuffix {
        /// Fester Grund, z. B. `"not lowercase hex"`; nie der Suffix selbst.
        reason: &'static str,
    },

    /// Eine Invariante des Kodierers ist verletzt (Attribut- oder
    /// Nachrichtenlänge, Batch-Obergrenze, NUL in einer Zeichenkette).
    /// Für geprüfte Eingaben unerreichbar.
    Encoding {
        /// Fester Grund, z. B. `"batch exceeds MAX_BATCH_LEN"`.
        reason: &'static str,
    },

    /// nf_tables ist hier nicht verfügbar: kein Linux, oder der Kernel
    /// antwortet `EOPNOTSUPP` gegen `BATCH_BEGIN` (Subsystem nf_tables
    /// fehlt). `EOPNOTSUPP` gegen eine Objektnachricht ist
    /// [`Self::Rejected`].
    Unsupported,

    /// Ein Socket-Aufruf scheiterte.
    Socket {
        /// Der gescheiterte Schritt.
        op: SocketOp,
        /// Positive Fehlernummer (`errno`) des Aufrufs.
        errno: i32,
    },

    /// `sendto` nahm nicht den ganzen Batch an. Bei Datagramm-Sockets
    /// unerreichbar; gemeldet statt still einen halben Batch anzunehmen.
    PartialSend,

    /// Der Kernel verweigert den ganzen Batch (`EPERM` gegen
    /// `BATCH_BEGIN`): dem sendenden Thread fehlt `CAP_NET_ADMIN` in der
    /// wirksamen Menge (bezogen auf den Netzwerk-Namespace des Sockets). `EPERM` gegen eine Objektnachricht heißt etwas anderes (z. B.
    /// gehört die Tabelle mit `NFT_TABLE_F_OWNER` einem anderen Socket) und
    /// ist [`Self::Rejected`].
    PermissionDenied,

    /// Der Kernel meldet `ENOENT` — z. B. fehlt ihm ein Ausdruckstyp
    /// (`socket`, Modul `nft_socket`). Beim Aufheben gilt `ENOENT` nur dann
    /// als „schon fort“ (kein Fehler), wenn **jede** Löschnachricht es
    /// meldet und sonst kein Fehler kam; trifft es nur einen Teil, hat der
    /// Kernel den ganzen Batch verworfen, und es bleibt dieser Fehler.
    NotFound,

    /// Der Kernel meldet `EEXIST`. Ohne `NLM_F_EXCL` unerwartet.
    AlreadyExists,

    /// Dem Kernel fehlte Netlink-Pufferplatz (`ENOBUFS`), als Antwort oder
    /// beim Empfang (Antworten verworfen). Der Batch wirkt ganz oder gar
    /// nicht; jede Operation ist idempotent und darf wiederholt werden.
    NoBufferSpace,

    /// Der Kernel lehnt den Batch mit einer anderen Fehlernummer ab
    /// (z. B. `ERANGE` = 34), oder mit `EPERM` (1) bzw. `EOPNOTSUPP` (95)
    /// gegen eine Objektnachricht — etwa weil `inet harw_warden` mit
    /// `NFT_TABLE_F_OWNER` einem anderen Netlink-Socket gehört (`EPERM`,
    /// Probe mit Kernel 6.18).
    Rejected {
        /// Positive Fehlernummer aus `struct nlmsgerr`.
        errno: i32,
    },

    /// Nicht jede nötige Antwort kam innerhalb der Frist, und keine trug
    /// einen Fehler: eine Antwort auf eine Objektnachricht fehlt, oder der
    /// Kernel zeigt Batch-Bestätigungen, hat `BATCH_END` aber nicht bestätigt
    /// (Batch ohne Meldung verworfen, Modul `reply`, Regel 4). Ob der Batch
    /// wirkt, ist dann unbekannt bzw. verneint; als Fehler behandelt, nie
    /// als Erfolg. Jede Operation ist idempotent und darf wiederholt werden.
    NoReply,

    /// Eine Antwort des Kernels ist nicht wohlgeformt oder passt nicht zum
    /// gesendeten Batch.
    MalformedReply {
        /// Fester Grund, z. B. `"error reply shorter than struct nlmsgerr"`.
        reason: &'static str,
    },
}

impl NftError {
    /// Die passende [`io::ErrorKind`].
    ///
    /// # Description
    /// - Eingabefehler → `InvalidInput`; Kodier-Invariante → `Other`.
    /// - [`Self::Unsupported`] → `Unsupported`.
    /// - [`Self::Socket`]: `EPERM`/`EACCES` (z. B. seccomp) →
    ///   `PermissionDenied`, `EAFNOSUPPORT`/`EPROTONOSUPPORT` →
    ///   `Unsupported`, `ENOBUFS` → `OutOfMemory`, sonst `Other`.
    /// - [`Self::PermissionDenied`] → `PermissionDenied`,
    ///   [`Self::NotFound`] → `NotFound`, [`Self::AlreadyExists`] →
    ///   `AlreadyExists`, [`Self::NoBufferSpace`] → `OutOfMemory`,
    ///   [`Self::NoReply`] → `TimedOut`, [`Self::MalformedReply`] →
    ///   `InvalidData`, [`Self::PartialSend`] und [`Self::Rejected`] →
    ///   `Other`.
    ///
    /// # Returns
    /// Die Art.
    #[must_use]
    pub fn kind(&self) -> io::ErrorKind {
        match self {
            Self::InvalidKernelId | Self::InvalidLevel | Self::InvalidChainSuffix { .. } => {
                io::ErrorKind::InvalidInput
            }
            Self::Unsupported => io::ErrorKind::Unsupported,
            Self::Socket { errno, .. } => socket_errno_kind(*errno),
            Self::PermissionDenied => io::ErrorKind::PermissionDenied,
            Self::NotFound => io::ErrorKind::NotFound,
            Self::AlreadyExists => io::ErrorKind::AlreadyExists,
            Self::NoBufferSpace => io::ErrorKind::OutOfMemory,
            Self::NoReply => io::ErrorKind::TimedOut,
            Self::MalformedReply { .. } => io::ErrorKind::InvalidData,
            Self::Encoding { .. } | Self::PartialSend | Self::Rejected { .. } => {
                io::ErrorKind::Other
            }
        }
    }
}

// Art eines Socket-Fehlers nach seiner Fehlernummer (Linux-Nummerierung).
fn socket_errno_kind(errno: i32) -> io::ErrorKind {
    match errno {
        EPERM | EACCES => io::ErrorKind::PermissionDenied,
        EAFNOSUPPORT | EPROTONOSUPPORT => io::ErrorKind::Unsupported,
        ENOBUFS => io::ErrorKind::OutOfMemory,
        _ => io::ErrorKind::Other,
    }
}

impl fmt::Display for NftError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidKernelId => f.write_str("cgroup kernel id must not be zero"),
            Self::InvalidLevel => f.write_str("cgroup level must be between 1 and 255"),
            Self::InvalidChainSuffix { reason } => write!(f, "chain suffix is invalid: {reason}"),
            Self::Encoding { reason } => write!(f, "nf_tables batch encoding failed: {reason}"),
            Self::Unsupported => f.write_str("nf_tables is not available on this system"),
            Self::Socket { op, errno } => {
                write!(f, "netlink socket {op} failed with errno {errno}")
            }
            Self::PartialSend => f.write_str("netlink socket accepted only part of the batch"),
            Self::PermissionDenied => f.write_str(
                "the kernel refused the ruleset change: insufficient privileges (CAP_NET_ADMIN required)",
            ),
            Self::NotFound => f.write_str(
                "the kernel could not find a referenced object or expression type (ENOENT)",
            ),
            Self::AlreadyExists => f.write_str("the kernel reported an existing object (EEXIST)"),
            Self::NoBufferSpace => f.write_str(
                "the kernel ran out of netlink buffer space (ENOBUFS); the operation can be retried",
            ),
            Self::Rejected { errno } => {
                write!(f, "the kernel rejected the ruleset change with errno {errno}")
            }
            Self::NoReply => {
                f.write_str("the kernel did not answer every message of the batch in time")
            }
            Self::MalformedReply { reason } => write!(f, "malformed netlink reply: {reason}"),
        }
    }
}

impl std::error::Error for NftError {}

impl From<NftError> for io::Error {
    /// Baut einen `io::Error` mit [`NftError::kind`] und der inhaltsfreien
    /// Meldung als Quelle.
    fn from(error: NftError) -> Self {
        Self::new(error.kind(), error)
    }
}

/// Kurzform für `Result<T, NftError>`.
pub type NftResult<T> = Result<T, NftError>;

#[cfg(test)]
mod tests {
    use std::io;

    use super::{NftError, SocketOp};
    use crate::test_support::TestResult;

    #[test]
    fn test_display_is_exact_and_content_free() -> TestResult {
        let cases = [
            (
                NftError::InvalidKernelId,
                "cgroup kernel id must not be zero",
            ),
            (
                NftError::InvalidLevel,
                "cgroup level must be between 1 and 255",
            ),
            (
                NftError::InvalidChainSuffix {
                    reason: "not lowercase hex",
                },
                "chain suffix is invalid: not lowercase hex",
            ),
            (
                NftError::Encoding {
                    reason: "batch exceeds MAX_BATCH_LEN",
                },
                "nf_tables batch encoding failed: batch exceeds MAX_BATCH_LEN",
            ),
            (
                NftError::Unsupported,
                "nf_tables is not available on this system",
            ),
            (
                NftError::Socket {
                    op: SocketOp::Bind,
                    errno: 98,
                },
                "netlink socket bind failed with errno 98",
            ),
            (
                NftError::PartialSend,
                "netlink socket accepted only part of the batch",
            ),
            (
                NftError::PermissionDenied,
                "the kernel refused the ruleset change: insufficient privileges (CAP_NET_ADMIN required)",
            ),
            (
                NftError::NotFound,
                "the kernel could not find a referenced object or expression type (ENOENT)",
            ),
            (
                NftError::AlreadyExists,
                "the kernel reported an existing object (EEXIST)",
            ),
            (
                NftError::NoBufferSpace,
                "the kernel ran out of netlink buffer space (ENOBUFS); the operation can be retried",
            ),
            (
                NftError::Rejected { errno: 34 },
                "the kernel rejected the ruleset change with errno 34",
            ),
            (
                NftError::NoReply,
                "the kernel did not answer every message of the batch in time",
            ),
            (
                NftError::MalformedReply {
                    reason: "duplicate reply",
                },
                "malformed netlink reply: duplicate reply",
            ),
        ];
        for (error, expected) in cases {
            assert_eq!(error.to_string(), expected);
            assert!(std::error::Error::source(&error).is_none());
        }
        Ok(())
    }

    #[test]
    fn test_socket_op_names() -> TestResult {
        let names: Vec<String> = [
            SocketOp::Open,
            SocketOp::Bind,
            SocketOp::SetTimeout,
            SocketOp::Send,
            SocketOp::Receive,
        ]
        .iter()
        .map(ToString::to_string)
        .collect();
        assert_eq!(names, ["open", "bind", "timeout setup", "send", "receive"]);
        Ok(())
    }

    #[test]
    fn test_error_kinds() -> TestResult {
        let socket = |errno| NftError::Socket {
            op: SocketOp::Open,
            errno,
        };
        let cases = [
            (NftError::InvalidKernelId, io::ErrorKind::InvalidInput),
            (NftError::InvalidLevel, io::ErrorKind::InvalidInput),
            (
                NftError::InvalidChainSuffix { reason: "empty" },
                io::ErrorKind::InvalidInput,
            ),
            (NftError::Encoding { reason: "x" }, io::ErrorKind::Other),
            (NftError::Unsupported, io::ErrorKind::Unsupported),
            (socket(1), io::ErrorKind::PermissionDenied),
            (socket(13), io::ErrorKind::PermissionDenied),
            (socket(93), io::ErrorKind::Unsupported),
            (socket(97), io::ErrorKind::Unsupported),
            (socket(105), io::ErrorKind::OutOfMemory),
            (socket(98), io::ErrorKind::Other),
            (NftError::PartialSend, io::ErrorKind::Other),
            (NftError::PermissionDenied, io::ErrorKind::PermissionDenied),
            (NftError::NotFound, io::ErrorKind::NotFound),
            (NftError::AlreadyExists, io::ErrorKind::AlreadyExists),
            (NftError::NoBufferSpace, io::ErrorKind::OutOfMemory),
            (NftError::Rejected { errno: 34 }, io::ErrorKind::Other),
            (NftError::NoReply, io::ErrorKind::TimedOut),
            (
                NftError::MalformedReply { reason: "x" },
                io::ErrorKind::InvalidData,
            ),
        ];
        for (error, kind) in cases {
            assert_eq!(error.kind(), kind, "{error}");
            let io_error = io::Error::from(error);
            assert_eq!(io_error.kind(), kind);
            assert_eq!(io_error.to_string(), error.to_string());
        }
        Ok(())
    }

    #[test]
    fn test_error_is_send_sync_and_copy() -> TestResult {
        fn assert_traits<T: Send + Sync + Copy + 'static>() {}
        assert_traits::<NftError>();
        assert_traits::<SocketOp>();
        Ok(())
    }
}
