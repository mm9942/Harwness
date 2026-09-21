//! Antwort-Cache der Web-Tools: Schlüssel, Eintragsformat, sicheres Lesen/Schreiben.
//!
//! # Verantwortung
//! Dieses Modul besitzt das on-disk-Format und die Dateioperationen des Caches
//! (F-035, F-062, F-138). Ob ein gelesener Eintrag **verwendet** werden darf,
//! entscheidet [`crate::fetch::WebFetcher`]: jede URL der gespeicherten
//! Redirect-Kette muss erneut [`crate::hop::check_hop`] bestehen.
//!
//! # Isolation
//! - **Schlüssel** ([`cache_key`]): BLAKE3 über Domänentrenner ‖
//!   `EgressPolicy::digest()` ‖ [`CacheScope`] ‖ normalisierte Anfrage-URL.
//!   Eine andere Policy, ein anderer Mandant/Workspace oder ein anderer
//!   Sandbox-Netz-Scope ergibt eine andere Datei — der Cache ist nicht mehr
//!   prozessweit geteilt.
//! - **Ort**: `<cache_dir>/web/<hex(key)>.json`; `cache_dir` ist ein
//!   Konstruktor-Parameter (vorgesehen: `harw_home::paths::cache_dir(home)`),
//!   es gibt keinen `/tmp`- oder Umgebungsvariablen-Fallback mehr.
//! - **Rechte**: Verzeichnis `0700`, Einträge `0600` über
//!   [`harw_fsutil::write_atomic`]. Gelesen wird nur über
//!   [`harw_fsutil::open_nofollow`] + [`harw_fsutil::ensure_private_regular`]:
//!   Symlinks, fremde Eigentümer und gruppen-/weltlesbare Dateien werden
//!   abgelehnt — ein anderer lokaler Nutzer kann keine Einträge unterschieben.
//! - **Format**: [`CACHE_FORMAT_VERSION`]; Einträge tragen ihren Schlüssel und
//!   ihre Anfrage-URL, [`entry_matches`] vergleicht beides.
//!
//! # Nebenläufigkeit
//! Blockierende Datei-I/O ohne Sperren; im async-Kontext über
//! [`crate::fetch::run_blocking`]. Konkurrierende Schreiber ersetzen die Datei
//! atomar (`rename`), Leser sehen alt oder neu, nie halb.
//!
//! # Fehler
//! [`crate::WebToolError::Io`], [`crate::WebToolError::Json`],
//! [`crate::WebToolError::CacheCorrupt`].
//!
//! # Examples
//! ```rust
//! use std::path::Path;
//! use harw_egress::EgressPolicy;
//! use harw_authority::NetworkScope;
//! use harw_tool_web::cache::{CacheScope, cache_key, cache_path};
//!
//! let policy = EgressPolicy::new(vec!["docs.rs".into()], false).unwrap();
//! let scope = CacheScope::new("tenant", "ws", &NetworkScope::from_hosts(["docs.rs".into()]));
//! let key = cache_key(&policy.digest(), &scope, "https://docs.rs/");
//! assert!(cache_path(Path::new("/home/u/.harw/cache"), &key).starts_with("/home/u/.harw/cache/web"));
//! ```

use crate::error::{WebToolError, WebToolResult};
use harw_fsutil::{AtomicWriteOptions, OpenMode, ensure_private_regular, open_nofollow, write_atomic};
use harw_authority::{EgressTarget, NetworkScope};
use harw_tools::ToolExecutionContext;
use serde::{Deserialize, Serialize};
use std::fs::{self, DirBuilder, Permissions};
use std::io::{self, ErrorKind, Read};
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Unterverzeichnis des Cache-Verzeichnisses, in dem Antworten liegen.
pub const CACHE_SUBDIR: &str = "web";

/// Version des on-disk-Formats; ältere Einträge gelten als beschädigt (Miss).
pub const CACHE_FORMAT_VERSION: u32 = 2;

/// Obergrenze der Dateigröße eines Eintrags beim Lesen (64 MiB).
///
/// Ein Körper ist höchstens [`crate::fetch::HARD_MAX_BYTES`] groß; JSON-Escapes
/// können ihn vervielfachen. Größere Dateien werden nicht eingelesen.
pub const MAX_CACHE_FILE_BYTES: u64 = 64 * 1_048_576;

