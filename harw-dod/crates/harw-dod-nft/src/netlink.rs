//! Die API für `harw-warden`: [`NftNetlink`] isoliert cgroups und hebt
//! Isolationen auf, im eigenen Prozess über `NETLINK_NETFILTER`.
//!
//! # Verantwortungsbereich
//! [`NftNetlink`] hält einen geöffneten Socket und einen Sequenzzähler
//! hinter einem `Mutex` und bietet die vier Operationen des Wardens:
//! [`NftNetlink::isolate`], [`NftNetlink::release`],
//! [`NftNetlink::release_chains`] (ohne Kernel-ID) und
//! [`NftNetlink::release_all`]. Jede Operation ist **ein** Batch, also eine
//! nftables-Transaktion: ganz oder gar nicht, idempotent (Modul `batch`).
//! Erfolg heißt: der Kernel hat jede Objektnachricht des Batches einzeln
//! fehlerfrei bestätigt und — auf Kerneln, die Batch-Nachrichten bestätigen
//! — auch die Übernahme (`BATCH_END`, Modul `reply`).
//!
//! # Sandbox
//! [`NETLINK_SYSCALLS`] nennt die Systemaufrufe: zum Öffnen `socket` (nur
//! `AF_NETLINK`), `bind`, `setsockopt`; je Operation `sendto`, `recvfrom`;
//! zum Schließen `close`. Kein `execve`, keine Pfade (Landlock braucht
//! nichts), kein Kindprozess. Wird [`NftNetlink::open`] vor Landlock und
//! seccomp aufgerufen, braucht der laufende Warden nur noch `sendto`,
//! `recvfrom` und `close`.
//!
//! # Rechte
//! `CAP_NET_ADMIN` in der **wirksamen** Menge des sendenden Threads, bei
//! jeder Operation (keine Ambient-Menge, kein Kindprozess); der Socket und
//! damit jede Regel gehören zum Netzwerk-Namespace, in dem
//! [`NftNetlink::open`] lief. Ohne die Capability scheitert jede Operation
//! mit [`crate::NftError::PermissionDenied`]; das Öffnen gelingt trotzdem.
//! Belege und Folgen für den Warden: [`NftNetlink`], „Rechte“.
//!
//! # Plattformen
//! Nur Linux. Anderswo scheitert [`NftNetlink::open`] mit
//! [`crate::NftError::Unsupported`], und es kann keine Instanz geben.
//!
//! # Nebenläufigkeit
//! `Send + Sync`. Der `Mutex` serialisiert die Operationen auf dem einen
//! Socket, damit sich Antworten nicht vermischen; der Kernel serialisiert
//! nftables-Transaktionen ohnehin. Ein vergifteter `Mutex` wird übernommen:
//! der Zustand ist nur Socket und Zähler, und jede Operation räumt vor dem
//! Senden verspätete Antworten ab.
//!
//! # Examples
//! ```rust,no_run
//! use harw_dod_nft::{CgroupRule, NftNetlink};
//!
//! let nft = NftNetlink::open()?;
//! // Kernel-ID = Inode der cgroup, Ebene = Tiefe unter der cgroup2-Wurzel.
//! let rule = CgroupRule::new(4242, 2, "6a6f622d31")?;
//! nft.isolate(&rule)?;
//! nft.release(&rule)?;
//! nft.release_chains("6a6f622d31")?;
//! nft.release_all()?;
//! # Ok::<(), harw_dod_nft::NftError>(())
//! ```

use std::fmt;
use std::sync::{Mutex, PoisonError};
use std::time::Duration;

use crate::error::NftResult;
use crate::rule::CgroupRule;
use crate::transport::{Operation, Session};

#[cfg(not(target_os = "linux"))]
use crate::error::NftError;
#[cfg(not(target_os = "linux"))]
use crate::transport::{Received, RecvMode, Transport};

/// Längstes Warten auf eine fehlende Antwort des Kernels.
///
/// # Description
/// Der Kernel beantwortet einen Batch synchron im `sendto`; die Frist greift
/// nur, wenn eine Antwort fehlt (dann [`NftError::NoReply`]). Kurz genug,
/// dass der Warden seine Eingriffssperre nicht lange hält.
///
/// [`NftError::NoReply`]: crate::NftError::NoReply
pub const REPLY_TIMEOUT: Duration = Duration::from_secs(2);

