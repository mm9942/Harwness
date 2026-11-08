//! `SO_PEERCRED`-Zulassungsliste des IPC-Empfangspfads (PL-100 WP0.5).
//!
//! # Verantwortungsbereich
//! Der Empfangssocket (`/run/harw/sentinel.sock`, Modus `0770`, Gruppe
//! `harw-ipc`) ist für jedes Mitglied von `harw-ipc` erreichbar. Bis zu
//! diesem Arbeitspaket hat [`crate::ipc`] die Kernel-verbürgte Identität
//! eines Peers nur geloggt; jeder Prozess mit Zugriff auf den Socket konnte
//! beliebige `SecurityEvent`s einspeisen, deren Text später in einen
//! LLM-Kontext gelangt (INVENTORY Lücke 6). [`PeerAllowlist`] legt fest,
//! welche Nutzer-IDs Ereignisse einreichen dürfen: die konfigurierten
//! Sonden-Konten (`--probe-peer-user`, Vorgabe
//! [`DEFAULT_PROBE_PEER_USERS`], wie in `deploy/sysusers.d/harw.conf` und
//! den Units `harw-probe-bpf.service`/`harw-probe-fs.service` angelegt) und
//! immer uid `0`.
//!
//! # Auflösung beim Start
//! [`resolve_probe_peers`] löst die Einträge genau einmal beim Start auf —
//! **vor** der Landlock-Selbstbeschränkung, denn `/etc` ist keine Lesewurzel
//! des Sentinels. Ein rein numerischer Eintrag gilt unmittelbar als uid
//! (für Konten aus anderen NSS-Quellen als `files`); jeder andere Eintrag
//! wird in [`PASSWD_PATH`] gesucht (dieselbe Datenbank, in die
//! `systemd-sysusers` die Sonden-Konten schreibt), erster Treffer gewinnt
//! wie bei `getpwnam(3)`. Kein `unsafe`, kein libc-Aufruf, keine neue
//! Abhängigkeit. Ein nicht auflösbarer Eintrag wird übersprungen und von
//! `main` gemeldet — die Liste wird dadurch nur **kürzer**, nie offener
//! (fail closed); uid `0` bleibt immer zugelassen.
//!
//! # Wichtige Typen
//! - [`PeerAllowlist`]: die unveränderliche Menge zugelassener uids.
//! - [`PeerResolution`]: Ergebnis der Startauflösung samt der nicht
//!   auflösbaren Einträge (für Log und `--print-sandbox`).
//!
//! # Nebenläufigkeit
//! [`PeerAllowlist`] ist ein unveränderlicher Werttyp (`Clone + Send +
//! Sync`); die Annahmeschleife in [`crate::ipc`] übernimmt eine eigene
//! Kopie in ihren Thread.
//!
//! # Fehler
//! Keine eigenen: ein Lesefehler von [`PASSWD_PATH`] macht nur die
//! betroffenen Namen unauflösbar (siehe oben).
//!
//! # Examples
//! ```rust,ignore
//! use harw_sentinel::peer_allow::{PeerAllowlist, resolve_probe_peers, PASSWD_PATH};
//! use std::path::Path;
//!
//! let entries = vec!["harw-probe-bpf".to_owned(), "991".to_owned()];
//! let resolution = resolve_probe_peers(&entries, Path::new(PASSWD_PATH));
//! assert!(resolution.allowlist.permits(0));
//! assert!(resolution.allowlist.permits(991));
//! ```

use std::collections::BTreeSet;
use std::io::Read as _;
use std::path::Path;

/// Die Nutzerdatenbank, gegen die Kontonamen aufgelöst werden.
pub const PASSWD_PATH: &str = "/etc/passwd";

/// Vorgabe für `--probe-peer-user`: die Konten der beiden privilegierten
/// Sonden aus `deploy/sysusers.d/harw.conf` (`User=` in
/// `harw-probe-bpf.service` bzw. `harw-probe-fs.service`).
pub const DEFAULT_PROBE_PEER_USERS: [&str; 2] = ["harw-probe-bpf", "harw-probe-fs"];