/// Domänentrenner des Cache-Schlüssels.
const KEY_DOMAIN: &[u8] = b"harw:web-cache-key:v2\0";

/// Domänentrenner des Scope-Digests.
const SCOPE_DOMAIN: &[u8] = b"harw:web-cache-scope:v1\0";

/// Hex-Alphabet für Dateinamen und Schlüssel.
const HEX_DIGITS: &[u8; 16] = b"0123456789abcdef";

/// Isolationsbereich eines Cache-Eintrags.
///
/// # Description
/// Digest über Mandant, Workspace und die verlustfreie Zielliste des
/// Sandbox-[`NetworkScope`]. Zwei Aufrufe teilen Einträge nur, wenn alle drei
/// übereinstimmen.
///
/// # Concurrency
/// Reine Daten; `Send + Sync + Copy`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CacheScope {
    digest: [u8; 32],
}

impl CacheScope {
    /// Berechnet den Scope aus Mandant, Workspace und Netz-Scope.
    ///
    /// # Description
    /// Kodierung: `harw:web-cache-scope:v1\0` ‖ `lp(tenant)` ‖ `lp(workspace)` ‖
    /// `u64le(n)` ‖ Σ `lp(target)`, wobei `lp(x) = u64le(len) ‖ bytes` und
    /// `target` die Wire-Form von [`EgressTarget`] ist (`=host`, `suffix`,
    /// `cidr`). Die Ziele kommen aus [`NetworkScope::targets`] in stabiler
    /// Ordnung.
    ///
    /// # Arguments
    /// - `tenant` (`&str`): Mandanten-ID.
    /// - `workspace` (`&str`): Workspace-ID.
    /// - `network` (`&NetworkScope`): Netz-Scope der Sandbox.
    ///
    /// # Returns
    /// Den [`CacheScope`].
    ///
    /// # Concurrency
    /// Rein.
    ///
    /// # Examples
    /// ```rust
    /// use harw_authority::NetworkScope;
    /// use harw_tool_web::cache::CacheScope;
    ///
    /// let a = CacheScope::new("t1", "w", &NetworkScope::empty());
    /// let b = CacheScope::new("t2", "w", &NetworkScope::empty());
    /// assert_ne!(a, b);
    /// ```
    #[must_use]
    pub fn new(tenant: &str, workspace: &str, network: &NetworkScope) -> Self {
        let mut hasher = blake3::Hasher::new();
        hasher.update(SCOPE_DOMAIN);
        update_len_prefixed(&mut hasher, tenant.as_bytes());
        update_len_prefixed(&mut hasher, workspace.as_bytes());
        let targets: Vec<String> = network.targets().map(target_wire_form).collect();
        hasher.update(&len_le(targets.len()));
        for target in &targets {
            update_len_prefixed(&mut hasher, target.as_bytes());
        }
        Self {
            digest: hasher.finalize().into(),
        }
    }

    /// Leitet den Scope aus dem Tool-Kontext ab (Mandant, Workspace, Netz-Scope).
    ///
    /// # Arguments
    /// - `context` (`&ToolExecutionContext`): die vom Harness gesetzte Autorität.
    ///
    /// # Returns
    /// Den [`CacheScope`] dieses Aufrufs.
    ///
    /// # Concurrency
    /// Rein.
    #[must_use]
    pub fn from_context(context: &ToolExecutionContext) -> Self {
        let sandbox = context.sandbox();
        let binding = sandbox.workspace();
        Self::new(
            binding.tenant().as_str(),
            binding.workspace().as_str(),
            sandbox.network_scope(),
        )
    }

    /// Der Scope eines Fetchers ohne Sandbox-Bindung (erreicht nichts).
    #[must_use]
    pub fn unbound() -> Self {
        Self::new("", "", &NetworkScope::empty())
    }

    /// Die 32 Digest-Bytes.
    #[must_use]
    pub fn digest(&self) -> [u8; 32] {
        self.digest
    }
}

/// Wire-Form eines Egress-Ziels (identisch zur Serde-Form in `harw-sandbox`).
fn target_wire_form(target: &EgressTarget) -> String {
    match target {
        EgressTarget::Host(host) => format!("={host}"),
        EgressTarget::DnsSuffix(suffix) => suffix.to_owned(),
        EgressTarget::Cidr(net) => net.to_string(),
    }
}