/// Sequenznummer der ersten Operation eines Sockets.
// Ohne Linux gibt es keinen Socket und damit keine erste Operation.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
const FIRST_SEQ: u32 = 1;

/// Die Systemaufrufe von [`NftNetlink`] für die seccomp-Allowlist des
/// Wardens (Namen; die Nummern ordnet der Warden über `libc::SYS_*` zu).
///
/// # Description
/// Unterteilt nach Zeitpunkt, damit ein Warden, der den Socket vor seccomp
/// öffnet, nur [`Self::per_operation`] und [`Self::on_drop`] freigeben
/// muss. Kein `execve`, kein Pfad.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NetlinkSyscalls {
    /// [`NftNetlink::open`]: `socket` (Argument 0 `AF_NETLINK`, Argument 2
    /// `NETLINK_NETFILTER` = 12), `bind`, `setsockopt` (`SOL_SOCKET`,
    /// `SO_RCVTIMEO_NEW`, Rückfall `SO_RCVTIMEO_OLD`).
    pub open: &'static [&'static str],
    /// Jede Operation: `sendto` (an Port-ID 0), `recvfrom`.
    pub per_operation: &'static [&'static str],
    /// Fallenlassen von [`NftNetlink`]: `close`.
    pub on_drop: &'static [&'static str],
}

impl NetlinkSyscalls {
    /// Alle Namen, in der Reihenfolge Öffnen, Betrieb, Schließen.
    ///
    /// # Returns
    /// Einen Iterator über die Namen.
    pub fn all(self) -> impl Iterator<Item = &'static str> {
        let Self {
            open,
            per_operation,
            on_drop,
        } = self;
        open.iter().chain(per_operation).chain(on_drop).copied()
    }
}

/// Die Systemaufrufe von [`NftNetlink`] (siehe [`NetlinkSyscalls`]).
pub const NETLINK_SYSCALLS: NetlinkSyscalls = NetlinkSyscalls {
    open: &["socket", "bind", "setsockopt"],
    per_operation: &["sendto", "recvfrom"],
    on_drop: &["close"],
};

// Der Transport dieser Plattform.
#[cfg(target_os = "linux")]
type PlatformSocket = crate::socket::NetlinkSocket;
#[cfg(not(target_os = "linux"))]
type PlatformSocket = NoSocket;

/// Ohne Linux gibt es keinen Socket: ein leerer Typ, von dem es keinen Wert
/// geben kann.
#[cfg(not(target_os = "linux"))]
#[derive(Debug)]
enum NoSocket {}

#[cfg(not(target_os = "linux"))]
impl Transport for NoSocket {
    fn send(&mut self, _datagram: &[u8]) -> NftResult<()> {
        match *self {}
    }

    fn recv(&mut self, _buf: &mut [u8], _mode: RecvMode) -> NftResult<Received> {
        match *self {}
    }
}

/// nftables im eigenen Prozess: ein `NETLINK_NETFILTER`-Socket für die
/// Isolationsregeln des Wardens.
///
/// # Description
/// Siehe Moduldoku. Jede Methode sendet genau einen Batch und wartet auf
/// die Antworten, die der Erfolg verlangt (Modul `reply`).
///
/// # Rechte
/// `nfnetlink_rcv` prüft bei **jeder** Operation `CAP_NET_ADMIN` über
/// `netlink_net_capable` (`net/netfilter/nfnetlink.c` v6.18 `:661-664`):
/// - Der **sendende Thread** braucht die Capability in seiner wirksamen
///   Menge, bezogen auf den User-Namespace, dem der Netzwerk-Namespace des
///   Sockets gehört (`ns_capable`, `net/netlink/af_netlink.c` v6.18
///   `:849-855`, `:900-903`). Keine Ambient-Menge, kein Kindprozess.
/// - Die Rechte dessen, der den Socket **geöffnet** hat
///   (`file_ns_capable`), prüft der Kernel nur bei Nachrichten ohne
///   ausdrückliche Zieladresse. Diese Crate sendet immer mit `sendto` an die
///   Adresse des Kernels; das setzt `NETLINK_SKB_DST` (`af_netlink.c` v6.18
///   `:1838-1850`) und überspringt diese Prüfung (`:852-854`).
///
/// Für den Warden folgt daraus: [`Self::open`] erst aufrufen, wenn
/// `CAP_NET_ADMIN` wirksam ist, und die Capability danach nie abgeben —
/// dann hängt nichts an der Einzelheit mit der Zieladresse.
///
/// **Netzwerk-Namespace:** Der Socket gehört zum Netzwerk-Namespace des
/// Threads, der [`Self::open`] aufruft (`sock_net`), und jede Regel landet
/// dort. Der Warden muss den Socket deshalb im Netzwerk-Namespace des Hosts
/// öffnen: kein `PrivateNetwork=` in der Unit, kein eigener
/// Netzwerk-Namespace davor.
///
/// # Concurrency
/// `Send + Sync`; Operationen laufen nacheinander.
pub struct NftNetlink {
    session: Mutex<Session<PlatformSocket>>,
}

