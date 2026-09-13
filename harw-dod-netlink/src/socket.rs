//! Bindungsteil: der echte `AUDIT`-Netlink-Socket.
//!
//! # Verantwortungsbereich
//! [`NetlinkAuditSource`] ist die einzige Stelle dieser Crate, die
//! tatsächlich mit dem Kernel spricht. Alles andere in `harw-dod-netlink`
//! (Tokenisierung, [`crate::parse_record`], die Netlink-Rahmenzerlegung in
//! [`crate::frame`]) ist reiner Code auf Bytes und läuft ohne Berechtigung.
//! **Dieses Modul braucht `CAP_AUDIT_READ` (oder root)** — `bind()` auf einen
//! `AUDIT`-Netlink-Socket mit einer Multicast-Lesegruppe scheitert ohne diese
//! Fähigkeit mit `EPERM`.
//!
//! Nur unter `#[cfg(target_os = "linux")]` kompiliert: das Audit-Subsystem
//! und `AF_NETLINK` sind Linux-spezifisch. Auf jeder anderen Plattform bleibt
//! die Crate ohne dieses Modul voll benutzbar (Formungsteil plus
//! [`crate::FixtureAuditSource`]).
//!
//! # Warum `rustix` statt einer Netlink-Bibliothek
//! Diese Crate öffnet einen Socket, bindet ihn auf eine feste Multicast-
//! Gruppe und liest Datagramme — keine Route-Netlink-Attribute, kein
//! `NLA`-Aufbau, kein generisches Netlink. `rustix::net` deckt `socket`,
//! `bind`, `recv` und die `AUDIT`-Protokollkonstante bereits sicher ab (jede
//! hier verwendete Funktion ist ein normaler, sicherer Aufruf — kein
//! `unsafe` auf dieser Seite, obwohl `unsafe_code = "forbid"` im Workspace es
//! ohnehin verbieten würde) und steht bereits als Workspace-Abhängigkeit
//! bereit. Eine vollwertige Netlink-Bibliothek (z. B. `neli`, mit generischem
//! Attribut-Parsing für Route-/Firewall-Netlink) wäre für diesen schmalen
//! Ausschnitt — einen Socket öffnen, Zeilen lesen — unverhältnismäßig.
//!
//! # Kein `unsafe`, keine Ausprobierprobe
//! Diese Implementierung wurde gegen den in der Registry-Quelle von `rustix`
//! nachgelesenen API-Vertrag geschrieben, aber **nie gegen einen echten
//! Kernel ausgeführt** — dieser Knoten darf kein `cargo` ausführen (zentrale,
//! sequenzielle Verifikation). Jeder Fehler beim Öffnen oder Binden des
//! Sockets wird als [`harw_dod_cap::SensorError::SourceUnavailable`]
//! gemeldet, nie als Panic und nie als stiller Nulldurchgang — siehe
//! [`NetlinkAuditSource::open`].
//!
//! # Exportierte Typen
//! [`NetlinkAuditSource`].
//!
//! # Nebenläufigkeit
//! `NetlinkAuditSource` hält nur einen `OwnedFd` (kein internes Locking) und
//! ist `Send + Sync`, wie es [`crate::AuditSource`] verlangt. Mehrere Threads
//! dürfen dieselbe Instanz referenzieren; ein `recv()` auf demselben
//! Dateideskriptor aus zwei Threads gleichzeitig liefert jedem Thread
//! unterschiedliche Datagramme (Kernel-seitige Serialisierung), teilt aber
//! keinen Zustand innerhalb dieses Prozesses, den diese Crate selbst
//! schützen müsste.
//!
//! # Fehler
//! [`crate::error::NetlinkError`] über
//! [`harw_dod_cap::SensorError::SourceUnavailable`] (Socket nicht zu öffnen
//! oder zu binden) und [`harw_dod_cap::SensorError::Io`] (Lesefehler nach
//! erfolgreichem Bind). Ein Timeout ohne neue Daten ist **kein** Fehler —
//! siehe [`NetlinkAuditSource::read_records`].
//!
//! # Examples
//! ```rust,no_run
//! use harw_dod_netlink::AuditSource;
//! use harw_dod_netlink::socket::NetlinkAuditSource;
//! use std::time::Duration;
//!
//! let source = NetlinkAuditSource::open()?;
//! let records = source.read_records(Duration::from_secs(1))?;
//! # Ok::<(), harw_dod_netlink::error::NetlinkError>(())
//! ```

use std::time::Duration;

use rustix::net::netlink::{AUDIT, SocketAddrNetlink};
use rustix::net::sockopt::{self, Timeout};
use rustix::net::{self, AddressFamily, RecvFlags, SocketType};

use harw_dod_cap::SensorError;

use crate::error::NetlinkError;
use crate::frame::split_messages;
use crate::record::RawRecord;
use crate::source::AuditSource;

