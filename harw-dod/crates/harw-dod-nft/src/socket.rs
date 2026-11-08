//! Der echte Transport: ein `AF_NETLINK`/`NETLINK_NETFILTER`-Socket über
//! `rustix` (nur Linux).
//!
//! # Verantwortungsbereich
//! [`NetlinkSocket`] ist die einzige Stelle dieser Crate, die mit dem Kernel
//! spricht. Sie öffnet den Socket (`SOCK_RAW | SOCK_CLOEXEC`), bindet ihn an
//! eine vom Kernel vergebene Port-ID ohne Multicast-Gruppe, setzt die
//! Empfangsfrist (`SO_RCVTIMEO`) und setzt dann nur noch `sendto` an den
//! Kernel (Port-ID 0) und `recvfrom` mit `MSG_TRUNC` (und bei
//! [`RecvMode::Poll`] `MSG_DONTWAIT`) ab.
//!
//! # Systemaufrufe (für die seccomp-Allowlist des Wardens)
//! Öffnen: `socket` (`AF_NETLINK`, `SOCK_RAW | SOCK_CLOEXEC`,
//! `NETLINK_NETFILTER` = 12, `linux/netlink.h:21`), `bind`, `setsockopt`
//! (`SOL_SOCKET`, `SO_RCVTIMEO_NEW`; `rustix` fällt bei `ENOPROTOOPT` auf
//! `SO_RCVTIMEO_OLD` zurück). Je Operation: `sendto`, `recvfrom`.
//! Beim Fallenlassen: `close`. `rustix` ruft auf `x86_64` und `aarch64`
//! genau diese Systemaufrufe direkt auf (kein `socketcall`, kein
//! `sendmsg`/`recvmsg`); siehe `crate::NETLINK_SYSCALLS`. Kein Pfad, kein
//! `execve`.
//!
//! # Rechte
//! Öffnen und Binden brauchen keine Capability. Jede Änderung prüft der
//! Kernel beim Senden gegen `CAP_NET_ADMIN` des **sendenden** Threads
//! (Probe: ohne die Capability genau eine Antwort `EPERM` auf
//! `BATCH_BEGIN`). Die Rechte dessen, der den Socket geöffnet hat, prüft er
//! nur bei Nachrichten ohne Zieladresse; `send` nennt deshalb immer
//! [`KERNEL`] als Ziel (`NETLINK_SKB_DST`, `net/netlink/af_netlink.c` v6.18
//! `:1838-1850`, `:852-854`). Der Socket gehört zum Netzwerk-Namespace des
//! öffnenden Threads. Einzelheiten: `crate::NftNetlink`, „Rechte“.
//!
//! # Kein `unsafe`
//! Jeder Aufruf ist eine sichere `rustix`-Funktion; die Signaturen sind aus
//! der Registry-Quelle von `rustix` 1.1.5 nachgelesen (`net/socket.rs`,
//! `net/send_recv/mod.rs`, `net/sockopt.rs`, `net/types.rs`).
//!
//! # Tests
//! Die Unit-Tests hier laufen gegen den echten Kernel, ohne ihn zu ändern:
//! Öffnen, leeres Lesen (mit und ohne Warten) und — nur ohne
//! `CAP_NET_ADMIN` — ein leerer Batch, den `nfnetlink_rcv` mit `EPERM`
//! ablehnt, bevor nf_tables ihn sieht. Mit `CAP_NET_ADMIN` (root) wird der
//! Batch-Test übersprungen, damit ein normaler Testlauf nie das nf_tables
//! des eigenen Netzwerk-Namespace erreicht (und kein Modul nachlädt).
//! Bietet die Umgebung keinen `NETLINK_NETFILTER`-Socket (gVisor, Container
//! ohne nfnetlink, seccomp-Filter auf die Socket-Familie), werden beide
//! übersprungen. Sie prüfen Absender, `MSG_TRUNC`-Länge und die Zerlegung
//! echter Antworten. Der Test mit Regeln und Datenverkehr ist
//! `tests/kernel.rs` (root, `#[ignore]`, eigener Netzwerk-Namespace).
//!
//! # Nebenläufigkeit
//! Hält nur einen `OwnedFd`; `Send`. Gemeinsame Nutzung serialisiert
//! `crate::NftNetlink`.

use std::time::Duration;

use rustix::fd::OwnedFd;
use rustix::io::Errno;
use rustix::net::netlink::{self, SocketAddrNetlink};
use rustix::net::sockopt::{self, Timeout};
use rustix::net::{self, AddressFamily, RecvFlags, SendFlags, SocketFlags, SocketType};

use crate::error::{NftError, NftResult, SocketOp};
use crate::transport::{KERNEL_PORT_ID, Received, RecvMode, Transport};

