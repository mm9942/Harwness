//! `[dod]` — Grundsatz-Policy für die Harw-DoD-Kette (Detect · Orient ·
//! Defend: Sensorik, Eskalation, Warden) (Remediationsplan Teil B, Welle W3, AP `C-CFG`;
//! Konsumenten: `harw-dod-escalate`/`harw-escalator` `D-ESC`,
//! `harw-dod-warden`/`harw-warden` `D-WARDEN`, `harw-dod-sentinel` `D-SENTLIB`).
//!
//! Dieses Modul definiert ausschließlich die deklarative Policy, unter der
//! die DoD-Kette eskalieren darf: ob ein Kill-Kommando ausschließlich mit
//! menschlicher Freigabe ausgeführt werden darf
//! ([`DodSection::kill_requires_human`] — laut Plan-Entscheidung "Warden-
//! Automatik: Freeze/Release automatisch nach signiertem Proof; Kill nur mit
//! menschlicher Freigabe (Default aus)" **nicht verhandelbar**, siehe
//! [`DodSection::validate`]), ob Freeze/Release-Übergänge automatisch nach
//! einem signierten Proof erfolgen ([`DodSection::auto_freeze`]), wo die
//! Schlüssel für `SignedAuthorization`-Prüfung liegen
//! ([`DodSection::proof_key_dir`]), und welche cgroup-Pfadpräfixe die
//! Eskalationskette überhaupt adressieren darf
//! ([`DodSection::allowed_cgroup_prefixes`] — Design "Ziel-cgroup aus Record
//! + Präfix"/"cgroup-Präfix-Allowlist"). Durchsetzung liegt vollständig beim
//!   Consumer (Escalator/Warden-Binaries); dieses Modul beschreibt nur die
//!   Konfiguration und deren Invarianten.
//!
//! Two read-side keys (PL-97 D1) tell harw where to read the sentinel's
//! exports and how far back to summarize findings:
//! [`DodSection::export_dir`] (explicit export directory; when unset the
//! reader falls back to `/var/lib/harw-dod/export`, then to
//! `<HARW home>/dod/export`) and [`DodSection::findings_window_hours`]
//! (default [`DEFAULT_FINDINGS_WINDOW_HOURS`]). Both are profile-scoped:
//! only trusted layers (home, active profile) set them. An untrusted
//! repository layer can set neither — `export_dir` for the same reason as
//! `proof_key_dir` (redirecting harw to an attacker-chosen directory would
//! let the repository fabricate posture), `findings_window_hours` because
//! the restricted merge only copies keys it explicitly narrows.
//!
//! Defaults sind bewusst restriktiv (`Default = sicher/aus`):
//! `kill_requires_human = true`, `auto_freeze = true`, leere
//! cgroup-Präfixliste. Ein nicht vertrauter Repo-Layer darf diese Sektion
//! nur verengen
//! (`allowed_cgroup_prefixes` nur Schnittmenge, `auto_freeze` nur Richtung
//! `true`); `kill_requires_human` ist ohnehin in jeder Layer fest auf `true`
//! erzwungen (siehe unten) und `proof_key_dir` wird aus einem Repo-Layer
//! **nie** übernommen (Umlenkung auf einen fremden Schlüsselordner wäre eine
//! Rechteausweitung); ebenso `export_dir` und `findings_window_hours`. Die
//! Merge-Logik lebt in `harw_config::discovery`, nicht hier.
//!
//! Drei Untertabellen (PL-100 WP 1.9) steuern die KI-Seite der Kette:
//! - [`DodTriageSection`] (`[dod.triage]`): Triage-Modell
//!   `"<provider>/<model>"` ([`parse_triage_model`]), Reasoning-Effort
//!   (kanonische Namen von `harw_types::ReasoningEffort`), `max_tokens`
//!   (`1..=`[`MAX_TRIAGE_TOKENS`]) und `max_parallel`
//!   (`1..=`[`MAX_TRIAGE_PARALLEL`], Vorgabe
//!   [`DEFAULT_TRIAGE_MAX_PARALLEL`]).
//! - [`DodApproveSection`] (`[dod.approve]`): `tui_tty` (Vorgabe `true`,
//!   Entscheidung O12). Der Same-TTY-Modus deckt nur die umkehrbaren
//!   Eingriffe Freeze und Thaw ab; Kill und Isolation bleiben dort gesperrt.
//! - [`DodWatchSection`] (`[dod.watch]`): Watch-Modus ([`DodWatchMode`],
//!   Vorgabe `triage`) und `autoconnect` der User-Unit `harw-dod-watch`
//!   zum Gateway (Vorgabe `true`, Entscheidung O5).
//!
//! `[dod.triage]` und `[dod.approve]` gelten nur aus der Home-Ebene und dem
//! aktiven Profil (Bedrohung B15). Die Config-Discovery verwirft sie aus
//! jeder anderen Ebene (vertrauter Repo-Layer, projektbezogene
//! Einstellungen, nicht vertrauter Repo-Layer) und meldet das als
//! `ScopeDiagnostic`; einzige Ausnahme ist die Verengung `[dod.approve]
//! tui_tty = false`, die aus jeder Ebene wirkt (B17). Einen nicht vertrauten
//! Repo-Layer übernimmt sie auch für `[dod.watch]` nie. Ohne Triage-Modell
//! läuft keine automatische Triage
//! ([`DodSection::effective_watch_mode`], Invariante 7).
//!
//! Keine Nebenläufigkeit: reiner Datentyp, `Send + Sync` über `derive`.
//! Fehler: [`DodSection::validate`] liefert `Result<(), String>` (gleiches
//! Muster wie `research_toml::ResearchSection::validate`).
//!
//! Deserializing a `[dod]` table runs [`DodSection::validate`] (serde
//! `try_from` over a private raw shape), so the invariants hold for every
//! value that config discovery produces: a `[dod]` table in any layer —
//! trusted or not — with `kill_requires_human = false`, a blank cgroup
//! prefix, a relative `export_dir` or `findings_window_hours = 0` makes
//! `discover_config` fail instead of loading. A section built in code
//! (struct literal) bypasses that check; call [`DodSection::validate`]
//! yourself there.
//!
//! # Examples
//! ```rust
//! use harw_config::DodSection;
//!
//! let section: DodSection = toml::from_str("").unwrap();
//! assert!(section.kill_requires_human);
//! assert_eq!(section.export_dir, None);
//! assert_eq!(section.findings_window_hours, 24);
//! assert_eq!(section.triage.model, None);
//! assert!(section.approve.tui_tty);
//! assert_eq!(section.watch.mode.as_str(), "triage");
//! assert!(section.validate().is_ok());
//! ```