/// Die immer zugelassene uid (root).
pub const ROOT_UID: u32 = 0;

/// Obergrenze der gelesenen Bytes von [`PASSWD_PATH`]; eine längere Datei
/// wird abgeschnitten gelesen (spätere Einträge bleiben unauflösbar).
const MAX_PASSWD_BYTES: u64 = 4 * 1024 * 1024;

/// `(uid_t)-1` ist keine gültige Nutzer-ID (`setresuid(2)`: „unverändert").
const INVALID_UID: u32 = u32::MAX;

/// Menge der Nutzer-IDs, die Ereignisse über den IPC-Socket einreichen
/// dürfen.
///
/// # Description
/// Wird einmal beim Start gebaut ([`resolve_probe_peers`]) und danach nie
/// verändert. [`Self::with_root`] nimmt uid `0` immer mit auf.
///
/// # Concurrency
/// Unveränderlicher Werttyp, `Clone + Send + Sync`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PeerAllowlist {
    uids: BTreeSet<u32>,
}

impl PeerAllowlist {
    /// Baut eine Zulassungsliste aus `uids` plus uid `0`.
    ///
    /// # Arguments
    /// - `uids` (`impl IntoIterator<Item = u32>`): die aufgelösten
    ///   Sonden-uids; Duplikate sind unschädlich.
    ///
    /// # Returns
    /// Eine Liste, die uid `0` und jede übergebene uid zulässt.
    ///
    /// # Examples
    /// ```rust,ignore
    /// let allowlist = PeerAllowlist::with_root([991]);
    /// assert!(allowlist.permits(0) && allowlist.permits(991));
    /// assert!(!allowlist.permits(1000));
    /// ```
    #[must_use]
    pub fn with_root(uids: impl IntoIterator<Item = u32>) -> Self {
        let mut uids: BTreeSet<u32> = uids.into_iter().collect();
        uids.insert(ROOT_UID);
        Self { uids }
    }

    /// Testfassung **ohne** die implizite uid `0`: die Socket-Tests laufen
    /// unter Umständen als root und müssen dennoch eine Abweisung des
    /// eigenen Prozesses prüfen können.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn exactly(uids: impl IntoIterator<Item = u32>) -> Self {
        Self {
            uids: uids.into_iter().collect(),
        }
    }

    /// Ob ein Peer mit dieser (Kernel-verbürgten) uid Ereignisse einreichen
    /// darf.
    ///
    /// # Arguments
    /// - `uid` (`u32`): die `SO_PEERCRED`-uid des Peers.
    #[must_use]
    pub fn permits(&self, uid: u32) -> bool {
        self.uids.contains(&uid)
    }

    /// Die zugelassenen uids in aufsteigender Reihenfolge.
    pub fn uids(&self) -> impl Iterator<Item = u32> + '_ {
        self.uids.iter().copied()
    }
}

/// Ein aufgelöster Eintrag von `--probe-peer-user`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedPeer {
    /// Der Eintrag, wie er konfiguriert wurde (Name oder Zahl).
    pub entry: String,
    /// Die ermittelte uid.
    pub uid: u32,
}

/// Ergebnis von [`resolve_probe_peers`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PeerResolution {
    /// Die wirksame Zulassungsliste (aufgelöste uids plus uid `0`).
    pub allowlist: PeerAllowlist,
    /// Jeder erfolgreich aufgelöste Eintrag, in Konfigurationsreihenfolge.
    pub resolved: Vec<ResolvedPeer>,
    /// Jeder nicht auflösbare Eintrag, in Konfigurationsreihenfolge.
    pub unresolved: Vec<String>,
}