/// Länge als `u64` little-endian.
fn len_le(len: usize) -> [u8; 8] {
    u64::try_from(len).unwrap_or(u64::MAX).to_le_bytes()
}

/// Schreibt `u64le(len) ‖ bytes` in den Hasher.
fn update_len_prefixed(hasher: &mut blake3::Hasher, bytes: &[u8]) {
    hasher.update(&len_le(bytes.len()));
    hasher.update(bytes);
}

/// Berechnet den Cache-Schlüssel.
///
/// # Description
/// BLAKE3 über `harw:web-cache-key:v2\0` ‖ `policy_digest` (32 B) ‖
/// `scope.digest()` (32 B) ‖ `lp(url)`. `url` ist die von
/// [`crate::hop::check_hop`] normalisierte Anfrage-URL.
///
/// # Arguments
/// - `policy_digest` (`&[u8; 32]`): [`harw_egress::EgressPolicy::digest`].
/// - `scope` (`&CacheScope`): Isolationsbereich des Aufrufs.
/// - `url` (`&str`): normalisierte Anfrage-URL.
///
/// # Returns
/// 32 Schlüsselbytes.
///
/// # Concurrency
/// Rein.
///
/// # Examples
/// ```rust
/// use harw_tool_web::cache::{CacheScope, cache_key};
///
/// let scope = CacheScope::unbound();
/// assert_ne!(cache_key(&[0; 32], &scope, "https://a/"), cache_key(&[1; 32], &scope, "https://a/"));
/// ```
#[must_use]
pub fn cache_key(policy_digest: &[u8; 32], scope: &CacheScope, url: &str) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(KEY_DOMAIN);
    hasher.update(policy_digest);
    hasher.update(&scope.digest);
    update_len_prefixed(&mut hasher, url.as_bytes());
    hasher.finalize().into()
}

/// Kleinbuchstaben-Hex einer Bytefolge.
///
/// # Arguments
/// - `bytes` (`&[u8]`): Eingabe.
///
/// # Returns
/// `2 * bytes.len()` Hex-Zeichen.
///
/// # Concurrency
/// Rein.
///
/// # Examples
/// ```rust
/// assert_eq!(harw_tool_web::cache::to_hex(&[0x0f, 0xa0]), "0fa0");
/// ```
#[must_use]
pub fn to_hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(char::from(HEX_DIGITS[usize::from(byte >> 4)]));
        out.push(char::from(HEX_DIGITS[usize::from(byte & 0x0f)]));
    }
    out
}

/// Das Unterverzeichnis, in dem Antworten liegen.
fn cache_subdir(base: &Path) -> PathBuf {
    base.join(CACHE_SUBDIR)
}

/// Der Pfad des Eintrags zu einem Schlüssel.
///
/// # Arguments
/// - `base` (`&Path`): Basis-Cache-Verzeichnis (unter `HARW_HOME`).
/// - `key` (`&[u8; 32]`): Ergebnis von [`cache_key`].
///
/// # Returns
/// `<base>/web/<hex(key)>.json`.
///
/// # Concurrency
/// Rein.
#[must_use]
pub fn cache_path(base: &Path, key: &[u8; 32]) -> PathBuf {
    cache_subdir(base).join(format!("{}.json", to_hex(key)))
}

/// On-disk-Format eines Cache-Eintrags.
///
/// # Description
/// Gespeichert wird der **Rohkörper** (bereits byte-gekappt), nicht die
/// aufbereitete Ausgabe. `chain` enthält jede gesendete URL in Reihenfolge:
/// `chain[0]` ist die normalisierte Anfrage-URL, das letzte Element das
/// Endziel. Beim Lesen muss jedes Element erneut die Egress-Prüfung bestehen.
///
/// # Concurrency
/// Reine Daten.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CacheEntry {
    /// Format-Version, muss [`CACHE_FORMAT_VERSION`] sein.
    pub format_version: u32,
    /// Hex des Schlüssels, unter dem der Eintrag geschrieben wurde.
    pub key: String,
    /// Alle gesendeten URLs, Anfrage zuerst, Endziel zuletzt.
    pub chain: Vec<String>,
    /// Der HTTP-Status der ursprünglichen Antwort.
    pub status: u16,
    /// Der `ETag` für den nächsten Conditional-GET.
    pub etag: Option<String>,
    /// Unix-Zeitstempel des Abrufs in Sekunden.
    pub fetched_at: u64,
    /// Der Medientyp ohne Parameter.
    pub content_type: String,
    /// Der Antwort-Körper (höchstens `max_bytes` Bytes vor dem Dekodieren).
    pub body: String,
}