use crate::serde_defaults::default_true;
use std::path::PathBuf;

use harw_types::ReasoningEffort;
use serde::{Deserialize, Serialize};

/// Mount point of the cgroup v2 hierarchy: the canonical
/// [`DodSection::allowed_cgroup_prefixes`] form is an absolute path strictly
/// below it (PL-97 E3; the same contract as
/// `harw_job_runtime::host::CGROUP2_MOUNT`).
pub const CGROUP2_MOUNT: &str = "/sys/fs/cgroup";

/// Converts a canonical `allowed_cgroup_prefixes` entry (absolute, strictly
/// below [`CGROUP2_MOUNT`], optional trailing `/`) to the Warden's relative
/// `CgroupId` form (`/sys/fs/cgroup/user.slice/harw.slice/` →
/// `user.slice/harw.slice`). `None` for any other form (relative paths,
/// `.`/`..` segments, paths outside the mount, the mount itself), which
/// [`DodSection::validate`] rejects. Wiring the converted prefixes into the
/// Warden's `ProofPolicy` is not done here (PL-96 W01).
#[must_use]
pub fn warden_cgroup_id(prefix: &str) -> Option<String> {
    let rest = prefix
        .trim()
        .strip_prefix(CGROUP2_MOUNT)?
        .strip_prefix('/')?;
    let rest = rest.trim_end_matches('/');
    let valid = !rest.is_empty()
        && rest
            .split('/')
            .all(|segment| !segment.is_empty() && segment != "." && segment != "..");
    valid.then(|| rest.to_owned())
}

/// Default of [`DodSection::findings_window_hours`]: one day.
pub const DEFAULT_FINDINGS_WINDOW_HOURS: u32 = 24;

/// Serde default for [`DodSection::findings_window_hours`].
const fn default_findings_window_hours() -> u32 {
    DEFAULT_FINDINGS_WINDOW_HOURS
}

/// `[dod]` — Eskalations-Policy für die DoD-Kette: Kill-Freigabe,
/// Auto-Freeze, Schlüsselverzeichnis für Warden-Proofs und erlaubte
/// cgroup-Präfixe; dazu die beiden Leseschlüssel `export_dir` und
/// `findings_window_hours` (PL-97 D1).
///
/// Deserialization goes through a private raw shape and then
/// [`Self::validate`]; an invalid `[dod]` table never deserializes (see the
/// module docs).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "DodSectionToml")]
pub struct DodSection {
    /// Muss laut Plan-Entscheidung ("Kill nur mit menschlicher Freigabe")
    /// immer `true` sein. Nicht verhandelbar: [`Self::validate`] lehnt
    /// `false` in jeder Layer ab, unabhängig davon, ob sie vertraut ist.
    pub kill_requires_human: bool,
    /// Erlaubt automatisches Freeze/Release, sobald ein gültiger,
    /// signierter `WardenRequest`-Proof vorliegt. `true` (Default) ist die
    /// sichere Voreinstellung (siehe Modul-Doku); Abschalten reduziert den
    /// Automatisierungsgrad der DoD-Kette, nicht ihre Sicherheit — daher darf
    /// ein Repo-Layer diesen Wert nur *anheben*, nie absenken.
    pub auto_freeze: bool,
    /// Verzeichnis, aus dem der Warden die `ProofKey`-Materialien liest.
    /// `None` (Default) heißt: Consumer nutzt seinen eigenen fest
    /// verdrahteten Standardpfad.
    pub proof_key_dir: Option<PathBuf>,
    /// cgroup-Pfadpräfixe, auf die Eskalationsaktionen (Freeze/Release/Kill)
    /// beschränkt sind. Leer (Default) heißt: keine cgroup adressierbar, bis
    /// der Consumer explizit Präfixe erhält.
    pub allowed_cgroup_prefixes: Vec<String>,
    /// Explicit directory holding the sentinel exports (`findings.jsonl`,
    /// `health.json`). `None` (default): the reader resolves
    /// `/var/lib/harw-dod/export` if readable, else `<HARW home>/dod/export`.
    /// Must be absolute ([`Self::validate`]). Profile scope; never taken from
    /// an untrusted repository layer (same rule as `proof_key_dir`).
    pub export_dir: Option<PathBuf>,
    /// Window in hours over which harw summarizes exported findings
    /// (status line, `harw dod status`). Default
    /// [`DEFAULT_FINDINGS_WINDOW_HOURS`]; `0` is rejected by
    /// [`Self::validate`]. Profile scope; not taken from an untrusted
    /// repository layer.
    pub findings_window_hours: u32,
    /// `[dod.triage]` — Modell und Grenzen der automatischen Triage
    /// (PL-100 §3.2). Nur aus Home-Ebene und aktivem Profil (siehe
    /// Modul-Doku).
    pub triage: DodTriageSection,
    /// `[dod.approve]` — Freigabe im TTY der TUI (O12, nur Freeze/Thaw).
    /// Nur aus Home-Ebene und aktivem Profil (siehe Modul-Doku).
    pub approve: DodApproveSection,
    /// `[dod.watch]` — Watch-Modus und Gateway-Autoconnect der User-Unit
    /// `harw-dod-watch` (O5). Nie aus einem nicht vertrauten Repo-Layer.
    pub watch: DodWatchSection,
}