/// Adresse des Kernels: Port-ID 0, keine Multicast-Gruppe.
///
/// # Description
/// Jedes `sendto` nennt sie ausdrücklich (nie `connect` und `send`): nur
/// dann prüft der Kernel `CAP_NET_ADMIN` allein am sendenden Thread und
/// nicht zusätzlich an dem, der den Socket geöffnet hat (siehe Moduldoku,
/// „Rechte“).
const KERNEL: SocketAddrNetlink = SocketAddrNetlink::new(KERNEL_PORT_ID, 0);

/// Eigene Adresse beim Binden: Port-ID 0 = „der Kernel vergibt eine“,
/// keine Multicast-Gruppe (keine ungefragten Nachrichten).
const AUTOBIND: SocketAddrNetlink = SocketAddrNetlink::new(0, 0);

/// Ein gebundener `NETLINK_NETFILTER`-Socket.
#[derive(Debug)]
pub(crate) struct NetlinkSocket {
    fd: OwnedFd,
}

// Fehler eines Socket-Aufrufs mit seiner Fehlernummer.
fn socket_error(op: SocketOp) -> impl FnOnce(Errno) -> NftError {
    move |errno| NftError::Socket {
        op,
        errno: errno.raw_os_error(),
    }
}

impl NetlinkSocket {
    /// Öffnet und bindet den Socket und setzt die Empfangsfrist.
    ///
    /// # Arguments
    /// - `reply_timeout` (`Duration`): längstes Warten auf eine Antwort
    ///   (nicht null).
    ///
    /// # Errors
    /// [`NftError::Socket`] mit [`SocketOp::Open`], [`SocketOp::Bind`] oder
    /// [`SocketOp::SetTimeout`].
    pub(crate) fn open(reply_timeout: Duration) -> NftResult<Self> {
        let fd = net::socket_with(
            AddressFamily::NETLINK,
            SocketType::RAW,
            SocketFlags::CLOEXEC,
            Some(netlink::NETFILTER),
        )
        .map_err(socket_error(SocketOp::Open))?;
        net::bind(&fd, &AUTOBIND).map_err(socket_error(SocketOp::Bind))?;
        sockopt::set_socket_timeout(&fd, Timeout::Recv, Some(reply_timeout))
            .map_err(socket_error(SocketOp::SetTimeout))?;
        Ok(Self { fd })
    }
}

impl Transport for NetlinkSocket {
    fn send(&mut self, datagram: &[u8]) -> NftResult<()> {
        let sent =
            net::sendto(&self.fd, datagram, SendFlags::empty(), &KERNEL).map_err(|errno| {
                if errno == Errno::NOBUFS {
                    NftError::NoBufferSpace
                } else {
                    socket_error(SocketOp::Send)(errno)
                }
            })?;
        if sent == datagram.len() {
            Ok(())
        } else {
            Err(NftError::PartialSend)
        }
    }

