//! Peer-Allowlist am Socket: [`PeerGate`], [`resolve_escalator_uid`]
//! (PL-100 WP 3.1, Bedrohung B1, Invariante 3 „fremde Peers“).
//!
//! # Zweck
//! Nur der Escalator darf dem Warden Aufträge senden. Die vom Kernel
//! verbürgte Identität des verbundenen Prozesses (`SO_PEERCRED`, siehe
//! `ipc.rs`) muss die uid des Escalator-Systembenutzers haben. Jeder andere
//! Peer — auch ein Mitglied von `harw-warden-clients` (Angreifer A2) — wird
//! abgewiesen, **bevor** ein Byte seiner Anfrage gelesen oder dekodiert wird;
//! er erhält nur die feste Antwort `peer-rejected`, und ein Zähler steigt.
//!
//! # Auflösung der uid
//! Einmal beim Start, vor Landlock: entweder die feste uid aus `warden.toml`
//! (`escalator_uid`) oder der Benutzername (`escalator_user`, Vorgabe
//! `harw-escalator`), aufgelöst über `/etc/passwd` (der Systembenutzer wird
//! per `sysusers.d` dort angelegt; dieses Binary zieht keine NSS-Bibliothek).
//! Fehlt der Benutzer, ist er mehrdeutig oder ist seine uid `0`, startet der
//! Warden nicht. Die Datei wird als Bytes gelesen und zeilenweise
//! ausgewertet: fremde Zeilen mit Nicht-UTF-8 (etwa ein Latin-1-GECOS-Feld)
//! stören nicht. Eine Datei über [`MAX_PASSWD_LEN`] wird nie abgeschnitten
//! ausgewertet — ein Schnitt mitten in der Escalator-Zeile ergäbe sonst eine
//! falsche, kürzere uid —, sondern ist ein Startfehler.
//!
//! # Nebenläufigkeit
//! [`PeerGate`] ist `Send + Sync`; der Zähler ist ein `AtomicU64`.
//!
//! # Fehler
//! [`WardenBinError::EscalatorUserUnknown`],
//! [`WardenBinError::EscalatorUserAmbiguous`],
//! [`WardenBinError::EscalatorUidRoot`], [`WardenBinError::PasswdUnreadable`],
//! [`WardenBinError::PasswdTooLarge`].

use std::io::Read as _;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

use harw_dod_warden::EscalatorIdentity;

use crate::error::WardenBinError;

/// Pfad der lokalen Benutzerdatenbank.
pub const PASSWD_PATH: &str = "/etc/passwd";

/// Größenobergrenze für `/etc/passwd` in Bytes; eine größere Datei ist ein
/// Startfehler ([`WardenBinError::PasswdTooLarge`]).
pub const MAX_PASSWD_LEN: u64 = 4 * 1024 * 1024;

/// Die Peer-Allowlist: genau eine uid (der Escalator) plus Ablehnungszähler.
#[derive(Debug)]
pub struct PeerGate {
    escalator_uid: u32,
    rejected: AtomicU64,
}

impl PeerGate {
    /// Builds the gate for the resolved escalator uid.
    #[must_use]
    pub fn new(escalator_uid: u32) -> Self {
        Self {
            escalator_uid,
            rejected: AtomicU64::new(0),
        }
    }

    /// Returns whether a peer with `uid` may submit requests; counts every rejection.
    ///
    /// # Concurrency
    /// Der Zähler sättigt bei `u64::MAX` (kein Überlauf).
    pub fn admits(&self, uid: u32) -> bool {
        if uid == self.escalator_uid {
            return true;
        }
        self.record_rejection();
        false
    }

    /// Counts a rejected peer whose identity could not even be read.
    ///
    /// # Concurrency
    /// Sättigt bei `u64::MAX`.
    pub fn record_rejection(&self) {
        // `fetch_update` mit sättigender Addition: kein Überlauf, keine Panik;
        // das Ergebnis (alter Zählerstand) wird nicht gebraucht.
        let _previous = self
            .rejected
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |count| {
                Some(count.saturating_add(1))
            });
    }

    /// Returns how many peers were rejected so far.
    #[must_use]
    pub fn rejected_total(&self) -> u64 {
        self.rejected.load(Ordering::Relaxed)
    }
}