impl fmt::Debug for NftNetlink {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("NftNetlink").finish_non_exhaustive()
    }
}

impl NftNetlink {
    /// Öffnet und bindet den `NETLINK_NETFILTER`-Socket.
    ///
    /// # Description
    /// Öffnet den Socket im Netzwerk-Namespace des aufrufenden Threads — dort
    /// landen alle Regeln. Öffnen und Binden selbst brauchen keine
    /// Capability; `CAP_NET_ADMIN` prüft der Kernel bei jeder Operation am
    /// sendenden Thread (siehe [`NftNetlink`], „Rechte“). Der Warden ruft
    /// `open` trotzdem erst auf, wenn die Capability wirksam ist, im
    /// Netzwerk-Namespace des Hosts, und gibt sie danach nicht mehr ab.
    /// Setzt die Empfangsfrist [`REPLY_TIMEOUT`].
    ///
    /// # Returns
    /// Die geöffnete Instanz.
    ///
    /// # Errors
    /// - [`NftError::Socket`](crate::NftError::Socket): `socket`, `bind`
    ///   oder `setsockopt` scheiterte (z. B. seccomp, Kernel ohne
    ///   `NETLINK_NETFILTER`).
    /// - [`NftError::Unsupported`](crate::NftError::Unsupported): kein
    ///   Linux.
    pub fn open() -> NftResult<Self> {
        open_session().map(|session| Self {
            session: Mutex::new(session),
        })
    }

    /// Trennt die cgroup vom Netz (ein- und ausgehend).
    ///
    /// # Description
    /// Ein Batch: Tabelle und beide Basisketten anlegen (falls sie fehlen),
    /// leeren, je eine Drop-Regel für die cgroup einfügen. Idempotent.
    ///
    /// # Arguments
    /// - `target` (`&CgroupRule`): Kernel-ID, Ebene und Kettensuffix.
    ///
    /// # Returns
    /// `Ok(())`, sobald der Kernel den Batch bestätigt hat (siehe
    /// Moduldoku) — die Regeln sind dann aktiv.
    ///
    /// # Errors
    /// [`NftError::PermissionDenied`](crate::NftError::PermissionDenied),
    /// [`NftError::NotFound`](crate::NftError::NotFound) (z. B. Kernel ohne
    /// `nft_socket`), [`NftError::NoReply`](crate::NftError::NoReply),
    /// [`NftError::NoBufferSpace`](crate::NftError::NoBufferSpace),
    /// [`NftError::Rejected`](crate::NftError::Rejected),
    /// [`NftError::MalformedReply`](crate::NftError::MalformedReply) und
    /// Transportfehler. Nie ein stiller Erfolg.
    pub fn isolate(&self, target: &CgroupRule) -> NftResult<()> {
        self.apply(Operation::Isolate(target))
    }

    /// Hebt die Isolation der cgroup auf (entfernt ihre beiden Ketten).
    ///
    /// # Description
    /// Wie [`Self::release_chains`] mit `target.chain_suffix()`; Kernel-ID
    /// und Ebene zählen hier nicht.
    ///
    /// # Arguments
    /// - `target` (`&CgroupRule`): das Ziel.
    ///
    /// # Returns
    /// `Ok(())`, auch wenn die cgroup nicht isoliert war.
    ///
    /// # Errors
    /// Wie [`Self::isolate`].
    pub fn release(&self, target: &CgroupRule) -> NftResult<()> {
        self.apply(Operation::Release(target.chain_suffix()))
    }