/// Multicast-Gruppe zum Lesen von Audit-Ereignissen, ohne selbst der
/// Audit-Daemon zu werden.
///
/// UAPI-Konstante `AUDIT_NLGRP_READLOG` aus `<linux/audit.h>` (Wert `1`).
/// Nicht Teil von `rustix`s generischem Netlink-Vokabular, weil sie
/// audit-spezifisch ist — `rustix` liefert nur die protokollunabhängigen
/// `AF_NETLINK`/`NETLINK_AUDIT`-Bausteine.
const AUDIT_NLGRP_READLOG: u32 = 1;

/// Größe des Empfangspuffers. Reichlich bemessen für einen einzelnen
/// Audit-Record (üblicherweise deutlich unter 1 KiB Text) plus etwaige
/// vom Kernel gebündelte Folgenachrichten in einem `recv()`.
const RECV_BUFFER_LEN: usize = 8192;

/// Eine [`AuditSource`], die echte `AUDIT`-Ereignisse vom Kernel liest.
///
/// # Description
/// Hält einen gebundenen `AF_NETLINK`/`NETLINK_AUDIT`-Socket. Baut auf der
/// `AUDIT_NLGRP_READLOG`-Multicast-Gruppe auf: der Prozess muss nicht der
/// registrierte Audit-Daemon sein, aber `CAP_AUDIT_READ` besitzen.
#[derive(Debug)]
pub struct NetlinkAuditSource {
    socket: rustix::fd::OwnedFd,
}

impl NetlinkAuditSource {
    /// Öffnet den `AUDIT`-Netlink-Socket und bindet ihn auf die
    /// Lesegruppe.
    ///
    /// # Returns
    /// Eine einsatzbereite `NetlinkAuditSource`.
    ///
    /// # Errors
    /// - [`NetlinkError`] (über [`SensorError::SourceUnavailable`]), wenn der
    ///   Socket auf diesem Host nicht erzeugt oder gebunden werden kann —
    ///   zum Beispiel fehlende `CAP_AUDIT_READ`, ein Kernel ohne
    ///   Audit-Unterstützung, oder ein bereits durch einen anderen Prozess
    ///   belegter Port. Die Fehlermeldung nennt nie den zugrunde liegenden
    ///   Betriebssystem-Fehlercode (inhaltsfrei, siehe
    ///   [`harw_dod_cap::SensorError`]).
    ///
    /// # Examples
    /// ```rust,no_run
    /// use harw_dod_netlink::socket::NetlinkAuditSource;
    ///
    /// let source = NetlinkAuditSource::open()?;
    /// # Ok::<(), harw_dod_netlink::error::NetlinkError>(())
    /// ```
    pub fn open() -> Result<Self, NetlinkError> {
        let socket = net::socket(AddressFamily::NETLINK, SocketType::RAW, Some(AUDIT))
            .map_err(|_| SensorError::SourceUnavailable)?;

        let addr = SocketAddrNetlink::new(0, AUDIT_NLGRP_READLOG);
        net::bind(&socket, &addr).map_err(|_| SensorError::SourceUnavailable)?;

        Ok(Self { socket })
    }
}

impl AuditSource for NetlinkAuditSource {
    /// Liest das nächste Datagramm vom `AUDIT`-Socket, blockierend bis
    /// `timeout`.
    ///
    /// # Description
    /// Setzt `SO_RCVTIMEO` auf `timeout` und führt genau ein `recv()` aus.
    /// Das empfangene Datagramm wird über [`crate::frame::split_messages`]
    /// in einzelne [`RawRecord`]-Werte zerlegt — ein Datagramm kann mehrere
    /// `nlmsghdr`-gerahmte Nachrichten tragen.
    ///
    /// # Errors
    /// - [`SensorError::Io`], wenn `recv()` mit einem anderen Fehler als
    ///   Zeitüberschreitung scheitert.
    ///
    /// Ein Ablauf des Timeouts ohne neue Daten (`EAGAIN`/`EWOULDBLOCK`) ist
    /// **kein** Fehler: diese Methode liefert dann `Ok(Vec::new())`.
    fn read_records(&self, timeout: Duration) -> Result<Vec<RawRecord>, NetlinkError> {
        sockopt::set_socket_timeout(&self.socket, Timeout::Recv, Some(timeout))
            .map_err(|_| SensorError::SourceUnavailable)?;

        let mut buf = [0u8; RECV_BUFFER_LEN];
        match net::recv(&self.socket, &mut buf[..], RecvFlags::empty()) {
            Ok((received, _)) => Ok(split_messages(&buf[..received])),
            Err(errno)
                if errno == rustix::io::Errno::AGAIN || errno == rustix::io::Errno::WOULDBLOCK =>
            {
                Ok(Vec::new())
            }
            Err(errno) => Err(NetlinkError::from(SensorError::Io(errno.into()))),
        }
    }
}