/// Löst die konfigurierten Sonden-Konten in eine [`PeerAllowlist`] auf.
///
/// # Description
/// Rein numerische Einträge gelten direkt als uid (außer `4294967295`);
/// alle übrigen werden in `passwd_path` gesucht. Die Datei wird nur gelesen,
/// wenn mindestens ein Eintrag ein Name ist. Ein Lesefehler macht jeden
/// Namen unauflösbar, aber nie die Liste offener.
///
/// # Arguments
/// - `entries` (`&[String]`): die Werte von `--probe-peer-user`.
/// - `passwd_path` (`&Path`): die Nutzerdatenbank (in Produktion
///   [`PASSWD_PATH`]).
///
/// # Returns
/// Die [`PeerResolution`]; ihre `allowlist` enthält immer uid `0`.
///
/// # Concurrency
/// Liest höchstens eine Datei; nur vor der Landlock-Selbstbeschränkung
/// aufrufen (`/etc` ist danach nicht mehr lesbar).
#[must_use]
pub fn resolve_probe_peers(entries: &[String], passwd_path: &Path) -> PeerResolution {
    let needs_passwd = entries.iter().any(|entry| numeric_uid(entry).is_none());
    let passwd = if needs_passwd {
        read_passwd(passwd_path).ok()
    } else {
        None
    };

    let mut resolved = Vec::with_capacity(entries.len());
    let mut unresolved = Vec::new();
    for entry in entries {
        let uid = numeric_uid(entry).or_else(|| {
            passwd
                .as_deref()
                .and_then(|passwd| uid_for_name(passwd, entry))
        });
        match uid {
            Some(uid) => resolved.push(ResolvedPeer {
                entry: entry.clone(),
                uid,
            }),
            None => unresolved.push(entry.clone()),
        }
    }

    PeerResolution {
        allowlist: PeerAllowlist::with_root(resolved.iter().map(|peer| peer.uid)),
        resolved,
        unresolved,
    }
}

/// Ein rein numerischer Eintrag als uid; `None` für Namen und für die
/// ungültige uid `4294967295`.
fn numeric_uid(entry: &str) -> Option<u32> {
    if entry.is_empty() || !entry.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    entry.parse::<u32>().ok().filter(|uid| *uid != INVALID_UID)
}

/// Liest höchstens [`MAX_PASSWD_BYTES`] von `path` als Text.
fn read_passwd(path: &Path) -> std::io::Result<String> {
    let file = std::fs::File::open(path)?;
    let mut text = String::new();
    file.take(MAX_PASSWD_BYTES).read_to_string(&mut text)?;
    Ok(text)
}

/// Sucht `name` im `passwd(5)`-Text und liefert die uid des ersten
/// wohlgeformten Treffers.
///
/// # Description
/// Format je Zeile: `name:passwort:uid:gid:gecos:home:shell`. Leere Zeilen,
/// Kommentare (`#`) und NIS-Kompatibilitätszeilen (`+`/`-`) werden
/// übersprungen, ebenso Zeilen mit fehlender oder nicht numerischer uid.
///
/// # Returns
/// `Some(uid)` für den ersten passenden Eintrag, sonst `None`.
fn uid_for_name(passwd: &str, name: &str) -> Option<u32> {
    passwd.lines().find_map(|line| {
        if line.is_empty() || line.starts_with(['#', '+', '-']) {
            return None;
        }
        let mut fields = line.split(':');
        if fields.next()? != name {
            return None;
        }
        let _password = fields.next()?;
        let uid = fields.next()?;
        if uid.is_empty() || !uid.bytes().all(|byte| byte.is_ascii_digit()) {
            return None;
        }
        uid.parse::<u32>().ok().filter(|uid| *uid != INVALID_UID)
    })
}

#[cfg(test)]
mod tests {
    use super::{
        DEFAULT_PROBE_PEER_USERS, PeerAllowlist, ROOT_UID, resolve_probe_peers, uid_for_name,
    };
    use crate::test_support::{TestResult, ctx};

    const PASSWD: &str = "\
root:x:0:0:root:/root:/bin/bash
# a comment line
+nisuser::::::
harw-sentinel:x:990:990:Harw-DoD sentinel:/:/usr/sbin/nologin
harw-probe-fs:x:991:991:Harw-DoD fanotify probe:/:/usr/sbin/nologin
harw-probe-bpf:x:992:992:Harw-DoD eBPF probe:/:/usr/sbin/nologin
broken:x:notanumber:1::/:/bin/false
harw-probe-bpf:x:4000:4000:duplicate, ignored:/:/bin/false
";