    /// Hebt die Isolation über den Kettensuffix auf, ohne Kernel-ID (die
    /// cgroup kann schon fort sein).
    ///
    /// # Description
    /// Ein Batch: Tabelle und Ketten anlegen (falls sie fehlen), leeren,
    /// löschen. Idempotent; fremde Ketten bleiben unberührt. Meldet der
    /// Kernel `ENOENT` auf **jede** Löschnachricht und keinen anderen
    /// Fehler, gilt die Isolation als schon aufgehoben. `ENOENT` auf nur
    /// einem Teil ist ein Fehler: der Kernel hat den ganzen Batch verworfen,
    /// die übrigen Ketten stehen noch.
    ///
    /// # Arguments
    /// - `chain_suffix` (`&str`): Kleinbuchstaben-Hex-Stamm der Ketten
    ///   (`o_<suffix>`, `i_<suffix>`).
    ///
    /// # Returns
    /// `Ok(())`, auch wenn keine Ketten da waren.
    ///
    /// # Errors
    /// [`NftError::InvalidChainSuffix`](crate::NftError::InvalidChainSuffix)
    /// vor jedem Senden, sonst wie [`Self::isolate`].
    pub fn release_chains(&self, chain_suffix: &str) -> NftResult<()> {
        self.apply(Operation::Release(chain_suffix))
    }

    /// Entfernt die ganze eigene Tabelle und damit jede Isolation.
    ///
    /// # Returns
    /// `Ok(())`, auch wenn es die Tabelle nicht gab.
    ///
    /// # Errors
    /// Wie [`Self::isolate`].
    pub fn release_all(&self) -> NftResult<()> {
        self.apply(Operation::ReleaseAll)
    }

    // Eine Operation unter dem Lock; das Ergebnis im Detail (z. B. ob
    // `BATCH_END` bestätigt wurde) braucht der Aufrufer nicht.
    fn apply(&self, operation: Operation<'_>) -> NftResult<()> {
        let mut session = self.session.lock().unwrap_or_else(PoisonError::into_inner);
        session.apply(operation).map(|_outcome| ())
    }
}

// Öffnet den Socket dieser Plattform.
#[cfg(target_os = "linux")]
fn open_session() -> NftResult<Session<PlatformSocket>> {
    let socket = crate::socket::NetlinkSocket::open(REPLY_TIMEOUT)?;
    Ok(Session::new(socket, FIRST_SEQ))
}

// Ohne Linux gibt es nichts zu öffnen.
#[cfg(not(target_os = "linux"))]
fn open_session() -> NftResult<Session<PlatformSocket>> {
    Err(NftError::Unsupported)
}

#[cfg(test)]
mod tests {
    use super::{NETLINK_SYSCALLS, NftNetlink, REPLY_TIMEOUT};
    use crate::test_support::TestResult;

    #[test]
    fn test_nft_netlink_is_send_and_sync() -> TestResult {
        fn assert_traits<T: Send + Sync>() {}
        assert_traits::<NftNetlink>();
        Ok(())
    }

    #[test]
    fn test_syscalls_are_only_the_netlink_socket_calls() -> TestResult {
        assert_eq!(NETLINK_SYSCALLS.open, ["socket", "bind", "setsockopt"]);
        assert_eq!(NETLINK_SYSCALLS.per_operation, ["sendto", "recvfrom"]);
        assert_eq!(NETLINK_SYSCALLS.on_drop, ["close"]);
        let all: Vec<&str> = NETLINK_SYSCALLS.all().collect();
        assert_eq!(
            all,
            [
                "socket",
                "bind",
                "setsockopt",
                "sendto",
                "recvfrom",
                "close"
            ]
        );
        for forbidden in [
            "execve", "execveat", "fork", "vfork", "clone", "kill", "wait4",
        ] {
            assert!(!all.contains(&forbidden), "{forbidden}");
        }
        Ok(())
    }

    // Short enough not to hold the warden's action lock for long; never zero
    // (rustix refuses a zero `SO_RCVTIMEO` with EINVAL, the kernel would
    // read 0 as "wait forever").
    #[test]
    fn test_reply_timeout_is_two_seconds() -> TestResult {
        assert_eq!(REPLY_TIMEOUT, std::time::Duration::from_secs(2));
        Ok(())
    }

    #[cfg(not(target_os = "linux"))]
    #[test]
    fn test_open_is_unsupported_off_linux() -> TestResult {
        assert_eq!(NftNetlink::open().err(), Some(crate::NftError::Unsupported));
        Ok(())
    }
}