/// Raw TOML shape of `[dod]`: the same keys, defaults and
/// `deny_unknown_fields` as [`DodSection`], but without its invariants.
/// Only used as the serde `try_from` source of [`DodSection`], so that
/// deserialization cannot skip [`DodSection::validate`].
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DodSectionToml {
    #[serde(default = "default_true")]
    kill_requires_human: bool,
    #[serde(default = "default_true")]
    auto_freeze: bool,
    #[serde(default)]
    proof_key_dir: Option<PathBuf>,
    #[serde(default)]
    allowed_cgroup_prefixes: Vec<String>,
    #[serde(default)]
    export_dir: Option<PathBuf>,
    #[serde(default = "default_findings_window_hours")]
    findings_window_hours: u32,
    #[serde(default)]
    triage: DodTriageSection,
    #[serde(default)]
    approve: DodApproveSection,
    #[serde(default)]
    watch: DodWatchSection,
}

impl TryFrom<DodSectionToml> for DodSection {
    type Error = String;

    /// Builds the section from its raw TOML shape and checks
    /// [`DodSection::validate`].
    ///
    /// # Errors
    /// The reason from [`DodSection::validate`] when an invariant is
    /// violated.
    fn try_from(raw: DodSectionToml) -> Result<Self, Self::Error> {
        let DodSectionToml {
            kill_requires_human,
            auto_freeze,
            proof_key_dir,
            allowed_cgroup_prefixes,
            export_dir,
            findings_window_hours,
            triage,
            approve,
            watch,
        } = raw;
        let section = Self {
            kill_requires_human,
            auto_freeze,
            proof_key_dir,
            allowed_cgroup_prefixes,
            export_dir,
            findings_window_hours,
            triage,
            approve,
            watch,
        };
        section.validate()?;
        Ok(section)
    }
}

impl Default for DodSection {
    fn default() -> Self {
        Self {
            kill_requires_human: true,
            auto_freeze: true,
            proof_key_dir: None,
            allowed_cgroup_prefixes: Vec::new(),
            export_dir: None,
            findings_window_hours: DEFAULT_FINDINGS_WINDOW_HOURS,
            triage: DodTriageSection::default(),
            approve: DodApproveSection::default(),
            watch: DodWatchSection::default(),
        }
    }
}

impl DodSection {
    /// Prüft Invarianten, die reine Deserialisierung nicht ausdrücken kann.
    ///
    /// # Errors
    /// Liefert `Err(String)` mit einer menschenlesbaren Begründung, wenn
    /// `kill_requires_human == false` (in jeder Layer, unabhängig von
    /// Vertrauensstatus — Kill ohne menschliche Freigabe ist per
    /// Plan-Entscheidung nie zulässig), oder wenn `allowed_cgroup_prefixes`
    /// einen leeren Eintrag enthält; also when `export_dir` is not an
    /// absolute path or `findings_window_hours` is `0`; dazu jede Verletzung
    /// von [`DodTriageSection::validate`].
    pub fn validate(&self) -> Result<(), String> {
        if !self.kill_requires_human {
            return Err(
                "dod.kill_requires_human muss immer true sein (Warden-Kill nur mit \
                 menschlicher Freigabe)"
                    .to_owned(),
            );
        }
        for prefix in &self.allowed_cgroup_prefixes {
            if prefix.trim().is_empty() {
                return Err("dod.allowed_cgroup_prefixes enthält einen leeren Eintrag".to_owned());
            }
            if warden_cgroup_id(prefix).is_none() {
                return Err(format!(
                    "dod.allowed_cgroup_prefixes entries must be absolute paths below \
                     {CGROUP2_MOUNT} (canonical form, e.g. {CGROUP2_MOUNT}/harw.slice), got {prefix:?}"
                ));
            }
        }
        if let Some(dir) = &self.export_dir
            && !dir.is_absolute()
        {
            return Err(format!(
                "dod.export_dir must be an absolute path, got {}",
                dir.display()
            ));
        }
        if self.findings_window_hours == 0 {
            return Err("dod.findings_window_hours must be at least 1".to_owned());
        }
        self.triage.validate()
    }

    /// Wirksamer Watch-Modus unter Invariante 7 (THREAT-MODEL, README §3.2).
    ///
    /// # Description
    /// `triage` ohne gesetztes [`DodTriageSection::model`] fällt auf
    /// [`DodWatchMode::Notify`] zurück: ohne Triage-Modell läuft keine
    /// automatische Triage, und es gibt keinen stillen Rückfall auf das
    /// Session-Modell. Alle anderen Modi bleiben unverändert.
    ///
    /// # Returns
    /// Den Modus, mit dem `harw-dod-watch` tatsächlich arbeiten darf.
    ///
    /// # Examples
    /// ```rust
    /// use harw_config::{DodSection, DodWatchMode};
    ///
    /// let mut section = DodSection::default();
    /// assert_eq!(section.effective_watch_mode(), DodWatchMode::Notify);
    /// section.triage.model = Some("local/qwen2.5".to_owned());
    /// assert_eq!(section.effective_watch_mode(), DodWatchMode::Triage);
    /// ```
    #[must_use]
    pub fn effective_watch_mode(&self) -> DodWatchMode {
        match self.watch.mode {
            DodWatchMode::Triage if self.triage.model.is_none() => DodWatchMode::Notify,
            mode => mode,
        }
    }
}

/// Vorgabe von [`DodTriageSection::max_parallel`]: zwei gleichzeitige
/// Triage-Läufe.
pub const DEFAULT_TRIAGE_MAX_PARALLEL: u8 = 2;

/// Obergrenze von [`DodTriageSection::max_parallel`] (Untergrenze `1`).
pub const MAX_TRIAGE_PARALLEL: u8 = 8;

/// Obergrenze von [`DodTriageSection::max_tokens`] (Untergrenze `1`).
pub const MAX_TRIAGE_TOKENS: u32 = 32_768;

/// Längste zulässige Modellreferenz in [`DodTriageSection::model`], in
/// Bytes.
pub const MAX_TRIAGE_MODEL_BYTES: usize = 256;

/// Serde default for [`DodTriageSection::max_parallel`].
const fn default_triage_max_parallel() -> u8 {
    DEFAULT_TRIAGE_MAX_PARALLEL
}

