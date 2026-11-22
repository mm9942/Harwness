//! Das Admin-Konto: Benutzername, E-Mail, Argon2id-Hash, optionales TOTP.
//!
//! # Beschreibung
//! Gespeichert wird `<home>/admin/account.json` (Verzeichnis 0700, Datei 0600,
//! atomar geschrieben). Das TOTP-Geheimnis liegt nie im Klartext in der Datei,
//! sondern im versiegelten Secret-Store (`secrets:<uuid>`-Referenz, siehe
//! `crate::secret_store`; KEK bzw. AuthHub wie bei den Provider-Schlüsseln).
//! Passwörter, Hashes und Geheimnisse werden nie geloggt oder in Fehlern
//! ausgegeben.

use std::io::Write as _;
use std::path::{Path, PathBuf};

use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argon2::{Algorithm, Argon2, Params, Version};
use serde::{Deserialize, Serialize};

/// Argon2id: 64 MiB, 3 Durchläufe, 1 Lane (Stärke über Speicher, nicht Parallelität).
const M_COST_KIB: u32 = 64 * 1024;
const T_COST: u32 = 3;
const P_COST: u32 = 1;
/// Mindest- und Höchstlänge des Passworts (Zeichen).
pub(crate) const MIN_PASSWORD_CHARS: usize = 12;
pub(crate) const MAX_PASSWORD_BYTES: usize = 256;
/// Verzeichnis unter dem Home.
const ADMIN_DIR: &str = "admin";
const ACCOUNT_FILE: &str = "account.json";

/// Persistiertes Admin-Konto.
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Account {
    pub version: u32,
    pub username: String,
    pub email: String,
    /// PHC-String `$argon2id$…`.
    pub password_hash: String,
    /// `secrets:<uuid>`-Referenz auf das versiegelte TOTP-Geheimnis.
    #[serde(default)]
    pub totp_secret_ref: Option<String>,
    /// Zuletzt akzeptierter TOTP-Zeitschritt (Replay-Schutz).
    #[serde(default)]
    pub totp_last_step: u64,
    /// Aufeinanderfolgende Fehlversuche gegen dieses Konto.
    #[serde(default)]
    pub failed_count: u32,
    /// Unix-Sekunden, bis zu denen das Konto gesperrt ist (0: nicht gesperrt).
    #[serde(default)]
    pub locked_until: u64,
    /// Unix-Sekunden des letzten Fehlversuchs.
    #[serde(default)]
    pub last_failure: u64,
    pub created: u64,
    pub updated: u64,
}

impl std::fmt::Debug for Account {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Account")
            .field("username", &self.username)
            .field("totp", &self.totp_secret_ref.is_some())
            .finish_non_exhaustive()
    }
}

pub(crate) fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// Verzeichnis des Admin-Zustands.
pub(crate) fn admin_dir(home: &Path) -> PathBuf {
    home.join(ADMIN_DIR)
}

/// Legt das Verzeichnis mit Modus 0700 an.
pub(crate) fn ensure_admin_dir(home: &Path) -> Result<PathBuf, String> {
    let dir = admin_dir(home);
    std::fs::create_dir_all(&dir)
        .map_err(|error| format!("Admin-Verzeichnis {} nicht anlegbar: {error}", dir.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).map_err(
            |error| format!("Rechte von {} nicht setzbar: {error}", dir.display()),
        )?;
    }
    Ok(dir)
}

/// Schreibt `bytes` atomar mit Modus 0600 (temporäre Datei, fsync, Umbenennen).
pub(crate) fn write_private_atomic(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let tmp = path.with_extension("tmp");
    let _ = std::fs::remove_file(&tmp);
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    let result = (|| -> std::io::Result<()> {
        let mut file = options.open(&tmp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        std::fs::rename(&tmp, path)
    })();
    if let Err(error) = result {
        let _ = std::fs::remove_file(&tmp);
        return Err(format!("{} nicht schreibbar: {error}", path.display()));
    }
    Ok(())
}

fn argon2() -> Result<Argon2<'static>, String> {
    let params = Params::new(M_COST_KIB, T_COST, P_COST, None)
        .map_err(|_| "Argon2-Parameter ungültig".to_owned())?;
    Ok(Argon2::new(Algorithm::Argon2id, Version::V0x13, params))
}

/// Hasht ein Passwort mit frischem Salt.
pub(crate) fn hash_password(password: &str) -> Result<String, String> {
    let mut salt_bytes = [0_u8; 16];
    getrandom::fill(&mut salt_bytes).map_err(|_| "Zufallsquelle nicht verfügbar".to_owned())?;
    let salt = SaltString::encode_b64(&salt_bytes).map_err(|_| "Salt ungültig".to_owned())?;
    argon2()?
        .hash_password(password.as_bytes(), &salt)
        .map(|hash| hash.to_string())
        .map_err(|_| "Passwort konnte nicht gehasht werden".to_owned())
}