impl CacheEntry {
    /// Das Endziel der gespeicherten Kette (`""` bei leerer Kette).
    #[must_use]
    pub fn final_url(&self) -> &str {
        self.chain.last().map_or("", String::as_str)
    }
}

/// Prüft, ob ein gelesener Eintrag formal zur Anfrage passt.
///
/// # Description
/// Format-Version, gespeicherter Schlüssel und `chain[0]` müssen exakt
/// übereinstimmen. Die Egress-Prüfung der Kette ist Sache des Aufrufers.
///
/// # Arguments
/// - `entry` (`&CacheEntry`): gelesener Eintrag.
/// - `key_hex` (`&str`): erwarteter Schlüssel als Hex.
/// - `request_url` (`&str`): normalisierte Anfrage-URL.
///
/// # Returns
/// `true`, wenn der Eintrag formal passt.
///
/// # Concurrency
/// Rein.
#[must_use]
pub fn entry_matches(entry: &CacheEntry, key_hex: &str, request_url: &str) -> bool {
    entry.format_version == CACHE_FORMAT_VERSION
        && entry.key == key_hex
        && entry.chain.first().is_some_and(|first| first == request_url)
}

/// Aktuelle Unix-Zeit in Sekunden; vor der Epoche `0`.
#[must_use]
pub fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|delta| delta.as_secs())
        .unwrap_or(0)
}

/// Sagt, ob ein Eintrag jünger als die TTL ist.
///
/// # Description
/// Ein Zeitstempel in der Zukunft gilt als frisch, statt eine Neuabruf-Schleife
/// auszulösen.
///
/// # Arguments
/// - `entry` (`&CacheEntry`): der zu bewertende Eintrag.
/// - `ttl` ([`Duration`]): die zulässige Lebensdauer.
///
/// # Returns
/// `true`, wenn der Eintrag ohne Netzabruf verwendet werden darf.
///
/// # Concurrency
/// Rein.
#[must_use]
pub fn entry_is_fresh(entry: &CacheEntry, ttl: Duration) -> bool {
    now_secs().saturating_sub(entry.fetched_at) < ttl.as_secs()
}

/// Liest einen Cache-Eintrag, falls vorhanden.
///
/// # Description
/// Öffnet ohne Symlink-Folgen ([`open_nofollow`]), verlangt eine reguläre
/// Datei des eigenen Nutzers ohne Gruppen-/Weltrechte
/// ([`ensure_private_regular`]) und liest höchstens
/// [`MAX_CACHE_FILE_BYTES`] Bytes. Ein fehlender Eintrag ist `Ok(None)`.
///
/// # Arguments
/// - `path` (`&Path`): Pfad aus [`cache_path`].
///
/// # Returns
/// `Some(CacheEntry)` bei einem syntaktisch gültigen Eintrag.
///
/// # Errors
/// - [`WebToolError::Io`]: Symlink, FIFO, fremder Eigentümer, zu offene Rechte
///   oder Lesefehler.
/// - [`WebToolError::CacheCorrupt`]: zu groß oder kein gültiger Eintrag.
///
/// # Concurrency
/// Blockierend; über [`crate::fetch::run_blocking`] aufrufen.
pub fn read_cache_entry(path: &Path) -> WebToolResult<Option<CacheEntry>> {
    let file = match open_nofollow(path, OpenMode::read_only()) {
        Ok(file) => file,
        Err(source) if source.kind() == ErrorKind::NotFound => return Ok(None),
        Err(source) => return Err(WebToolError::Io(source)),
    };
    ensure_private_regular(&file)?;

    let mut raw = Vec::new();
    file.take(MAX_CACHE_FILE_BYTES.saturating_add(1))
        .read_to_end(&mut raw)?;
    let corrupt = || WebToolError::CacheCorrupt {
        path: path.display().to_string(),
    };
    if u64::try_from(raw.len()).unwrap_or(u64::MAX) > MAX_CACHE_FILE_BYTES {
        return Err(corrupt());
    }
    serde_json::from_slice::<CacheEntry>(&raw)
        .map(Some)
        .map_err(|_| corrupt())
}