/// Zerlegt eine Triage-Modellreferenz `"<provider>/<model>"` in Provider
/// und Modell.
///
/// # Description
/// Getrennt wird am ersten `/`; der Modellteil darf selbst `/` enthalten
/// (`openrouter/openai/gpt-4o` → `("openrouter", "openai/gpt-4o")`), aber
/// kein leeres `/`-Segment und kein Segment `.` oder `..` (falls ein
/// Konsument Provider oder Modell je in einen Pfad einsetzt). Der Provider
/// besteht aus ASCII-Buchstaben, ASCII-Ziffern, `-`, `_` und `.` und ist
/// weder `.` noch `..`; die ganze Referenz aus druckbarem ASCII
/// ohne Leerzeichen (keine Steuer-, Bidi- oder Formatzeichen, die in TUI
/// oder Doctor-Anzeige landen könnten), höchstens
/// [`MAX_TRIAGE_MODEL_BYTES`] Bytes. Ob Provider und Modell im Katalog
/// existieren, prüft diese Funktion nicht.
///
/// # Arguments
/// - `model` (`&str`): der Wert von `[dod.triage] model`.
///
/// # Returns
/// `(provider, model)` als Teilstücke von `model`.
///
/// # Errors
/// `Err(String)` mit einer Begründung, die `dod.triage.model` nennt, wenn
/// die Form nicht passt.
///
/// # Examples
/// ```rust
/// use harw_config::dod_toml::parse_triage_model;
///
/// assert_eq!(
///     parse_triage_model("openrouter/openai/gpt-4o"),
///     Ok(("openrouter", "openai/gpt-4o"))
/// );
/// assert!(parse_triage_model("noslash").is_err());
/// assert!(parse_triage_model("provider/").is_err());
/// ```
pub fn parse_triage_model(model: &str) -> Result<(&str, &str), String> {
    const SHAPE: &str = "expected \"<provider>/<model>\"";
    if model.len() > MAX_TRIAGE_MODEL_BYTES {
        return Err(format!(
            "dod.triage.model is longer than {MAX_TRIAGE_MODEL_BYTES} bytes"
        ));
    }
    if model.is_empty() || !model.bytes().all(|byte| byte.is_ascii_graphic()) {
        return Err(format!(
            "dod.triage.model must be printable ASCII without whitespace ({SHAPE})"
        ));
    }
    let Some((provider, name)) = model.split_once('/') else {
        return Err(format!(
            "dod.triage.model names no provider ({SHAPE}), got {model:?}"
        ));
    };
    let provider_ok = !is_dot_segment(provider)
        && !provider.is_empty()
        && provider
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'));
    if !provider_ok {
        return Err(format!(
            "dod.triage.model has an invalid provider ({SHAPE}), got {model:?}"
        ));
    }
    if name.split('/').any(str::is_empty) {
        return Err(format!(
            "dod.triage.model has an empty model part ({SHAPE}), got {model:?}"
        ));
    }
    if name.split('/').any(is_dot_segment) {
        return Err(format!(
            "dod.triage.model must not contain a `.` or `..` segment ({SHAPE}), got {model:?}"
        ));
    }
    Ok((provider, name))
}

// `.` oder `..`: ein Pfadsegment, das auf das eigene bzw. das
// Elternverzeichnis zeigt.
fn is_dot_segment(segment: &str) -> bool {
    matches!(segment, "." | "..")
}

// Prüft `dod.triage.effort` gegen die kanonischen (kleingeschriebenen)
// Namen von `harw_types::ReasoningEffort`.
fn validate_triage_effort(effort: &str) -> Result<(), String> {
    match effort.parse::<ReasoningEffort>() {
        Ok(level) if level.to_string() == effort => Ok(()),
        _ => Err(format!(
            "dod.triage.effort must be one of minimal, low, medium, high, xhigh, max, \
             got {effort:?}"
        )),
    }
}

/// `[dod.triage]` — Modellwahl und Grenzen der automatischen DoD-Triage
/// (PL-100 §3.2, WP 1.9).
///
/// # Description
/// Ohne [`Self::model`] läuft keine automatische Triage (Invariante 7,
/// [`DodSection::effective_watch_mode`]). Nur Home-Ebene und aktives Profil
/// setzen diese Tabelle; die Config-Discovery verwirft sie aus jeder
/// anderen Ebene mit einer `ScopeDiagnostic` (Bedrohung B15).
/// Deserialisierung läuft über eine private Rohform und danach
/// [`Self::validate`]; `effort` wird dabei auf den kanonischen Namen
/// normalisiert (`"HIGH"` → `"high"`).
///
/// # Examples
/// ```rust
/// use harw_config::DodTriageSection;
///
/// let triage: DodTriageSection =
///     toml::from_str("model = \"local/qwen2.5\"\neffort = \"HIGH\"")?;
/// assert_eq!(triage.effort.as_deref(), Some("high"));
/// assert_eq!(triage.max_parallel, 2);
/// assert_eq!(triage.provider_and_model(), Some(("local", "qwen2.5")));
/// # Ok::<(), toml::de::Error>(())
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "DodTriageSectionToml")]
pub struct DodTriageSection {
    /// Triage-Modell als `"<provider>/<model>"` ([`parse_triage_model`]).
    /// `None` (Vorgabe): keine automatische Triage.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Reasoning-Effort der Triage-Anfragen, einer der kanonischen Namen
    /// von `harw_types::ReasoningEffort` (`minimal`, `low`, `medium`,
    /// `high`, `xhigh`, `max`). `None`: Vorgabe des Modells.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
    /// Antwortbudget je Triage-Anfrage, `1..=`[`MAX_TRIAGE_TOKENS`].
    /// `None`: Vorgabe des Dispatchers.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
    /// Höchstzahl gleichzeitiger Triage-Läufe, `1..=`[`MAX_TRIAGE_PARALLEL`],
    /// Vorgabe [`DEFAULT_TRIAGE_MAX_PARALLEL`].
    pub max_parallel: u8,
}

/// Raw TOML shape of `[dod.triage]`: same keys, defaults and
/// `deny_unknown_fields` as [`DodTriageSection`], without its invariants.
/// Only the serde `try_from` source of [`DodTriageSection`].
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DodTriageSectionToml {
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    effort: Option<String>,
    #[serde(default)]
    max_tokens: Option<u32>,
    #[serde(default = "default_triage_max_parallel")]
    max_parallel: u8,
}

