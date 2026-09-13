//! Peer-Identifikation über `SO_PEERCRED` — kein Bearer-Token, kein zweiter
//! Autoritätspfad.
//!
//! # Verantwortungsbereich
//! `harw-web` bindet ausschließlich auf einen Unix-Socket. Der Aufrufer wird
//! über die Kernel-verbürgten Peer-Credentials des Sockets identifiziert
//! (`pid`/`uid`/`gid`), nie über einen Token im HTTP-Rumpf oder -Header.
//! Dieses Modul liest genau diesen einen Wert, direkt nach `accept()` — der
//! Kernel füllt ihn aus dem verbindenden Prozess, nicht aus dessen
//! (fälschbarer) Nutzlast.
//!
//! # Vorbild
//! Dasselbe Muster wie `harw_sentinel::ipc` (`rustix::net::sockopt::socket_peercred`,
//! dort auf einem `SOCK_SEQPACKET`-Socket). `harw-web` braucht einen
//! Byte-Strom für HTTP (`SOCK_STREAM`, nicht `SOCK_SEQPACKET`), liest die
//! Credentials aber über dieselbe `rustix`-Funktion und denselben
//! Aufrufzeitpunkt — kein zweites Verfahren im Workspace.
//!
//! # Ohne `unsafe`
//! `rustix::net::sockopt::socket_peercred` ist ein sicherer Aufruf; diese
//! Datei enthält kein `unsafe` (ohnehin durch `unsafe_code = "forbid"` im
//! Workspace verboten).
//!
//! # Nebenläufigkeit
//! [`PeerCredentials`] ist ein zustandsloser, `Copy`-fähiger Wert; das Lesen
//! über [`read_peer_credentials`] ist ein einzelner, blockierungsfreier
//! Syscall ohne gemeinsamen Zustand.

use rustix::fd::AsFd;

use crate::error::WebError;

/// Kernel-verbürgte Identität des verbundenen Peers (`SO_PEERCRED`).
///
/// # Description
/// Gelesen genau einmal, direkt nach `accept()`. `harw-web` selbst
/// **entscheidet** anhand dieser Werte nicht — die Zuordnung zu einer
/// [`harw_operations::operation::PermissionTier`] übernimmt
/// [`crate::authz::PeerAuthorizer`], eine von außen (serverseitig
/// vertrauenswürdig) konfigurierte Richtlinie.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PeerCredentials {
    /// Prozess-ID des Peers zum Zeitpunkt von `connect()`.
    pub pid: u32,
    /// Effektive Nutzer-ID des Peers.
    pub uid: u32,
    /// Effektive Gruppen-ID des Peers.
    pub gid: u32,
}

impl PeerCredentials {
    /// Baut `PeerCredentials` aus einzelnen Werten — v. a. für Tests, in
    /// denen kein echter Socket gebunden werden darf.
    ///
    /// # Arguments
    /// - `pid` (`u32`): Prozess-ID.
    /// - `uid` (`u32`): Effektive Nutzer-ID.
    /// - `gid` (`u32`): Effektive Gruppen-ID.
    ///
    /// # Returns
    /// Eine neue [`PeerCredentials`]-Instanz mit den angegebenen Werten.
    ///
    /// # Examples
    /// ```rust
    /// use harw_web::peer::PeerCredentials;
    ///
    /// let peer = PeerCredentials::new(1234, 1000, 1000);
    /// assert_eq!(peer.uid, 1000);
    /// ```
    #[must_use]
    pub const fn new(pid: u32, uid: u32, gid: u32) -> Self {
        Self { pid, uid, gid }
    }
}

/// Liest die `SO_PEERCRED`-Identität eines verbundenen Unix-Sockets.
///
/// # Description
/// Ruft `rustix::net::sockopt::socket_peercred` auf `socket` auf. Muss
/// direkt nach `accept()` aufgerufen werden — der Kernel füllt die Werte aus
/// dem verbindenden Prozess zu diesem Zeitpunkt.
///
/// # Arguments
/// - `socket` (`&impl AsFd`): Ein akzeptierter Unix-Socket (z. B.
///   `tokio::net::UnixStream`, das `AsFd` implementiert).
///
/// # Returns
/// Die [`PeerCredentials`] des verbundenen Prozesses.
///
/// # Errors
/// [`WebError::PeerCredentialsUnavailable`], wenn `SO_PEERCRED` auf diesem
/// Socket nicht lesbar ist.
///
/// # Concurrency
/// Ein einzelner, synchroner Syscall ohne Sperren; sicher von jedem Thread
/// aufrufbar.
///
/// # Examples
/// ```rust,no_run
/// use harw_web::peer::read_peer_credentials;
///
/// # async fn run(stream: &tokio::net::UnixStream) -> Result<(), harw_web::error::WebError> {
/// let peer = read_peer_credentials(stream)?;
/// println!("uid={}", peer.uid);
/// # Ok(())
/// # }
/// ```
pub fn read_peer_credentials(socket: &impl AsFd) -> Result<PeerCredentials, WebError> {
    let creds = rustix::net::sockopt::socket_peercred(socket)
        .map_err(|_| WebError::PeerCredentialsUnavailable)?;
    Ok(PeerCredentials {
        pid: creds.pid.as_raw_pid() as u32,
        uid: creds.uid.as_raw() as u32,
        gid: creds.gid.as_raw() as u32,
    })
}

#[cfg(test)]
mod tests {
    use super::PeerCredentials;

    #[test]
    fn test_peer_credentials_new_sets_all_fields() {
        let peer = PeerCredentials::new(42, 1000, 1000);
        assert_eq!(peer.pid, 42);
        assert_eq!(peer.uid, 1000);
        assert_eq!(peer.gid, 1000);
    }

    #[test]
    fn test_peer_credentials_is_copy() {
        let peer = PeerCredentials::new(1, 2, 3);
        let copied = peer;
        assert_eq!(peer, copied);
    }

    #[test]
    fn test_peer_credentials_equality() {
        assert_eq!(PeerCredentials::new(1, 2, 3), PeerCredentials::new(1, 2, 3));
        assert_ne!(PeerCredentials::new(1, 2, 3), PeerCredentials::new(1, 2, 4));
    }

    // `read_peer_credentials` selbst wird — wie `harw_sentinel::ipc::IpcListener::accept`
    // — nicht getestet: das erforderte einen echten, gebundenen Socket, was diesem
    // Knoten ausdrücklich verboten ist ("Stelle in keinem Test eine echte
    // Netzwerkverbindung her und binde keinen echten Socket").
}
