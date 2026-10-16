//! Benutzer-/Gruppennamen aus `/etc/passwd` und `/etc/group` (reines Rust).
//!
//! Keine NSS-Auflösung (kein `getpwuid`): nur die lokalen Dateien. Nicht
//! auflösbare IDs werden vom Aufrufer numerisch angezeigt. Die Dateien werden
//! höchstens [`MAX_DB_BYTES`] groß gelesen; kaputte Zeilen werden übersprungen.

use std::collections::HashMap;
use std::io::Read;

/// Höchstgröße einer gelesenen Benutzerdatei.
pub const MAX_DB_BYTES: u64 = 1024 * 1024;

/// Namenstabellen für UIDs und GIDs.
#[derive(Debug, Default, Clone)]
pub struct UserDb {
    users: HashMap<u32, String>,
    groups: HashMap<u32, String>,
    by_name: HashMap<String, u32>,
}

impl UserDb {
    /// Lädt `/etc/passwd` und `/etc/group`; fehlende Dateien ergeben leere Tabellen.
    #[must_use]
    pub fn load() -> Self {
        let passwd = read_bounded("/etc/passwd");
        let group = read_bounded("/etc/group");
        Self::parse(&passwd, &group)
    }

    /// Baut die Tabellen aus Dateiinhalten.
    #[must_use]
    pub fn parse(passwd: &str, group: &str) -> Self {
        let mut db = Self::default();
        for line in passwd.lines() {
            let mut fields = line.split(':');
            let (Some(name), Some(_pw), Some(uid)) = (fields.next(), fields.next(), fields.next())
            else {
                continue;
            };
            if name.is_empty() || name.starts_with('#') {
                continue;
            }
            if let Ok(uid) = uid.parse::<u32>() {
                db.users.entry(uid).or_insert_with(|| name.to_owned());
                db.by_name.entry(name.to_owned()).or_insert(uid);
            }
        }
        for line in group.lines() {
            let mut fields = line.split(':');
            let (Some(name), Some(_pw), Some(gid)) = (fields.next(), fields.next(), fields.next())
            else {
                continue;
            };
            if name.is_empty() || name.starts_with('#') {
                continue;
            }
            if let Ok(gid) = gid.parse::<u32>() {
                db.groups.entry(gid).or_insert_with(|| name.to_owned());
            }
        }
        db
    }

    /// Benutzername zu `uid`.
    #[must_use]
    pub fn user(&self, uid: u32) -> Option<&str> {
        self.users.get(&uid).map(String::as_str)
    }

    /// Gruppenname zu `gid`.
    #[must_use]
    pub fn group(&self, gid: u32) -> Option<&str> {
        self.groups.get(&gid).map(String::as_str)
    }

    /// `uid` zu einem Benutzernamen.
    #[must_use]
    pub fn uid_of(&self, name: &str) -> Option<u32> {
        self.by_name.get(name).copied()
    }

    /// Name oder numerische Form (`1000`).
    #[must_use]
    pub fn user_or_id(&self, uid: u32) -> String {
        self.user(uid)
            .map_or_else(|| uid.to_string(), str::to_owned)
    }

    /// Name oder numerische Form (`1000`).
    #[must_use]
    pub fn group_or_id(&self, gid: u32) -> String {
        self.group(gid)
            .map_or_else(|| gid.to_string(), str::to_owned)
    }
}

/// Liest eine Datei höchstens [`MAX_DB_BYTES`] groß; Fehler ergeben `""`.
fn read_bounded(path: &str) -> String {
    let Ok(file) = std::fs::File::open(path) else {
        return String::new();
    };
    let mut bytes = Vec::new();
    if file.take(MAX_DB_BYTES).read_to_end(&mut bytes).is_err() {
        return String::new();
    }
    String::from_utf8_lossy(&bytes).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TestResult;

    #[test]
    fn parses_names_and_skips_garbage() -> TestResult {
        let db = UserDb::parse(
            "root:x:0:0:root:/root:/bin/bash\nbroken line\n:x:5:5\nalice:x:1000:1000::/home/alice:/bin/sh\n",
            "root:x:0:\nstaff:x:50:alice\nnope\n",
        );
        assert_eq!(db.user(0), Some("root"));
        assert_eq!(db.user(1000), Some("alice"));
        assert_eq!(db.group(50), Some("staff"));
        assert_eq!(db.uid_of("alice"), Some(1000));
        assert_eq!(db.user_or_id(4242), "4242");
        assert_eq!(db.group_or_id(4242), "4242");
        Ok(())
    }
}