/// Resolves the escalator identity from `warden.toml` to a uid.
///
/// # Arguments
/// - `identity` (`&EscalatorIdentity`): Name oder feste uid.
/// - `passwd` (`&Path`): Benutzerdatenbank (Produktion: [`PASSWD_PATH`]).
///
/// # Errors
/// - [`WardenBinError::PasswdUnreadable`]: Datei nicht lesbar.
/// - [`WardenBinError::PasswdTooLarge`]: Datei größer als
///   [`MAX_PASSWD_LEN`] (nie abgeschnitten ausgewertet).
/// - [`WardenBinError::EscalatorUserUnknown`] /
///   [`WardenBinError::EscalatorUserAmbiguous`]: Benutzer fehlt bzw. steht
///   mehrfach (oder mit unlesbarer uid) in der Datei.
/// - [`WardenBinError::EscalatorUidRoot`]: die aufgelöste uid ist `0`.
pub fn resolve_escalator_uid(
    identity: &EscalatorIdentity,
    passwd: &Path,
) -> Result<u32, WardenBinError> {
    let uid = match identity {
        EscalatorIdentity::Uid(uid) => *uid,
        EscalatorIdentity::User(name) => {
            let mut bytes = Vec::new();
            // Ein Byte mehr als erlaubt lesen: so ist „zu groß“ erkennbar,
            // ohne je eine abgeschnittene Datei auszuwerten.
            std::fs::File::open(passwd)
                .and_then(|file| {
                    file.take(MAX_PASSWD_LEN.saturating_add(1))
                        .read_to_end(&mut bytes)
                })
                .map_err(WardenBinError::PasswdUnreadable)?;
            if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > MAX_PASSWD_LEN {
                return Err(WardenBinError::PasswdTooLarge);
            }
            uid_for_user(&bytes, name)?
        }
    };
    if uid == 0 {
        return Err(WardenBinError::EscalatorUidRoot);
    }
    Ok(uid)
}

/// Finds the uid of `name` in passwd-formatted bytes.
///
/// # Description
/// Zeilen `name:passwort:uid:gid:gecos:home:shell`, getrennt durch `\n`;
/// Kommentare (`#`) und Leerzeilen werden übersprungen. Verglichen wird der
/// Name als Bytes; nur das uid-Feld der passenden Zeile muss aus
/// ASCII-Ziffern bestehen. Andere Zeilen dürfen beliebige Bytes enthalten.
/// Genau ein Treffer mit numerischer uid ist verlangt.
///
/// # Arguments
/// - `passwd` (`&[u8]`): Inhalt der Benutzerdatenbank.
/// - `name` (`&str`): gesuchter Benutzername.
///
/// # Errors
/// - [`WardenBinError::EscalatorUserUnknown`]: kein Treffer.
/// - [`WardenBinError::EscalatorUserAmbiguous`]: mehrere Treffer oder eine
///   nicht numerische uid.
pub fn uid_for_user(passwd: &[u8], name: &str) -> Result<u32, WardenBinError> {
    let mut found: Option<u32> = None;
    for line in passwd.split(|byte| *byte == b'\n') {
        if line.is_empty() || line.starts_with(b"#") {
            continue;
        }
        let mut fields = line.split(|byte| *byte == b':');
        if fields.next() != Some(name.as_bytes()) {
            continue;
        }
        let uid = fields
            .nth(1)
            .and_then(parse_uid)
            .ok_or(WardenBinError::EscalatorUserAmbiguous)?;
        if found.replace(uid).is_some() {
            return Err(WardenBinError::EscalatorUserAmbiguous);
        }
    }
    found.ok_or(WardenBinError::EscalatorUserUnknown)
}

// Ein uid-Feld: nur ASCII-Ziffern (kein Vorzeichen, kein Leerzeichen), in `u32`.
fn parse_uid(field: &[u8]) -> Option<u32> {
    if field.is_empty() || !field.iter().all(u8::is_ascii_digit) {
        return None;
    }
    std::str::from_utf8(field).ok()?.parse::<u32>().ok()
}

#[cfg(test)]
mod tests {
    use super::{MAX_PASSWD_LEN, PeerGate, resolve_escalator_uid, uid_for_user};
    use crate::error::WardenBinError;
    use crate::test_support::{TestError, TestResult, ctx};
    use harw_dod_warden::EscalatorIdentity;

    const PASSWD: &str = "\
root:x:0:0:root:/root:/bin/bash
# Kommentar
harw-warden:x:981:981::/:/usr/sbin/nologin

harw-escalator:x:982:982::/:/usr/sbin/nologin
harw-escalator2:x:983:983::/:/usr/sbin/nologin
";