impl TryFrom<DodTriageSectionToml> for DodTriageSection {
    type Error = String;

    /// Builds the section from its raw TOML shape, normalises `effort` to
    /// its canonical name and checks [`DodTriageSection::validate`].
    ///
    /// # Errors
    /// The reason from [`DodTriageSection::validate`].
    fn try_from(raw: DodTriageSectionToml) -> Result<Self, Self::Error> {
        let DodTriageSectionToml {
            model,
            effort,
            max_tokens,
            max_parallel,
        } = raw;
        // An unknown name stays as written and fails in `validate`.
        let effort = effort.map(|written| {
            written
                .parse::<ReasoningEffort>()
                .map_or(written, |level| level.to_string())
        });
        let section = Self {
            model,
            effort,
            max_tokens,
            max_parallel,
        };
        section.validate()?;
        Ok(section)
    }
}

impl Default for DodTriageSection {
    fn default() -> Self {
        Self {
            model: None,
            effort: None,
            max_tokens: None,
            max_parallel: DEFAULT_TRIAGE_MAX_PARALLEL,
        }
    }
}

impl DodTriageSection {
    /// Prüft die Invarianten von `[dod.triage]`.
    ///
    /// # Errors
    /// `Err(String)` mit einer Begründung, die das Feld nennt, wenn
    /// `model` nicht [`parse_triage_model`] genügt, `effort` kein
    /// kanonischer Effort-Name ist, `max_tokens` außerhalb von
    /// `1..=`[`MAX_TRIAGE_TOKENS`] oder `max_parallel` außerhalb von
    /// `1..=`[`MAX_TRIAGE_PARALLEL`] liegt.
    pub fn validate(&self) -> Result<(), String> {
        if let Some(model) = &self.model {
            parse_triage_model(model)?;
        }
        if let Some(effort) = &self.effort {
            validate_triage_effort(effort)?;
        }
        if let Some(max_tokens) = self.max_tokens
            && !(1..=MAX_TRIAGE_TOKENS).contains(&max_tokens)
        {
            return Err(format!(
                "dod.triage.max_tokens must be between 1 and {MAX_TRIAGE_TOKENS}, got {max_tokens}"
            ));
        }
        if !(1..=MAX_TRIAGE_PARALLEL).contains(&self.max_parallel) {
            return Err(format!(
                "dod.triage.max_parallel must be between 1 and {MAX_TRIAGE_PARALLEL}, got {}",
                self.max_parallel
            ));
        }
        Ok(())
    }

    /// Provider und Modell aus [`Self::model`].
    ///
    /// # Returns
    /// `None`, wenn kein Modell gesetzt ist oder der Wert (nur bei einem
    /// per Struct-Literal gebauten, ungeprüften Abschnitt möglich) nicht
    /// [`parse_triage_model`] genügt.
    #[must_use]
    pub fn provider_and_model(&self) -> Option<(&str, &str)> {
        self.model
            .as_deref()
            .and_then(|model| parse_triage_model(model).ok())
    }

    /// [`Self::effort`] als typisierter `ReasoningEffort`.
    ///
    /// # Returns
    /// `None`, wenn kein Effort gesetzt ist oder der Name unbekannt ist.
    #[must_use]
    pub fn effort_level(&self) -> Option<ReasoningEffort> {
        self.effort
            .as_deref()
            .and_then(|effort| effort.parse::<ReasoningEffort>().ok())
    }
}

/// `[dod.approve]` — Freigabe-Einstellungen der DoD-Kette (PL-100 §3.1).
///
/// # Description
/// [`Self::tui_tty`] erlaubt die polkit-Freigabe im TTY der TUI
/// (`pkttyagent`), Vorgabe `true` (Owner-Entscheidung O12; die
/// Installationspfade und `harw update` tragen den Schlüssel über
/// [`crate::dod_home::ensure_dod_defaults`] in eine vorhandene
/// `config.toml` ausdrücklich ein). Weil ein kompromittiertes harw in
/// diesem TTY die angezeigten Details überschreiben kann, deckt der Modus
/// **nur Freeze/Thaw** ab; Kill und Isolation laufen nie über ihn
/// (Bedrohung B17). Nur Home-Ebene und aktives Profil setzen diese Tabelle;
/// jede andere Ebene kann `tui_tty` nur auf `false` verengen.
///
/// # Examples
/// ```rust
/// use harw_config::DodApproveSection;
///
/// assert!(DodApproveSection::default().tui_tty);
/// let approve: DodApproveSection = toml::from_str("tui_tty = false")?;
/// assert!(!approve.tui_tty);
/// # Ok::<(), toml::de::Error>(())
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DodApproveSection {
    /// Same-TTY-Freigabe (nur Freeze/Thaw). Vorgabe `true`; `false`
    /// verlangt für jeden Eingriff Freigabe A oder B mit vertrauenswürdiger
    /// Anzeige.
    #[serde(default = "default_true")]
    pub tui_tty: bool,
}

impl Default for DodApproveSection {
    fn default() -> Self {
        Self { tui_tty: true }
    }
}

/// `[dod.watch]` — Betrieb der User-Unit `harw-dod-watch` (PL-100 O5).
///
/// # Description
/// [`Self::mode`] wählt, was der Watch mit neuen Befunden tut;
/// [`Self::autoconnect`] lässt die Unit sich beim Start mit dem Gateway
/// verbinden. Ein nicht vertrauter Repo-Layer setzt diese Tabelle nie.
///
/// # Examples
/// ```rust
/// use harw_config::{DodWatchMode, DodWatchSection};
///
/// let watch = DodWatchSection::default();
/// assert_eq!(watch.mode, DodWatchMode::Triage);
/// assert!(watch.autoconnect);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DodWatchSection {
    /// Watch-Modus, Vorgabe [`DodWatchMode::Triage`]. Ohne Triage-Modell
    /// wirkt `triage` als `notify` ([`DodSection::effective_watch_mode`]).
    #[serde(default)]
    pub mode: DodWatchMode,
    /// Verbindet `harw-dod-watch` beim Start automatisch mit dem Gateway.
    /// Vorgabe `true` (O5).
    #[serde(default = "default_true")]
    pub autoconnect: bool,
}