/// Prüft ein Passwort gegen einen PHC-Hash (Argon2 vergleicht konstant-zeitig).
pub(crate) fn verify_password(password: &str, phc: &str) -> bool {
    let Ok(parsed) = PasswordHash::new(phc) else {
        return false;
    };
    Argon2::default()
        .verify_password(password.as_bytes(), &parsed)
        .is_ok()
}

/// Ein Hash, gegen den bei unbekanntem Benutzer verifiziert wird, damit Zeit
/// und Aufwand keinen Benutzernamen verraten.
pub(crate) fn dummy_hash() -> &'static str {
    static HASH: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    HASH.get_or_init(|| hash_password("dummy-password-for-timing").unwrap_or_default())
}

/// Passwortregeln (Länge); keine weiteren Kompositionsregeln.
pub(crate) fn check_password_policy(password: &str) -> Result<(), String> {
    if password.chars().count() < MIN_PASSWORD_CHARS {
        return Err(format!(
            "Das Passwort muss mindestens {MIN_PASSWORD_CHARS} Zeichen lang sein."
        ));
    }
    if password.len() > MAX_PASSWORD_BYTES {
        return Err(format!(
            "Das Passwort darf höchstens {MAX_PASSWORD_BYTES} Bytes lang sein."
        ));
    }
    Ok(())
}

/// Benutzername: 3–64 Zeichen aus `[A-Za-z0-9._-]`.
pub(crate) fn check_username(name: &str) -> Result<(), String> {
    let ok = (3..=64).contains(&name.len())
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'));
    if ok {
        Ok(())
    } else {
        Err("Der Benutzername muss 3 bis 64 Zeichen aus A-Z, a-z, 0-9, '.', '_' und '-' enthalten."
            .to_owned())
    }
}

/// E-Mail: bewusst nur eine grobe Formprüfung (kein Versand, nur Hinterlegung).
pub(crate) fn check_email(mail: &str) -> Result<(), String> {
    let ok = mail.len() <= 254
        && mail.is_ascii()
        && !mail.chars().any(|c| c.is_whitespace() || c.is_control())
        && mail
            .split_once('@')
            .is_some_and(|(local, domain)| !local.is_empty() && domain.contains('.'));
    if ok {
        Ok(())
    } else {
        Err("Die E-Mail-Adresse ist ungültig.".to_owned())
    }
}

impl Account {
    pub(crate) fn new(username: String, email: String, password_hash: String) -> Self {
        let now = now_unix();
        Self {
            version: 1,
            username,
            email,
            password_hash,
            totp_secret_ref: None,
            totp_last_step: 0,
            failed_count: 0,
            locked_until: 0,
            last_failure: 0,
            created: now,
            updated: now,
        }
    }

    pub(crate) fn path(home: &Path) -> PathBuf {
        admin_dir(home).join(ACCOUNT_FILE)
    }

    /// Lädt das Konto; `Ok(None)` ohne Datei.
    pub(crate) fn load(home: &Path) -> Result<Option<Self>, String> {
        let path = Self::path(home);
        match std::fs::read_to_string(&path) {
            Ok(text) => serde_json::from_str(&text)
                .map(Some)
                .map_err(|_| format!("{} ist ungültig", path.display())),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(format!("{} nicht lesbar: {error}", path.display())),
        }
    }

    /// Speichert atomar (0600).
    pub(crate) fn save(&mut self, home: &Path) -> Result<(), String> {
        ensure_admin_dir(home)?;
        self.updated = now_unix();
        let json = serde_json::to_vec_pretty(self).map_err(|_| "Konto nicht kodierbar".to_owned())?;
        write_private_atomic(&Self::path(home), &json)
    }

    /// Setzt Zähler und Sperre zurück.
    pub(crate) fn clear_failures(&mut self) {
        self.failed_count = 0;
        self.locked_until = 0;
    }

    /// Ist das Konto gerade gesperrt?
    pub(crate) fn is_locked(&self, now: u64) -> bool {
        self.locked_until > now
    }

    /// Verbucht einen Fehlversuch; ab 5 in Folge exponentielle Sperre
    /// (30 s · 2^(n-5), gedeckelt auf 1 h).
    pub(crate) fn record_failure(&mut self, now: u64) {
        self.failed_count = self.failed_count.saturating_add(1);
        self.last_failure = now;
        if self.failed_count >= 5 {
            let exp = (self.failed_count - 5).min(7);
            let wait = (30_u64 << exp).min(3600);
            self.locked_until = now + wait;
        }
    }
}