    fn entries(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_owned()).collect()
    }

    fn write_passwd(dir: &tempfile::TempDir) -> TestResult<std::path::PathBuf> {
        let path = dir.path().join("passwd");
        std::fs::write(&path, PASSWD).map_err(ctx("write passwd fixture"))?;
        Ok(path)
    }

    #[test]
    fn test_with_root_always_admits_uid_zero() {
        let allowlist = PeerAllowlist::with_root([991]);
        assert!(allowlist.permits(ROOT_UID));
        assert!(allowlist.permits(991));
        assert!(!allowlist.permits(1000));
        assert_eq!(allowlist.uids().collect::<Vec<_>>(), vec![0, 991]);
    }

    #[test]
    fn test_empty_allowlist_still_admits_only_root() {
        let allowlist = PeerAllowlist::with_root(std::iter::empty());
        assert!(allowlist.permits(0));
        assert!(!allowlist.permits(1));
    }

    #[test]
    fn test_uid_for_name_takes_the_first_well_formed_match() {
        assert_eq!(uid_for_name(PASSWD, "harw-probe-bpf"), Some(992));
        assert_eq!(uid_for_name(PASSWD, "harw-probe-fs"), Some(991));
        assert_eq!(uid_for_name(PASSWD, "root"), Some(0));
    }

    #[test]
    fn test_uid_for_name_skips_comments_nis_and_malformed_lines() {
        assert_eq!(uid_for_name(PASSWD, "nobody-here"), None);
        assert_eq!(uid_for_name(PASSWD, "+nisuser"), None);
        assert_eq!(uid_for_name(PASSWD, "broken"), None);
        assert_eq!(uid_for_name(PASSWD, ""), None);
        // Ein Präfix ist kein Treffer.
        assert_eq!(uid_for_name(PASSWD, "harw-probe"), None);
    }

    #[test]
    fn test_resolve_default_probe_users_against_a_passwd_fixture() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let path = write_passwd(&dir)?;
        let resolution = resolve_probe_peers(&entries(&DEFAULT_PROBE_PEER_USERS), &path);
        assert!(resolution.unresolved.is_empty());
        assert_eq!(
            resolution.allowlist.uids().collect::<Vec<_>>(),
            vec![0, 991, 992]
        );
        assert!(!resolution.allowlist.permits(990), "the sentinel itself is no probe");
        Ok(())
    }

    #[test]
    fn test_numeric_entries_need_no_passwd_and_the_invalid_uid_is_refused() {
        let resolution = resolve_probe_peers(
            &entries(&["1234", "4294967295"]),
            std::path::Path::new("/nonexistent-harw-sentinel-passwd"),
        );
        assert_eq!(
            resolution.allowlist.uids().collect::<Vec<_>>(),
            vec![0, 1234]
        );
        assert_eq!(resolution.unresolved, entries(&["4294967295"]));
    }

    #[test]
    fn test_unreadable_passwd_leaves_names_unresolved_and_admits_only_root() {
        let resolution = resolve_probe_peers(
            &entries(&DEFAULT_PROBE_PEER_USERS),
            std::path::Path::new("/nonexistent-harw-sentinel-passwd"),
        );
        assert_eq!(resolution.unresolved, entries(&DEFAULT_PROBE_PEER_USERS));
        assert!(resolution.resolved.is_empty());
        assert_eq!(resolution.allowlist.uids().collect::<Vec<_>>(), vec![0]);
    }

    #[test]
    fn test_unknown_name_is_reported_while_known_ones_resolve() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let path = write_passwd(&dir)?;
        let resolution =
            resolve_probe_peers(&entries(&["harw-probe-fs", "harw-probe-netlink"]), &path);
        assert_eq!(resolution.unresolved, entries(&["harw-probe-netlink"]));
        assert_eq!(resolution.resolved.len(), 1);
        assert!(resolution.allowlist.permits(991));
        assert!(!resolution.allowlist.permits(992));
        Ok(())
    }
}