impl Default for DodWatchSection {
    fn default() -> Self {
        Self {
            mode: DodWatchMode::default(),
            autoconnect: true,
        }
    }
}

/// `[dod.watch] mode` — was `harw-dod-watch` mit neuen Befunden tut.
///
/// # Examples
/// ```rust
/// use harw_config::DodWatchMode;
///
/// assert_eq!(DodWatchMode::default(), DodWatchMode::Triage);
/// assert_eq!(DodWatchMode::Notify.as_str(), "notify");
/// let names: Vec<&str> = DodWatchMode::ALL.iter().map(|mode| mode.as_str()).collect();
/// assert_eq!(names, ["off", "notify", "triage"]);
/// ```
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
#[non_exhaustive]
pub enum DodWatchMode {
    /// Kein Watch: weder Hinweise noch Triage.
    Off,
    /// Nur deterministische Hinweise, keine KI-Triage.
    Notify,
    /// Hinweise plus automatische Triage mit dem Modell aus `[dod.triage]`
    /// (Vorgabe).
    #[default]
    Triage,
}

impl DodWatchMode {
    /// Alle Modi in aufsteigender Automatisierung (für Auswahllisten).
    pub const ALL: [Self; 3] = [Self::Off, Self::Notify, Self::Triage];

    /// Der Konfigurationswert (`"off"`, `"notify"` bzw. `"triage"`).
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Notify => "notify",
            Self::Triage => "triage",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult, ctx};

    #[test]
    fn test_dod_section_defaults_from_empty_toml() -> TestResult {
        let section: DodSection = toml::from_str("").map_err(ctx("parse toml"))?;
        assert!(section.kill_requires_human);
        assert!(section.auto_freeze);
        assert_eq!(section.proof_key_dir, None);
        assert!(section.allowed_cgroup_prefixes.is_empty());
        assert_eq!(section.export_dir, None);
        assert_eq!(section.findings_window_hours, DEFAULT_FINDINGS_WINDOW_HOURS);
        assert_eq!(section.findings_window_hours, 24);
        assert_eq!(section, DodSection::default());
        Ok(())
    }

    #[test]
    fn test_dod_section_parses_export_dir_and_window() -> TestResult {
        let src = r#"
            export_dir = "/var/lib/harw-dod/export"
            findings_window_hours = 72
        "#;
        let section: DodSection = toml::from_str(src).map_err(ctx("parse toml"))?;
        assert_eq!(
            section.export_dir,
            Some(PathBuf::from("/var/lib/harw-dod/export"))
        );
        assert_eq!(section.findings_window_hours, 72);
        assert!(section.validate().is_ok());
        Ok(())
    }

    #[test]
    fn test_validate_rejects_relative_export_dir() -> TestResult {
        let section = DodSection {
            export_dir: Some(PathBuf::from("dod/export")),
            ..DodSection::default()
        };
        let Err(error) = section.validate() else {
            return Err(TestError::Unexpected(
                "relative export_dir must be rejected".to_string(),
            ));
        };
        assert!(error.contains("export_dir"));
        Ok(())
    }

    #[test]
    fn test_validate_rejects_zero_findings_window() -> TestResult {
        let section = DodSection {
            findings_window_hours: 0,
            ..DodSection::default()
        };
        let Err(error) = section.validate() else {
            return Err(TestError::Unexpected(
                "findings_window_hours = 0 must be rejected".to_string(),
            ));
        };
        assert!(error.contains("findings_window_hours"));
        Ok(())
    }

    #[test]
    fn test_deserialization_runs_validate() -> TestResult {
        for (src, field) in [
            ("findings_window_hours = 0", "findings_window_hours"),
            ("export_dir = \"dod/export\"", "export_dir"),
            ("kill_requires_human = false", "kill_requires_human"),
            (
                "allowed_cgroup_prefixes = [\" \"]",
                "allowed_cgroup_prefixes",
            ),
        ] {
            let Err(error) = toml::from_str::<DodSection>(src) else {
                return Err(TestError::Unexpected(format!(
                    "`{src}` must not deserialize"
                )));
            };
            assert!(
                error.to_string().contains(field),
                "error for `{src}` names `{field}`: {error}"
            );
        }
        Ok(())
    }

    #[test]
    fn test_dod_section_full_toml_round_trip() -> TestResult {
        let src = r#"
            kill_requires_human = true
            auto_freeze = false
            proof_key_dir = "/var/lib/harw/dod/keys"
            allowed_cgroup_prefixes = ["/sys/fs/cgroup/harw.slice/"]
            export_dir = "/srv/harw-dod/export"
            findings_window_hours = 6
        "#;
        let section: DodSection = toml::from_str(src).map_err(ctx("parse toml"))?;
        assert!(section.kill_requires_human);
        assert!(!section.auto_freeze);
        assert_eq!(
            section.proof_key_dir,
            Some(PathBuf::from("/var/lib/harw/dod/keys"))
        );
        assert_eq!(
            section.allowed_cgroup_prefixes,
            vec!["/sys/fs/cgroup/harw.slice/".to_owned()]
        );

        let encoded = toml::to_string(&section).map_err(ctx("encode toml"))?;
        let decoded: DodSection = toml::from_str(&encoded).map_err(ctx("parse encoded toml"))?;
        assert_eq!(decoded, section);
        Ok(())
    }

    #[test]
    fn test_dod_section_rejects_unknown_field() -> TestResult {
        let src = r#"
            kill_requires_human = true
            kill_reqiures_human = true
        "#;
        let Err(error) = toml::from_str::<DodSection>(src) else {
            return Err(TestError::Unexpected(
                "unknown field must be rejected".to_string(),
            ));
        };
        assert!(error.to_string().contains("unknown field"));
        Ok(())
    }

    #[test]
    fn test_validate_accepts_default_section() {
        assert!(DodSection::default().validate().is_ok());
    }

    #[test]
    fn test_validate_rejects_kill_requires_human_false() -> TestResult {
        let section = DodSection {
            kill_requires_human: false,
            ..DodSection::default()
        };
        let Err(error) = section.validate() else {
            return Err(TestError::Unexpected(
                "kill_requires_human = false must be rejected".to_string(),
            ));
        };
        assert!(error.contains("kill_requires_human"));
        Ok(())
    }

    #[test]
    fn test_validate_rejects_relative_and_outside_mount_cgroup_prefixes() -> TestResult {
        for bad in [
            "harw.slice",
            "/harw.slice",
            "/sys/fs/cgroup",
            "/sys/fs/cgroup/",
            "/sys/fs/cgroup/../etc",
            "/sys/fs/cgroupx/harw.slice",
            "/sys/fs/cgroup//harw.slice",
        ] {
            let section = DodSection {
                allowed_cgroup_prefixes: vec![bad.to_owned()],
                ..DodSection::default()
            };
            let Err(error) = section.validate() else {
                return Err(TestError::Unexpected(format!("{bad:?} must be rejected")));
            };
            assert!(error.contains("allowed_cgroup_prefixes"), "{error}");
        }
        Ok(())
    }

    #[test]
    fn test_warden_cgroup_id_converts_the_canonical_prefix_form() {
        assert_eq!(
            warden_cgroup_id("/sys/fs/cgroup/user.slice/harw.slice/").as_deref(),
            Some("user.slice/harw.slice")
        );
        assert_eq!(
            warden_cgroup_id("/sys/fs/cgroup/harw.slice").as_deref(),
            Some("harw.slice")
        );
        assert_eq!(warden_cgroup_id("harw.slice"), None);
    }

    #[test]
    fn test_validate_rejects_blank_cgroup_prefix() -> TestResult {
        let section = DodSection {
            allowed_cgroup_prefixes: vec!["   ".to_owned()],
            ..DodSection::default()
        };
        let Err(error) = section.validate() else {
            return Err(TestError::Unexpected(
                "blank cgroup prefix must be rejected".to_string(),
            ));
        };
        assert!(error.contains("allowed_cgroup_prefixes"));
        Ok(())
    }

    #[test]
    fn test_validate_accepts_disabled_auto_freeze_with_human_kill() {
        let section = DodSection {
            auto_freeze: false,
            ..DodSection::default()
        };
        assert!(section.validate().is_ok());
    }

    #[test]
    fn test_dod_sub_sections_default_from_empty_toml() -> TestResult {
        let section: DodSection = toml::from_str("").map_err(ctx("parse toml"))?;
        assert_eq!(section.triage.model, None);
        assert_eq!(section.triage.effort, None);
        assert_eq!(section.triage.max_tokens, None);
        assert_eq!(section.triage.max_parallel, DEFAULT_TRIAGE_MAX_PARALLEL);
        assert_eq!(section.triage.max_parallel, 2);
        assert!(section.approve.tui_tty);
        assert_eq!(section.watch.mode, DodWatchMode::Triage);
        assert!(section.watch.autoconnect);
        assert_eq!(section.triage, DodTriageSection::default());
        assert_eq!(section.approve, DodApproveSection::default());
        assert_eq!(section.watch, DodWatchSection::default());

        // Empty sub-tables keep the defaults, too.
        let section: DodSection = toml::from_str("[triage]\n[approve]\n[watch]\n")
            .map_err(ctx("parse empty sub-tables"))?;
        assert_eq!(section, DodSection::default());
        Ok(())
    }

    #[test]
    fn test_dod_sub_sections_parse_and_round_trip() -> TestResult {
        let src = r#"
            [triage]
            model = "openrouter/openai/gpt-4o"
            effort = "medium"
            max_tokens = 4096
            max_parallel = 4

            [approve]
            tui_tty = false

            [watch]
            mode = "notify"
            autoconnect = false
        "#;
        let section: DodSection = toml::from_str(src).map_err(ctx("parse toml"))?;
        assert_eq!(
            section.triage.model.as_deref(),
            Some("openrouter/openai/gpt-4o")
        );
        assert_eq!(
            section.triage.provider_and_model(),
            Some(("openrouter", "openai/gpt-4o"))
        );
        assert_eq!(section.triage.effort.as_deref(), Some("medium"));
        assert_eq!(section.triage.effort_level(), Some(ReasoningEffort::Medium));
        assert_eq!(section.triage.max_tokens, Some(4096));
        assert_eq!(section.triage.max_parallel, 4);
        assert!(!section.approve.tui_tty);
        assert_eq!(section.watch.mode, DodWatchMode::Notify);
        assert!(!section.watch.autoconnect);

        let encoded = toml::to_string(&section).map_err(ctx("encode toml"))?;
        let decoded: DodSection = toml::from_str(&encoded).map_err(ctx("parse encoded toml"))?;
        assert_eq!(decoded, section);

        // The default section round-trips as well (no `None` value is emitted).
        let encoded = toml::to_string(&DodSection::default()).map_err(ctx("encode default"))?;
        let decoded: DodSection = toml::from_str(&encoded).map_err(ctx("parse encoded default"))?;
        assert_eq!(decoded, DodSection::default());
        Ok(())
    }

    #[test]
    fn test_triage_effort_is_normalised_to_its_canonical_name() -> TestResult {
        let section: DodSection =
            toml::from_str("[triage]\neffort = \"XHIGH\"\n").map_err(ctx("parse toml"))?;
        assert_eq!(section.triage.effort.as_deref(), Some("xhigh"));
        assert_eq!(section.triage.effort_level(), Some(ReasoningEffort::Xhigh));

        // A struct literal bypasses the normalisation; `validate` then
        // insists on the canonical spelling.
        let section = DodSection {
            triage: DodTriageSection {
                effort: Some("High".to_owned()),
                ..DodTriageSection::default()
            },
            ..DodSection::default()
        };
        let Err(error) = section.validate() else {
            return Err(TestError::Unexpected(
                "non-canonical effort in a struct literal must be rejected".to_string(),
            ));
        };
        assert!(error.contains("dod.triage.effort"), "{error}");
        Ok(())
    }

    #[test]
    fn test_triage_validation_rejects_out_of_range_and_unknown_values() -> TestResult {
        for (src, field) in [
            ("[triage]\nmax_tokens = 0\n", "dod.triage.max_tokens"),
            ("[triage]\nmax_tokens = 32769\n", "dod.triage.max_tokens"),
            ("[triage]\nmax_parallel = 0\n", "dod.triage.max_parallel"),
            ("[triage]\nmax_parallel = 9\n", "dod.triage.max_parallel"),
            ("[triage]\neffort = \"extreme\"\n", "dod.triage.effort"),
            ("[triage]\nmodel = \"noslash\"\n", "dod.triage.model"),
            ("[triage]\nmodel = \"/model\"\n", "dod.triage.model"),
            ("[triage]\nmodel = \"provider/\"\n", "dod.triage.model"),
            (
                "[triage]\nmodel = \"provider//model\"\n",
                "dod.triage.model",
            ),
            (
                "[triage]\nmodel = \"pro vider/model\"\n",
                "dod.triage.model",
            ),
            (
                "[triage]\nmodel = \"provider/mo del\"\n",
                "dod.triage.model",
            ),
            (
                "[triage]\nmodel = \"provider/model\\u202e\"\n",
                "dod.triage.model",
            ),
            ("[triage]\nmodel = \"\"\n", "dod.triage.model"),
            ("[triage]\nmodel = \"../x\"\n", "dod.triage.model"),
            ("[triage]\nmodel = \"./x\"\n", "dod.triage.model"),
            ("[triage]\nmodel = \"p/../../etc\"\n", "dod.triage.model"),
            ("[triage]\nmodel = \"p/m/.\"\n", "dod.triage.model"),
            ("[triage]\nmodel = \"p/..\"\n", "dod.triage.model"),
        ] {
            let Err(error) = toml::from_str::<DodSection>(src) else {
                return Err(TestError::Unexpected(format!(
                    "`{src}` must not deserialize"
                )));
            };
            assert!(
                error.to_string().contains(field),
                "error for `{src}` names `{field}`: {error}"
            );
        }
        Ok(())
    }

    #[test]
    fn test_triage_validation_accepts_range_bounds_and_nested_model_paths() -> TestResult {
        let src = r#"
            [triage]
            model = "p/m/extra"
            max_tokens = 32768
            max_parallel = 8
        "#;
        let section: DodSection = toml::from_str(src).map_err(ctx("parse upper bounds"))?;
        assert_eq!(section.triage.provider_and_model(), Some(("p", "m/extra")));
        assert_eq!(section.triage.max_tokens, Some(MAX_TRIAGE_TOKENS));
        assert_eq!(section.triage.max_parallel, MAX_TRIAGE_PARALLEL);

        let section: DodSection = toml::from_str("[triage]\nmax_tokens = 1\nmax_parallel = 1\n")
            .map_err(ctx("parse lower bounds"))?;
        assert!(section.validate().is_ok());
        Ok(())
    }

    #[test]
    fn test_parse_triage_model_splits_at_the_first_slash() -> TestResult {
        assert_eq!(
            parse_triage_model("local-ollama/qwen2.5-coder:7b"),
            Ok(("local-ollama", "qwen2.5-coder:7b"))
        );
        assert_eq!(
            parse_triage_model("openrouter/anthropic/claude"),
            Ok(("openrouter", "anthropic/claude"))
        );
        let too_long = format!("p/{}", "m".repeat(MAX_TRIAGE_MODEL_BYTES));
        let Err(error) = parse_triage_model(&too_long) else {
            return Err(TestError::Unexpected(
                "an over-long model reference must be rejected".to_string(),
            ));
        };
        assert!(error.contains("dod.triage.model"), "{error}");
        Ok(())
    }

    #[test]
    fn test_dod_sub_sections_reject_unknown_fields() -> TestResult {
        for (src, expected) in [
            ("[triage]\nmodle = \"a/b\"\n", "unknown field"),
            ("[approve]\ntui = true\n", "unknown field"),
            ("[watch]\nautoconect = true\n", "unknown field"),
            ("[watch]\nmode = \"auto\"\n", "unknown variant"),
        ] {
            let Err(error) = toml::from_str::<DodSection>(src) else {
                return Err(TestError::Unexpected(format!(
                    "`{src}` must not deserialize"
                )));
            };
            assert!(
                error.to_string().contains(expected),
                "error for `{src}` mentions `{expected}`: {error}"
            );
        }
        Ok(())
    }

    #[test]
    fn test_dod_section_validate_covers_the_triage_section() -> TestResult {
        let section = DodSection {
            triage: DodTriageSection {
                max_parallel: 0,
                ..DodTriageSection::default()
            },
            ..DodSection::default()
        };
        let Err(error) = section.validate() else {
            return Err(TestError::Unexpected(
                "max_parallel = 0 must be rejected".to_string(),
            ));
        };
        assert!(error.contains("dod.triage.max_parallel"), "{error}");
        Ok(())
    }

    #[test]
    fn test_watch_mode_names_are_lowercase_config_values() -> TestResult {
        for mode in DodWatchMode::ALL {
            let section: DodSection =
                toml::from_str(&format!("[watch]\nmode = \"{}\"\n", mode.as_str()))
                    .map_err(ctx("parse watch mode"))?;
            assert_eq!(section.watch.mode, mode);
        }
        assert_eq!(DodWatchMode::Off.as_str(), "off");
        assert_eq!(DodWatchMode::Notify.as_str(), "notify");
        assert_eq!(DodWatchMode::Triage.as_str(), "triage");
        Ok(())
    }

    #[test]
    fn test_effective_watch_mode_falls_back_to_notify_without_a_model() -> TestResult {
        let mut section = DodSection::default();
        assert_eq!(section.watch.mode, DodWatchMode::Triage);
        assert_eq!(section.effective_watch_mode(), DodWatchMode::Notify);

        section.triage.model = Some("local/qwen2.5".to_owned());
        assert_eq!(section.effective_watch_mode(), DodWatchMode::Triage);

        section.watch.mode = DodWatchMode::Off;
        assert_eq!(section.effective_watch_mode(), DodWatchMode::Off);
        section.triage.model = None;
        assert_eq!(section.effective_watch_mode(), DodWatchMode::Off);
        section.watch.mode = DodWatchMode::Notify;
        assert_eq!(section.effective_watch_mode(), DodWatchMode::Notify);
        Ok(())
    }
}