    fn recv(&mut self, buf: &mut [u8], mode: RecvMode) -> NftResult<Received> {
        let flags = match mode {
            RecvMode::Wait => RecvFlags::TRUNC,
            RecvMode::Poll => RecvFlags::TRUNC | RecvFlags::DONTWAIT,
        };
        match net::recvfrom(&self.fd, buf, flags) {
            Ok((len, full_len, sender)) => Ok(Received::Datagram {
                len,
                full_len,
                sender: sender
                    .and_then(|address| SocketAddrNetlink::try_from(address).ok())
                    .map(|address| address.pid()),
            }),
            // Leer (`MSG_DONTWAIT`) oder Frist abgelaufen (`SO_RCVTIMEO`).
            Err(errno) if errno == Errno::AGAIN || errno == Errno::WOULDBLOCK => {
                Ok(Received::Nothing)
            }
            Err(errno) if errno == Errno::INTR => Ok(Received::Interrupted),
            // Der Kernel hat Antworten verworfen (Empfangspuffer voll).
            Err(errno) if errno == Errno::NOBUFS => Err(NftError::NoBufferSpace),
            Err(errno) => Err(socket_error(SocketOp::Receive)(errno)),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::NetlinkSocket;
    use crate::batch::BatchBuilder;
    use crate::error::{NftError, SocketOp};
    use crate::reply::{Intent, ReplyTracker, parse_datagram};
    use crate::test_support::{TestError, TestResult};
    use crate::transport::{KERNEL_PORT_ID, RECV_BUFFER_LEN, Received, RecvMode, Transport};
    use crate::uapi::{EACCES, EAFNOSUPPORT, EPERM, EPROTONOSUPPORT};

    // Short: the kernel answers inside `sendto`; the timeout only bounds the
    // read that finds the socket empty.
    const TIMEOUT: Duration = Duration::from_millis(50);

    // `CAP_NET_ADMIN` (`/usr/include/linux/capability.h:202`).
    const CAP_NET_ADMIN: u32 = 12;

    // Opens the socket, or `None` where this environment offers no
    // NETLINK_NETFILTER socket: gVisor or a container without nfnetlink
    // (EPROTONOSUPPORT, EAFNOSUPPORT), a seccomp or LSM filter on the socket
    // family (EPERM, EACCES). Any other failure fails the test.
    fn open_or_skip() -> TestResult<Option<NetlinkSocket>> {
        match NetlinkSocket::open(TIMEOUT) {
            Ok(socket) => Ok(Some(socket)),
            Err(NftError::Socket {
                op: SocketOp::Open,
                errno,
            }) if [EPROTONOSUPPORT, EAFNOSUPPORT, EPERM, EACCES].contains(&errno) => Ok(None),
            Err(other) => Err(TestError::from(other)),
        }
    }

    // Whether this thread may hold CAP_NET_ADMIN: bit 12 of the hex `CapEff`
    // line in /proc/thread-self/status. Unreadable or unparsable counts as
    // "may hold it", so the caller skips rather than reach nf_tables.
    fn may_hold_net_admin() -> bool {
        let Ok(status) = std::fs::read_to_string("/proc/thread-self/status") else {
            return true;
        };
        let effective = status
            .lines()
            .find_map(|line| line.strip_prefix("CapEff:"))
            .and_then(|hex| u64::from_str_radix(hex.trim(), 16).ok());
        let bit = 1_u64.checked_shl(CAP_NET_ADMIN);
        match (effective, bit) {
            (Some(caps), Some(bit)) => caps & bit != 0,
            _ => true,
        }
    }

    // Opening and binding need no capability. An idle socket has nothing to
    // read, with or without waiting: EAGAIN from MSG_DONTWAIT and from the
    // expired SO_RCVTIMEO both become `Nothing`.
    #[test]
    fn test_real_socket_opens_unprivileged_and_reads_nothing_when_idle() -> TestResult {
        // Skipped (passes) where the environment has no such socket.
        let Some(mut socket) = open_or_skip()? else {
            return Ok(());
        };
        let mut buf = vec![0_u8; RECV_BUFFER_LEN];
        assert_eq!(socket.recv(&mut buf, RecvMode::Poll)?, Received::Nothing);
        assert_eq!(socket.recv(&mut buf, RecvMode::Wait)?, Received::Nothing);
        Ok(())
    }

    // Without CAP_NET_ADMIN, `nfnetlink_rcv` rejects a batch before
    // nf_tables sees it: exactly one EPERM against BEGIN (probe, kernel
    // 6.18; `nfnetlink.c` v6.18 :661-662). The reply must come from the
    // kernel (sender port id 0 through `SocketAddrNetlink`), fit the buffer
    // (MSG_TRUNC length), parse, and decide as PermissionDenied. With the
    // capability (e.g. root) the batch would reach this network namespace's
    // nf_tables, so the test is skipped then; tests/kernel.rs covers that
    // path in a private network namespace.
    #[test]
    fn test_real_kernel_rejects_a_batch_without_cap_net_admin() -> TestResult {
        if may_hold_net_admin() {
            return Ok(());
        }
        let Some(mut socket) = open_or_skip()? else {
            return Ok(());
        };
        let batch = BatchBuilder::begin(7)?.finish()?;
        socket.send(batch.as_bytes())?;
        let mut buf = vec![0_u8; RECV_BUFFER_LEN];
        let mut tracker = ReplyTracker::new(&batch, Intent::Release);
        let mut answers: Vec<(u32, i32)> = Vec::new();
        for _ in 0..8 {
            let (len, full_len, sender) = match socket.recv(&mut buf, RecvMode::Wait)? {
                Received::Nothing => break,
                Received::Interrupted => continue,
                Received::Datagram {
                    len,
                    full_len,
                    sender,
                } => (len, full_len, sender),
            };
            assert_eq!(sender, Some(KERNEL_PORT_ID), "replies come from the kernel");
            assert_eq!(full_len, len, "replies fit the receive buffer");
            let datagram = buf.get(..len).ok_or(TestError::Missing("datagram"))?;
            for ack in parse_datagram(datagram)? {
                answers.push((ack.seq, ack.errno));
                tracker.record(ack)?;
            }
        }
        assert_eq!(answers, [(7, EPERM)]);
        assert!(tracker.is_settled());
        assert_eq!(tracker.decide(), Err(NftError::PermissionDenied));
        Ok(())
    }
}