    #[test]
    fn test_foreign_peer_uid_is_refused_and_counted() {
        let gate = PeerGate::new(982);
        assert!(gate.admits(982));
        assert_eq!(gate.rejected_total(), 0);
        assert!(!gate.admits(0));
        assert!(!gate.admits(1000));
        assert!(!gate.admits(981));
        assert_eq!(gate.rejected_total(), 3);
        gate.record_rejection();
        assert_eq!(gate.rejected_total(), 4);
    }

    #[test]
    fn test_uid_for_user_finds_exactly_one_entry() -> TestResult {
        assert_eq!(
            uid_for_user(PASSWD.as_bytes(), "harw-escalator").map_err(ctx("lookup"))?,
            982
        );
        assert!(matches!(
            uid_for_user(PASSWD.as_bytes(), "harw-missing"),
            Err(WardenBinError::EscalatorUserUnknown)
        ));
        let duplicated = format!("{PASSWD}harw-escalator:x:990:990::/:/bin/false\n");
        assert!(matches!(
            uid_for_user(duplicated.as_bytes(), "harw-escalator"),
            Err(WardenBinError::EscalatorUserAmbiguous)
        ));
        for malformed in ["notanumber", "+982", " 982", "", "98 2"] {
            let line = format!("harw-escalator:x:{malformed}:1::/:/bin/false\n");
            assert!(
                matches!(
                    uid_for_user(line.as_bytes(), "harw-escalator"),
                    Err(WardenBinError::EscalatorUserAmbiguous)
                ),
                "{malformed:?} must not be accepted as a uid"
            );
        }
        Ok(())
    }

    #[test]
    fn test_foreign_non_utf8_lines_do_not_matter() -> TestResult {
        let mut passwd = b"legacy:x:1001:1001:M\xfcller:/home/legacy:/bin/sh\n".to_vec();
        passwd.extend_from_slice(PASSWD.as_bytes());
        assert_eq!(
            uid_for_user(&passwd, "harw-escalator").map_err(ctx("lookup"))?,
            982
        );
        Ok(())
    }

    #[test]
    fn test_oversized_passwd_is_refused_never_truncated() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let passwd = dir.path().join("passwd");
        // Die Grenze schneidet mitten in die uid der Escalator-Zeile:
        // abgeschnitten ergäbe sie `harw-escalator:x:98`, also uid 98.
        let line = b"harw-escalator:x:987:987::/:/usr/sbin/nologin\n";
        let limit = usize::try_from(MAX_PASSWD_LEN).map_err(ctx("limit"))?;
        let filler_len = limit.saturating_sub(20);
        let mut contents = vec![b'#'; filler_len];
        contents.push(b'\n');
        contents.extend_from_slice(line);
        std::fs::write(&passwd, &contents).map_err(ctx("write passwd"))?;
        let user = EscalatorIdentity::User("harw-escalator".to_owned());
        assert!(matches!(
            resolve_escalator_uid(&user, &passwd),
            Err(WardenBinError::PasswdTooLarge)
        ));
        // Genau an der Grenze ist die Datei noch zulässig.
        let mut exact = vec![b'#'; limit.saturating_sub(line.len()).saturating_sub(1)];
        exact.push(b'\n');
        exact.extend_from_slice(line);
        assert_eq!(exact.len(), limit);
        std::fs::write(&passwd, &exact).map_err(ctx("write passwd"))?;
        assert_eq!(
            resolve_escalator_uid(&user, &passwd).map_err(ctx("resolve"))?,
            987
        );
        Ok(())
    }

    #[test]
    fn test_resolution_refuses_root_and_reads_the_passwd_file() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let passwd = dir.path().join("passwd");
        std::fs::write(&passwd, PASSWD).map_err(ctx("write passwd"))?;
        let user = EscalatorIdentity::User("harw-escalator".to_owned());
        assert_eq!(
            resolve_escalator_uid(&user, &passwd).map_err(ctx("resolve"))?,
            982
        );
        let root = EscalatorIdentity::User("root".to_owned());
        assert!(matches!(
            resolve_escalator_uid(&root, &passwd),
            Err(WardenBinError::EscalatorUidRoot)
        ));
        assert_eq!(
            resolve_escalator_uid(&EscalatorIdentity::Uid(987), &passwd)
                .map_err(ctx("fixed uid"))?,
            987
        );
        match resolve_escalator_uid(&user, &dir.path().join("missing")) {
            Err(WardenBinError::PasswdUnreadable(_)) => Ok(()),
            other => Err(TestError::Unexpected(format!(
                "missing passwd must fail, got {other:?}"
            ))),
        }
    }
}