/// Legt `<base>/web` mit `0700` an und prüft es.
///
/// # Description
/// Fehlende Verzeichnisse werden mit `0700` erzeugt. Ein Symlink oder
/// Nicht-Verzeichnis wird abgelehnt; zu offene Rechte werden auf `0700`
/// zurückgesetzt (scheitert, wenn das Verzeichnis einem anderen Nutzer gehört).
///
/// # Errors
/// - [`WebToolError::Io`]: Anlegen, Prüfen oder `chmod` scheiterte.
fn ensure_cache_dir(base: &Path) -> WebToolResult<PathBuf> {
    let directory = cache_subdir(base);
    DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&directory)?;
    let meta = fs::symlink_metadata(&directory)?;
    if meta.file_type().is_symlink() || !meta.is_dir() {
        return Err(WebToolError::Io(io::Error::new(
            ErrorKind::InvalidInput,
            "web cache directory is not a real directory",
        )));
    }
    if meta.permissions().mode() & 0o077 != 0 {
        fs::set_permissions(&directory, Permissions::from_mode(0o700))?;
    }
    Ok(directory)
}

/// Schreibt einen Cache-Eintrag atomar mit Rechten `0600`.
///
/// # Description
/// Stellt `<base>/web` (`0700`) sicher, serialisiert nach JSON und ersetzt
/// `path` über [`write_atomic`] (Tempdatei, `fsync`, `rename`; ein
/// Ziel-Symlink wird ersetzt, nie gefolgt). `path` muss in `<base>/web`
/// liegen.
///
/// # Arguments
/// - `base` (`&Path`): Basis-Cache-Verzeichnis.
/// - `path` (`&Path`): Zielpfad aus [`cache_path`].
/// - `entry` (`&CacheEntry`): der Eintrag.
///
/// # Returns
/// `Ok(())`, wenn der Eintrag ersetzt wurde.
///
/// # Errors
/// - [`WebToolError::Io`][]: Verzeichnis, Pfad außerhalb von `<base>/web` oder
///   Schreiben.
/// - [`WebToolError::Json`][]: Serialisierung.
///
/// # Concurrency
/// Blockierend; über [`crate::fetch::run_blocking`] aufrufen.
pub fn write_cache_entry(base: &Path, path: &Path, entry: &CacheEntry) -> WebToolResult<()> {
    let directory = ensure_cache_dir(base)?;
    if path.parent() != Some(directory.as_path()) {
        return Err(WebToolError::Io(io::Error::new(
            ErrorKind::InvalidInput,
            "web cache entry path is outside the cache directory",
        )));
    }
    let payload = serde_json::to_vec(entry)?;
    write_atomic(path, &payload, AtomicWriteOptions { mode: 0o600, fsync_dir: false })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use harw_egress::EgressPolicy;
    use tempfile::TempDir;

    fn sample_entry(key_hex: &str, url: &str) -> CacheEntry {
        CacheEntry {
            format_version: CACHE_FORMAT_VERSION,
            key: key_hex.to_owned(),
            chain: vec![url.to_owned()],
            status: 200,
            etag: Some("\"abc\"".to_owned()),
            fetched_at: now_secs(),
            content_type: "text/html".to_owned(),
            body: "<p>x</p>".to_owned(),
        }
    }

    fn policy(hosts: &[&str], allow_private: bool) -> EgressPolicy {
        let hosts = hosts.iter().map(|host| (*host).to_owned()).collect();
        EgressPolicy::new(hosts, allow_private).expect("gültige Policy")
    }

    /// F-035: der Schlüssel hängt am Policy-Digest.
    #[test]
    fn test_cache_key_changes_with_policy_digest() {
        let scope = CacheScope::new("t", "w", &NetworkScope::from_hosts(["docs.rs".into()]));
        let url = "https://docs.rs/serde/";
        let strict = policy(&["docs.rs"], false);
        let reordered = policy(&["crates.io", "DOCS.rs"], false);
        let wider = policy(&["docs.rs", "crates.io"], false);
        let private = policy(&["docs.rs"], true);

        let base = cache_key(&strict.digest(), &scope, url);
        assert_ne!(base, cache_key(&wider.digest(), &scope, url));
        assert_ne!(base, cache_key(&private.digest(), &scope, url));
        assert_eq!(
            cache_key(&wider.digest(), &scope, url),
            cache_key(&reordered.digest(), &scope, url),
            "Normalisierung der Policy darf den Schlüssel nicht ändern"
        );
        assert_eq!(base, cache_key(&strict.digest(), &scope, url));
    }

    /// F-035: der Schlüssel hängt an Mandant, Workspace, Netz-Scope und URL.
    #[test]
    fn test_cache_key_changes_with_scope_and_url() {
        let digest = policy(&["docs.rs"], false).digest();
        let net = NetworkScope::from_hosts(["docs.rs".into()]);
        let url = "https://docs.rs/";
        let base = cache_key(&digest, &CacheScope::new("t", "w", &net), url);

        assert_ne!(base, cache_key(&digest, &CacheScope::new("t2", "w", &net), url));
        assert_ne!(base, cache_key(&digest, &CacheScope::new("t", "w2", &net), url));
        assert_ne!(
            base,
            cache_key(&digest, &CacheScope::new("t", "w", &NetworkScope::empty()), url)
        );
        assert_ne!(base, cache_key(&digest, &CacheScope::new("t", "w", &net), "https://docs.rs/x"));
        // Längenpräfixe: ("ab","c") und ("a","bc") kollidieren nicht.
        assert_ne!(CacheScope::new("ab", "c", &net), CacheScope::new("a", "bc", &net));
    }

    /// Pfad liegt unter `<base>/web` und ist 64 Hex-Zeichen lang.
    #[test]
    fn test_cache_path_is_hex_json_below_web_subdir() {
        let path = cache_path(Path::new("/h/.harw/cache"), &[0xab; 32]);
        assert!(path.starts_with("/h/.harw/cache/web"));
        let stem = path.file_stem().and_then(|s| s.to_str()).expect("Name");
        assert_eq!(stem, "ab".repeat(32));
    }

    /// Schreiben und Lesen sind invers; Rechte sind privat.
    #[test]
    fn test_write_then_read_cache_entry_round_trips_with_private_modes() {
        let dir = TempDir::new().expect("Tempdir");
        let key = [7u8; 32];
        let path = cache_path(dir.path(), &key);
        let entry = sample_entry(&to_hex(&key), "https://docs.rs/");

        write_cache_entry(dir.path(), &path, &entry).expect("schreiben");
        let loaded = read_cache_entry(&path).expect("lesen").expect("vorhanden");
        assert_eq!(loaded, entry);

        let file_mode = fs::metadata(&path).expect("meta").permissions().mode() & 0o777;
        assert_eq!(file_mode, 0o600);
        let dir_mode = fs::metadata(cache_subdir(dir.path()))
            .expect("meta")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(dir_mode, 0o700);
    }

    /// Fehlender Eintrag ist kein Fehler.
    #[test]
    fn test_read_cache_entry_missing_file_is_none() {
        let dir = TempDir::new().expect("Tempdir");
        let path = cache_path(dir.path(), &[1; 32]);
        assert_eq!(read_cache_entry(&path).expect("kein Fehler"), None);
    }

    /// F-062: ein gruppen-/weltlesbarer (untergeschobener) Eintrag wird abgelehnt.
    #[test]
    fn test_read_cache_entry_rejects_non_private_file() {
        let dir = TempDir::new().expect("Tempdir");
        let key = [2u8; 32];
        let path = cache_path(dir.path(), &key);
        let entry = sample_entry(&to_hex(&key), "https://docs.rs/");
        fs::create_dir_all(cache_subdir(dir.path())).expect("dir");
        fs::write(&path, serde_json::to_vec(&entry).expect("json")).expect("write");
        fs::set_permissions(&path, Permissions::from_mode(0o644)).expect("chmod");

        let err = read_cache_entry(&path).expect_err("0644 muss abgelehnt werden");
        assert!(matches!(err, WebToolError::Io(_)), "{err:?}");
    }

    /// Ein symlinkter Eintrag wird nicht gelesen.
    #[test]
    fn test_read_cache_entry_rejects_symlink() {
        use std::os::unix::fs::symlink;

        let dir = TempDir::new().expect("Tempdir");
        let path = cache_path(dir.path(), &[3; 32]);
        fs::create_dir_all(cache_subdir(dir.path())).expect("dir");
        let outside = dir.path().join("fremd.json");
        fs::write(&outside, "{}").expect("write");
        symlink(&outside, &path).expect("symlink");

        let err = read_cache_entry(&path).expect_err("Symlink");
        assert!(matches!(err, WebToolError::Io(_)), "{err:?}");
    }

    /// Kaputtes JSON und alte Formate melden `CacheCorrupt`.
    #[test]
    fn test_read_cache_entry_reports_corrupt_and_legacy_content() {
        let dir = TempDir::new().expect("Tempdir");
        fs::create_dir_all(cache_subdir(dir.path())).expect("dir");
        for (index, content) in [
            "{ kein json",
            r#"{"url":"https://docs.rs/","etag":null,"fetched_at":0,"content_type":"text/html","body":""}"#,
        ]
        .into_iter()
        .enumerate()
        {
            let path = cache_path(dir.path(), &[u8::try_from(index).unwrap_or(0) + 10; 32]);
            write_atomic(&path, content.as_bytes(), AtomicWriteOptions::private()).expect("write");
            let err = read_cache_entry(&path).expect_err("korrupt");
            assert!(matches!(err, WebToolError::CacheCorrupt { .. }), "{err:?}");
        }
    }

    /// Ein Ziel außerhalb von `<base>/web` wird nicht beschrieben.
    #[test]
    fn test_write_cache_entry_rejects_path_outside_cache_dir() {
        let dir = TempDir::new().expect("Tempdir");
        let outside = dir.path().join("x.json");
        let entry = sample_entry("00", "https://docs.rs/");
        let err = write_cache_entry(dir.path(), &outside, &entry).expect_err("außerhalb");
        assert!(matches!(err, WebToolError::Io(_)), "{err:?}");
        assert!(!outside.exists());
    }

    /// Zu offene Verzeichnisrechte werden auf 0700 zurückgesetzt.
    #[test]
    fn test_write_cache_entry_tightens_cache_dir_mode() {
        let dir = TempDir::new().expect("Tempdir");
        let web = cache_subdir(dir.path());
        fs::create_dir_all(&web).expect("dir");
        fs::set_permissions(&web, Permissions::from_mode(0o755)).expect("chmod");
        let key = [4u8; 32];
        let entry = sample_entry(&to_hex(&key), "https://docs.rs/");
        write_cache_entry(dir.path(), &cache_path(dir.path(), &key), &entry).expect("schreiben");
        let mode = fs::metadata(&web).expect("meta").permissions().mode() & 0o777;
        assert_eq!(mode, 0o700);
    }

    /// `entry_matches` verlangt Version, Schlüssel und Anfrage-URL.
    #[test]
    fn test_entry_matches_requires_version_key_and_request_url() {
        let entry = sample_entry("aa", "https://docs.rs/");
        assert!(entry_matches(&entry, "aa", "https://docs.rs/"));
        assert!(!entry_matches(&entry, "bb", "https://docs.rs/"));
        assert!(!entry_matches(&entry, "aa", "https://docs.rs/x"));
        let mut legacy = entry.clone();
        legacy.format_version = 1;
        assert!(!entry_matches(&legacy, "aa", "https://docs.rs/"));
        let mut empty = entry;
        empty.chain.clear();
        assert!(!entry_matches(&empty, "aa", "https://docs.rs/"));
        assert_eq!(empty.final_url(), "");
    }

    /// TTL-Logik inklusive Zukunftszeitstempel.
    #[test]
    fn test_entry_is_fresh_respects_ttl_and_future_timestamps() {
        let mut entry = sample_entry("aa", "https://docs.rs/");
        assert!(entry_is_fresh(&entry, Duration::from_secs(3_600)));
        entry.fetched_at = now_secs().saturating_sub(7_200);
        assert!(!entry_is_fresh(&entry, Duration::from_secs(3_600)));
        entry.fetched_at = now_secs().saturating_add(600);
        assert!(entry_is_fresh(&entry, Duration::from_secs(60)));
    }
}
