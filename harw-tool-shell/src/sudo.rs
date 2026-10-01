//! Werkzeug `host.sudo_exec`: ein einzelner Root-Befehl auf dem Host, freigegeben
//! in einem eigenen Fenster der TUI (Runde 5, Teil B).
//!
//! # Beschreibung
//! Das Modell nennt ein exaktes `argv` und einen Grund. Der Aufruf läuft **nie**
//! über `sh -c`, sondern immer über ein festgepinntes `sudo`
//! (`/usr/bin/sudo`, sonst `/bin/sudo` — Muster aus
//! `harw-killer/src/privilege.rs`) mit `-k` (kein Zeitstempel-Cache) und `--`
//! vor dem `argv`. Vor jeder Ausführung geht eine [`SudoPrompt`] an die
//! anzeigende Oberfläche (heute ausschließlich `harw-tui`,
//! `harw_tui::sudo_dialog`). Nur deren ausdrückliche Antwort
//! ([`SudoAnswer`]) führt zu einer Ausführung; jeder andere Ausgang
//! (kein Kanal, Kanal geschlossen, Antwort fallengelassen, Zeitablauf nach
//! [`SUDO_PROMPT_TIMEOUT`], Ablehnung) ist eine Ablehnung.
//!
//! # Zwei Wege
//! - **Passwortlos** (`sudo -n -k true` gelingt): das Fenster zeigt nur
//!   „Freigeben / Ablehnen“; der Befehl läuft als `sudo -n -k -- argv…` mit
//!   stdin `/dev/null`. Es gibt kein Geheimnis.
//! - **Mit Passwort**: `sudo -S -k -p <Einmal-Marke> -- argv…`. Das Passwort
//!   ([`SudoSecret`], `Zeroizing<Vec<u8>>`) wird **erst dann** in die
//!   stdin-Pipe geschrieben, wenn `sudo` die zufällige Einmal-Marke als
//!   Eingabeaufforderung auf stderr ausgibt; danach wird die Pipe sofort
//!   geschlossen. Fragt `sudo` nicht (z. B. eine `NOPASSWD`-Regel nur für
//!   diesen Befehl), wird das Passwort nie geschrieben — es kann so nicht auf
//!   der stdin des Befehls landen (Abweichung vom Plan-Wortlaut `-p ''`,
//!   bewusst strenger). Fragt `sudo` ein zweites Mal (falsches Passwort), gibt
//!   es keinen zweiten Versuch; das Modell erfährt nur
//!   „Authentifizierung fehlgeschlagen“.
//!
//! # Härtung
//! - Umgebung: `env_clear` plus Positivliste `PATH` (feste Systempfade) und
//!   `LANG=C.UTF-8` (vorhersagbare `sudo`-Meldungen). Kein `SUDO_ASKPASS`.
//! - Start über dieselben Bausteine wie `shell.exec` auf dem Host:
//!   `prlimit` ([`crate::limits::launch_command`]), `setsid --wait`
//!   ([`crate::exec::resolve_setsid`]), gekappte Ausgabe und ein hartes
//!   Zeitlimit.
//! - stderr wird von der Einmal-Marke und von `sudo`-Prompt-Zeilen bereinigt.
//! - `argv` und Grund dürfen keine Steuer- oder unsichtbaren Formatzeichen
//!   enthalten (keine Täuschung im Freigabefenster); `argv[0]` darf selbst
//!   kein Rechte-Werkzeug sein (`sudo`, `doas`, `pkexec`, `su`, …).
//! - Ein [`SudoAuditRecord`] (Operator, Sitzung, argv, argv-Hash, Modus,
//!   Entscheidung, Exit-Code, Dauer) geht an eine [`SudoAuditSink`]; er trägt
//!   nie ein Geheimnis und leitet `Redact` ab.
//!
//! # Registrierung
//! [`SudoToolProvider`] verlangt im Konstruktor einen [`SudoPromptSender`] —
//! ohne Empfänger gibt es kein Werkzeug. Die Runtime baut den Kanal nur für
//! `EntryKind::Tui` und hängt den Provider nur an die Rollen
//! `uia-shell-worker`/`host-process-worker`
//! (`harw_registry_defaults::profile::sudo_tools_for_role`).
//!
//! # Nebenläufigkeit
//! [`SudoPrompt`] ist `Send`, nicht `Sync`, und wird beim Beantworten
//! konsumiert. [`SudoPromptSender`] ist ein `mpsc::UnboundedSender` (`Clone`).

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fmt;
use std::path::{Path, PathBuf};
use std::process::{ExitStatus, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use harw_authority::{Permission, SandboxSpec};
use harw_tools::{
    ToolCall, ToolExecutionContext, ToolExecutor, ToolExecutorFuture, ToolOutput, ToolsError,
    schema::{AdditionalProperties, JsonSchema, JsonSchemaType},
    spec::{FunctionToolSpec, ToolName, ToolSpec},
};
use harw_types::cancel::CancelToken;
use serde::Deserialize;
use serde_json::json;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::{Child, ChildStdin, Command as TokioCommand};
use tokio::sync::{mpsc, oneshot};
use tracing::{info, warn};
use zeroize::{Zeroize, ZeroizeOnDrop, Zeroizing};

use crate::exec::{resolve_setsid, terminate};
use crate::limits::{ShellLimits, launch_command};

// ── Konstanten ────────────────────────────────────────────────────────────────

/// Name des Werkzeugs.
pub const SUDO_EXEC_TOOL: &str = "host.sudo_exec";

/// Wartezeit auf **eine** Entscheidung im Freigabefenster; danach gilt die
/// Anfrage als abgelehnt (Plan Teil B, Punkt 4).
pub const SUDO_PROMPT_TIMEOUT: Duration = Duration::from_secs(120);

/// Größte zulässige Passwortlänge in Bytes. Die TUI legt ihren
/// Eingabepuffer mit genau dieser Kapazität an, damit er nie umkopiert wird
/// (eine Umkopie ließe den alten Puffer ungenullt im Heap zurück).
pub const SUDO_MAX_SECRET_BYTES: usize = 1024;

/// Feste Suchpfade für `sudo`; nie `PATH` (Muster `harw-killer/src/privilege.rs`).
const SUDO_CANDIDATES: [&str; 2] = ["/usr/bin/sudo", "/bin/sudo"];

/// `PATH` des Root-Befehls (feste Systempfade, unabhängig vom Harness-`PATH`).
const SUDO_ENV_PATH: &str = "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin";

/// `LANG` des Root-Befehls: vorhersagbare (englische) `sudo`-Meldungen für
/// die Erkennung einer fehlgeschlagenen Authentifizierung, UTF-8-Ausgabe.
const SUDO_ENV_LANG: &str = "C.UTF-8";

/// Vorgabe-Zeitlimit eines Root-Befehls.
const DEFAULT_TIMEOUT_SECS: u64 = 300;

/// Vorgabe-Budget für stdout+stderr.
const DEFAULT_MAX_OUTPUT_BYTES: usize = 64 * 1024;

/// Zeitlimit der Passwortlos-Probe `sudo -n -k true`.
const PASSWORDLESS_PROBE_TIMEOUT: Duration = Duration::from_secs(5);

/// Frist, in der `sudo` nach dem Start nach dem Passwort fragen muss. Danach
/// wird die stdin-Pipe ohne Passwort geschlossen: eine spätere Frage scheitert
/// dann an EOF (fail-closed), ein Befehl ohne Passwortabfrage sieht EOF.
const DEFAULT_PASSWORD_WINDOW: Duration = Duration::from_secs(10);

/// Höchstdauer für das Schreiben des Passworts in die Pipe.
const PASSWORD_WRITE_TIMEOUT: Duration = Duration::from_secs(2);

/// Höchstzahl der `argv`-Elemente.
const MAX_ARGV_LEN: usize = 64;

/// Höchstlänge eines einzelnen `argv`-Elements in Bytes.
const MAX_ARG_BYTES: usize = 4096;

/// Höchstlänge des Grundes in Zeichen.
const MAX_REASON_CHARS: usize = 500;

/// Lesepuffer je Pipe.
const READ_CHUNK_BYTES: usize = 8 * 1024;

/// Programme, die Rechte erhöhen. `shell.exec` lehnt sie in Befehlsposition
/// ab ([`escalation_program`]); `host.sudo_exec` lehnt sie als `argv[0]` ab.
const ESCALATION_PROGRAMS: &[&str] = &[
    "sudo", "sudo-rs", "sudoedit", "doas", "pkexec", "su", "run0",
];

/// Wörter, nach denen in einer Shell-Zeile noch ein Befehl folgt
/// (Wrapper wie `env sudo …`, `nohup sudo …`, Schlüsselwörter wie `then`).
const COMMAND_PREFIX_WORDS: &[&str] = &[
    "env", "exec", "command", "builtin", "nohup", "nice", "ionice", "time", "stdbuf", "timeout",
    "xargs", "setsid", "chrt", "taskset", "then", "do", "else", "elif", "if", "while", "until",
    "!", "{", "(",
];

/// Teilstrings (klein geschrieben) in der stderr von `sudo`/`sudo-rs`, die
/// eine fehlgeschlagene Authentifizierung anzeigen (bei Exit-Code 1).
const AUTH_FAILURE_MARKERS: &[&str] = &[
    "incorrect password",
    "no password was provided",
    "a password is required",
    "authentication failed",
    "authentication failure",
    "sorry, try again",
    "incorrect authentication",
];

/// `sudo`-Zeilen, die aus der stderr für das Modell entfernt werden.
const PROMPT_LINE_PREFIXES: &[&str] = &["[sudo] password for", "Password:", "Sorry, try again."];

/// Meldung ohne angehängtes Freigabefenster (fail-closed).
const NO_UI_MSG: &str = "host.sudo_exec: kein Freigabefenster verfügbar — Root-Befehle laufen \
     nur mit angeschlossener interaktiver TUI (fail-closed). Nicht erneut versuchen: nenne dem \
     Nutzer den exakten Befehl, damit er ihn selbst ausführt.";
/// Meldung bei Ablehnung, Zeitablauf oder fallengelassener Antwort.
const DENIED_MSG: &str =
    "host.sudo_exec: nicht freigegeben (abgelehnt, Zeitablauf oder Fenster geschlossen)";
/// Einzige Auskunft an das Modell bei falschem Passwort.
const AUTH_FAILED_MSG: &str = "host.sudo_exec: Authentifizierung fehlgeschlagen";
/// Meldung, wenn der passwortlose Weg doch ein Passwort verlangt.
const PASSWORD_REQUIRED_MSG: &str = "host.sudo_exec: sudo verlangt für diesen Befehl ein \
     Passwort; passwortloses sudo ist hierfür nicht verfügbar";

/// Vorgabe-rlimits eines Root-Befehls: großzügiger als `shell.exec`
/// (Paketinstallationen schreiben große Dateien und brauchen CPU-Zeit bis zum
/// Zeitlimit), aber weiterhin gedeckelt.
fn default_sudo_limits() -> ShellLimits {
    const GIB: u64 = 1024 * 1024 * 1024;
    ShellLimits {
        as_bytes: 4 * GIB,
        cpu_secs: DEFAULT_TIMEOUT_SECS,
        fsize_bytes: 4 * GIB,
        nofile: 1024,
        ..ShellLimits::default()
    }
}

// ── Geheimnis ─────────────────────────────────────────────────────────────────

/// Warum ein Passwort nicht als [`SudoSecret`] angenommen wurde. Trägt nie
/// den Inhalt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SudoSecretError {
    /// Das Passwort ist leer.
    Empty,
    /// Das Passwort ist länger als [`SUDO_MAX_SECRET_BYTES`].
    TooLong,
    /// Das Passwort enthält `\n`, `\r` oder `\0` (würde die Zeile für
    /// `sudo -S` verfälschen).
    ForbiddenByte,
}

impl fmt::Display for SudoSecretError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => f.write_str("Passwort ist leer"),
            Self::TooLong => f.write_str("Passwort ist zu lang"),
            Self::ForbiddenByte => f.write_str("Passwort enthält ein unzulässiges Steuerzeichen"),
        }
    }
}

impl std::error::Error for SudoSecretError {}

/// Ein sudo-Passwort im Speicher.
///
/// # Beschreibung
/// Hält die Bytes in `Zeroizing<Vec<u8>>`: beim Drop wird die **gesamte**
/// Kapazität genullt. Kein `Clone` (nur [`Self::try_clone`], das bewusst eine
/// zweite, ebenfalls genullte Kopie mit exakter Kapazität anlegt), kein
/// `Display`, kein `Serialize`; `Debug` zeigt nur `SudoSecret(<redacted>)`.
/// Die Bytes verlassen dieses Crate nie: nur [`Self::line`] (crate-intern)
/// baut die Zeile für die stdin-Pipe von `sudo`.
pub struct SudoSecret(Zeroizing<Vec<u8>>);

impl SudoSecret {
    /// Übernimmt ein Passwort.
    ///
    /// # Argumente
    /// - `bytes` (`Zeroizing<Vec<u8>>`): das Passwort ohne Zeilenende.
    ///
    /// # Errors
    /// [`SudoSecretError`] bei leerem, zu langem Passwort oder einem Byte
    /// `\n`/`\r`/`\0`. `bytes` wird in jedem Fall beim Verlassen genullt.
    pub fn from_zeroizing(bytes: Zeroizing<Vec<u8>>) -> Result<Self, SudoSecretError> {
        if bytes.is_empty() {
            return Err(SudoSecretError::Empty);
        }
        if bytes.len() > SUDO_MAX_SECRET_BYTES {
            return Err(SudoSecretError::TooLong);
        }
        if bytes.iter().any(|byte| matches!(byte, b'\n' | b'\r' | 0)) {
            return Err(SudoSecretError::ForbiddenByte);
        }
        Ok(Self(bytes))
    }

    /// Legt eine zweite, unabhängige Kopie an (exakte Kapazität, ebenfalls
    /// beim Drop genullt) — für das Sitzungs-Merken in der TUI.
    #[must_use]
    pub fn try_clone(&self) -> Self {
        let mut copy = Vec::with_capacity(self.0.len());
        copy.extend_from_slice(&self.0);
        Self(Zeroizing::new(copy))
    }

    /// Die Zeile für `sudo -S`: Passwort plus `\n`, mit exakt passender
    /// Kapazität angelegt (kein Umkopieren beim Anhängen).
    pub(crate) fn line(&self) -> Zeroizing<Vec<u8>> {
        let mut line = Vec::with_capacity(self.0.len().saturating_add(1));
        line.extend_from_slice(&self.0);
        line.push(b'\n');
        Zeroizing::new(line)
    }
}

impl Zeroize for SudoSecret {
    fn zeroize(&mut self) {
        self.0.zeroize();
    }
}

// `Zeroizing<Vec<u8>>` nullt beim Drop; `SudoSecret` hat kein eigenes Drop,
// der innere Wert wird also immer genullt.
impl ZeroizeOnDrop for SudoSecret {}

impl fmt::Debug for SudoSecret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SudoSecret(<redacted>)")
    }
}

// ── Frage- und Antwortvertrag ─────────────────────────────────────────────────

/// Die Antwort des Freigabefensters.
///
/// # Varianten
/// - [`Self::ApproveOnce`]: Freigabe ohne Geheimnis — nur gültig, wenn die
///   Frage [`SudoPrompt::passwordless`] meldet. Auf dem Passwort-Weg gilt sie
///   als Ablehnung (fail-closed).
/// - [`Self::ApproveSession`]: Freigabe mit dem in der TUI gemerkten
///   Sitzungspasswort (jeder Befehl wird trotzdem einzeln freigegeben).
/// - [`Self::Password`]: Freigabe mit frisch eingegebenem Passwort;
///   `remember` sagt nur, ob die TUI es für die Sitzung behalten hat (für
///   das Audit), die Ausführung ändert es nicht.
///
/// `Debug` zeigt nie das Geheimnis (siehe [`SudoSecret`]).
#[derive(Debug)]
pub enum SudoAnswer {
    /// Passwortlos freigegeben.
    ApproveOnce,
    /// Mit dem gemerkten Sitzungspasswort freigegeben.
    ApproveSession {
        /// Kopie des gemerkten Passworts.
        secret: SudoSecret,
    },
    /// Mit frisch eingegebenem Passwort freigegeben.
    Password {
        /// Das eingegebene Passwort.
        secret: SudoSecret,
        /// Ob die TUI das Passwort für diese Sitzung gemerkt hat.
        remember: bool,
    },
}

/// Rückruf, den die Ausführung bei fehlgeschlagener Authentifizierung
/// genau einmal aufruft — die TUI löscht damit ihr gemerktes Passwort.
pub struct SudoAuthFailureHook(Box<dyn FnOnce() + Send>);

impl SudoAuthFailureHook {
    /// Baut den Rückruf.
    #[must_use]
    pub fn new(callback: impl FnOnce() + Send + 'static) -> Self {
        Self(Box::new(callback))
    }

    fn fire(self) {
        (self.0)();
    }
}

impl fmt::Debug for SudoAuthFailureHook {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SudoAuthFailureHook")
    }
}

/// Was über den Antwortkanal zurückkommt.
#[derive(Debug)]
struct SudoReply {
    answer: SudoAnswer,
    on_auth_failure: Option<SudoAuthFailureHook>,
}

/// Sendeseite des sudo-Fragekanals.
pub type SudoPromptSender = mpsc::UnboundedSender<SudoPrompt>;

/// Empfängerseite des sudo-Fragekanals (pollt die TUI).
pub type SudoPromptReceiver = mpsc::UnboundedReceiver<SudoPrompt>;

/// Baut einen frischen sudo-Fragekanal.
#[must_use]
pub fn sudo_prompt_channel() -> (SudoPromptSender, SudoPromptReceiver) {
    mpsc::unbounded_channel()
}

/// Eine einzelne sudo-Frage auf dem Weg zum Freigabefenster.
///
/// # Beschreibung
/// Trägt nur Anzeige-Daten (Sitzung, Worker, exaktes argv, cwd, Grund,
/// Passwortlos-Kennzeichen) und die Antwortseite. Ein Drop ohne Antwort ist
/// eine Ablehnung.
pub struct SudoPrompt {
    session: String,
    worker: String,
    argv: Vec<String>,
    cwd: PathBuf,
    reason: String,
    passwordless: bool,
    responder: oneshot::Sender<Option<SudoReply>>,
}

/// Wartende Seite einer [`SudoPrompt`] (nur für die Ausführung und Tests).
pub struct SudoAnswerReceiver(oneshot::Receiver<Option<SudoReply>>);

impl fmt::Debug for SudoAnswerReceiver {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SudoAnswerReceiver")
    }
}

impl SudoAnswerReceiver {
    /// Wartet auf die Antwort. `None` bei Ablehnung oder fallengelassener
    /// Frage.
    async fn recv(self) -> Option<SudoReply> {
        self.0.await.ok().flatten()
    }

    /// Wartet auf die Antwort und liefert nur die [`SudoAnswer`] (ohne
    /// Rückruf) — für Tests der anzeigenden Oberfläche. `None` bei
    /// Ablehnung oder fallengelassener Frage.
    pub async fn into_answer(self) -> Option<SudoAnswer> {
        self.recv().await.map(|reply| reply.answer)
    }
}

impl fmt::Debug for SudoPrompt {
    /// Zeigt nur Anzeige-Daten; die Antwortseite erscheint als Platzhalter.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SudoPrompt")
            .field("session", &self.session)
            .field("worker", &self.worker)
            .field("argv", &self.argv)
            .field("cwd", &self.cwd)
            .field("reason", &self.reason)
            .field("passwordless", &self.passwordless)
            .finish_non_exhaustive()
    }
}

impl SudoPrompt {
    /// Baut eine Frage plus die wartende Seite.
    ///
    /// # Argumente
    /// - `session`: die Sitzung des anfragenden Agenten.
    /// - `worker`: die Rolle des anfragenden Workers (Anzeige).
    /// - `argv`: das exakte, bereits geprüfte argv.
    /// - `cwd`: das Arbeitsverzeichnis des Befehls.
    /// - `reason`: der Grund laut Modell (Anzeige).
    /// - `passwordless`: `true`, wenn `sudo -n -k true` gelang.
    #[must_use]
    pub fn new(
        session: String,
        worker: String,
        argv: Vec<String>,
        cwd: PathBuf,
        reason: String,
        passwordless: bool,
    ) -> (Self, SudoAnswerReceiver) {
        let (responder, answer) = oneshot::channel();
        (
            Self {
                session,
                worker,
                argv,
                cwd,
                reason,
                passwordless,
                responder,
            },
            SudoAnswerReceiver(answer),
        )
    }

    /// Die Sitzung des anfragenden Agenten.
    #[must_use]
    pub fn session(&self) -> &str {
        &self.session
    }

    /// Die Rolle des anfragenden Workers.
    #[must_use]
    pub fn worker(&self) -> &str {
        &self.worker
    }

    /// Das exakte argv, das nach einer Freigabe läuft.
    #[must_use]
    pub fn argv(&self) -> &[String] {
        &self.argv
    }

    /// Das Arbeitsverzeichnis des Befehls.
    #[must_use]
    pub fn cwd(&self) -> &Path {
        &self.cwd
    }

    /// Der Grund laut Modell.
    #[must_use]
    pub fn reason(&self) -> &str {
        &self.reason
    }

    /// `true`, wenn sudo ohne Passwort läuft (nur „Freigeben / Ablehnen“).
    #[must_use]
    pub fn passwordless(&self) -> bool {
        self.passwordless
    }

    /// Beantwortet die Frage mit einer Freigabe.
    ///
    /// # Returns
    /// `true`, wenn die Antwort die wartende Ausführung erreicht hat. Bei
    /// `false` wird das Geheimnis sofort genullt (Drop).
    pub fn approve(self, answer: SudoAnswer) -> bool {
        self.reply(Some(SudoReply {
            answer,
            on_auth_failure: None,
        }))
    }

    /// Wie [`Self::approve`], mit Rückruf bei fehlgeschlagener
    /// Authentifizierung (Sitzungs-Merken der TUI).
    pub fn approve_with_failure_hook(self, answer: SudoAnswer, hook: SudoAuthFailureHook) -> bool {
        self.reply(Some(SudoReply {
            answer,
            on_auth_failure: Some(hook),
        }))
    }

    /// Lehnt die Frage ab.
    pub fn deny(self) -> bool {
        self.reply(None)
    }

    fn reply(self, reply: Option<SudoReply>) -> bool {
        let approved = reply.is_some();
        let delivered = self.responder.send(reply).is_ok();
        if delivered {
            tracing::debug!(session = %self.session, approved, "host.sudo_exec.answer_delivered");
        } else {
            tracing::warn!(session = %self.session, approved, "host.sudo_exec.answer_undeliverable");
        }
        delivered
    }
}

// ── Audit ─────────────────────────────────────────────────────────────────────

/// Ein Audit-Datensatz je `host.sudo_exec`-Aufruf (Plan Teil B, Punkt 7).
///
/// # Beschreibung
/// Enthält **kein** Feld für ein Geheimnis. `Redact` zeigt alle Felder über
/// `Display`; `argv` ist bereits eine lesbare, gequotete Darstellung,
/// `argv_hash` der BLAKE3-Hash der längenpräfixierten argv-Elemente.
/// Abgebildet auf `harw_secrets::audit::AuditEvent` entspricht das
/// `Actor::Operator(operator)`, `action = "host.sudo_exec"` und den übrigen
/// Feldern als `SubjectRef`s (Verdrahtung in eine persistierte Kette ist
/// Sache der Runtime).
#[derive(Debug, Clone, PartialEq, Eq, harw_macros::Redact)]
pub struct SudoAuditRecord {
    /// Lokaler Operator (Benutzername des Harness-Prozesses).
    #[redact(show)]
    pub operator: String,
    /// Sitzung des anfragenden Agenten.
    #[redact(show)]
    pub session: String,
    /// Rolle des anfragenden Workers.
    #[redact(show)]
    pub worker: String,
    /// Lesbare Darstellung des argv.
    #[redact(show)]
    pub argv: String,
    /// BLAKE3 über die längenpräfixierten argv-Elemente (Hex).
    #[redact(show)]
    pub argv_hash: String,
    /// `passwordless`, `password_once`, `password_session_new`,
    /// `password_session_reused` oder `none` (keine Ausführung).
    #[redact(show)]
    pub mode: &'static str,
    /// `approved`, `denied`, `no_ui`, `invalid`, `auth_failed`,
    /// `password_required`, `timeout`, `cancelled`, `error`.
    #[redact(show)]
    pub decision: &'static str,
    /// Exit-Code (`-` ohne Ausführung oder bei Signal).
    #[redact(show)]
    pub exit_code: String,
    /// Dauer ab Aufruf in Millisekunden.
    #[redact(show)]
    pub duration_ms: u64,
}

/// Ziel für [`SudoAuditRecord`]s.
pub trait SudoAuditSink: Send + Sync {
    /// Zeichnet einen Datensatz auf. Darf nicht blockieren.
    fn record(&self, record: &SudoAuditRecord);
}

/// Vorgabe-Ziel: ein `tracing`-Ereignis mit Ziel `harw::audit` und der
/// `Redact`-Form des Datensatzes.
#[derive(Debug, Default, Clone, Copy)]
pub struct TracingSudoAudit;

impl SudoAuditSink for TracingSudoAudit {
    fn record(&self, record: &SudoAuditRecord) {
        let rendered = match harw_observe::Redact::redact(record) {
            harw_observe::Redacted::Shown(text) | harw_observe::Redacted::Hashed(text) => text,
            harw_observe::Redacted::Omitted => String::new(),
        };
        info!(target: "harw::audit", audit = %rendered, "host.sudo_exec.audit");
    }
}

/// Lesbare, eindeutige Darstellung eines argv (einfache Anführungszeichen
/// um jedes Element mit Leer- oder Sonderzeichen).
#[must_use]
pub fn display_argv(argv: &[String]) -> String {
    argv.iter()
        .map(|arg| {
            let plain = !arg.is_empty()
                && arg.chars().all(|c| {
                    c.is_ascii_alphanumeric()
                        || matches!(c, '-' | '_' | '.' | '/' | '=' | ':' | ',' | '+' | '@' | '%')
                });
            if plain {
                arg.clone()
            } else {
                format!("'{}'", arg.replace('\'', "'\\''"))
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// BLAKE3 über die längenpräfixierten argv-Elemente (Hex).
#[must_use]
pub fn argv_hash(argv: &[String]) -> String {
    let mut hasher = blake3::Hasher::new();
    for arg in argv {
        let len = u64::try_from(arg.len()).unwrap_or(u64::MAX);
        hasher.update(&len.to_le_bytes());
        hasher.update(arg.as_bytes());
    }
    hasher.finalize().to_hex().to_string()
}

// ── Prüfungen ─────────────────────────────────────────────────────────────────

/// `true` für Zeichen, die im Freigabefenster täuschen könnten: Steuerzeichen
/// sowie Bidi-, Zero-Width- und Tag-Zeichen.
fn is_deceptive_char(c: char) -> bool {
    c.is_control()
        || matches!(
            c,
            '\u{00AD}'
                | '\u{061C}'
                | '\u{180E}'
                | '\u{200B}'..='\u{200F}'
                | '\u{2028}'..='\u{202E}'
                | '\u{2060}'..='\u{206F}'
                | '\u{FEFF}'
                | '\u{FFF9}'..='\u{FFFB}'
                | '\u{E0000}'..='\u{E007F}'
        )
}

/// Dateiname eines Pfads (`/usr/bin/sudo` → `sudo`).
fn basename(word: &str) -> &str {
    word.rsplit('/').next().unwrap_or(word)
}

/// Prüft, ob `word` (Basisname) ein Rechte-Werkzeug ist.
fn escalation_name(word: &str) -> Option<&'static str> {
    let name = basename(word);
    ESCALATION_PROGRAMS
        .iter()
        .copied()
        .find(|candidate| *candidate == name)
}

/// Shell-Interpreter, deren `-c`-Argument selbst eine Befehlszeile ist
/// (`bash -c 'sudo id'`).
const SHELL_INTERPRETERS: &[&str] = &["sh", "bash", "zsh", "dash", "ksh", "mksh", "ash"];

/// Höchste Verschachtelung von `sh -c '…'`, die [`escalation_program`]
/// noch auswertet (Schutz vor Rekursion ohne Ende).
const MAX_SHELL_C_DEPTH: u8 = 4;

/// Wo der Zerleger in [`command_segments`] gerade steht.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ShellFrame {
    /// Befehlskontext; `closer` sagt, was ihn beendet.
    Command { closer: FrameCloser, parens: u32 },
    /// In `'…'`: alles ist Literal.
    Single,
    /// In `"…"`: Literal bis auf `\`, `$(` und Backtick.
    Double,
}

/// Was einen verschachtelten Befehlskontext beendet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FrameCloser {
    /// Oberste Ebene: nur das Zeilenende.
    Top,
    /// `$(` … `)`.
    Paren,
    /// `` ` `` … `` ` ``.
    Backtick,
}

/// Gesicherter Zerlegerstand vor einer Befehlsersetzung.
struct SavedWord {
    word: String,
    word_started: bool,
    segment: Vec<String>,
}

/// Schließt das aktuelle Wort ab (auch ein leeres `""`).
fn end_word(word: &mut String, word_started: &mut bool, segment: &mut Vec<String>) {
    if *word_started || !word.is_empty() {
        segment.push(std::mem::take(word));
    }
    *word_started = false;
}

/// Schließt den aktuellen einfachen Befehl ab.
fn end_segment(segment: &mut Vec<String>, segments: &mut Vec<Vec<String>>) {
    if !segment.is_empty() {
        segments.push(std::mem::take(segment));
    }
}

/// Zerlegt eine Shell-Zeile anführungszeichengerecht in **einfache Befehle**
/// (je eine Wortliste).
///
/// # Beschreibung
/// Ein neuer Befehl beginnt nach `;`, `&`, `&&`, `|`, `||`, `(`, `)`,
/// Zeilenende, am Anfang von `$(…)` und `` `…` `` (auch innerhalb von
/// `"…"`) und am Ende einer solchen Ersetzung. Innerhalb von `'…'` und
/// `"…"` ist nichts davon ein Trenner: ein Suchmuster wie
/// `'sudo_exec|sudo -'` bleibt **ein** Argument. Anführungszeichen und
/// Backslashes werden wie in der Shell entfernt (`s"u"do` → `sudo`),
/// `# …` am Wortanfang ist ein Kommentar. Keine vollständige Shell-Grammatik
/// (Here-Docs, `case`-Muster und Umleitungsziele bleiben gewöhnliche Wörter).
fn command_segments(command: &str) -> Vec<Vec<String>> {
    let mut segments: Vec<Vec<String>> = Vec::new();
    let mut segment: Vec<String> = Vec::new();
    let mut word = String::new();
    let mut word_started = false;
    let mut stack = vec![ShellFrame::Command {
        closer: FrameCloser::Top,
        parens: 0,
    }];
    let mut saved: Vec<SavedWord> = Vec::new();

    let mut chars = command.chars().peekable();
    while let Some(c) = chars.next() {
        let Some(frame) = stack.last().copied() else {
            break;
        };
        // Beginn einer Befehlsersetzung (`$(` oder Backtick) aus Befehls-
        // oder Doppelquote-Kontext: aktuellen Stand sichern, neu beginnen.
        let opens_substitution = match frame {
            ShellFrame::Single => false,
            ShellFrame::Double => (c == '$' && chars.peek() == Some(&'(')) || c == '`',
            ShellFrame::Command { closer, .. } => {
                (c == '$' && chars.peek() == Some(&'('))
                    || (c == '`' && closer != FrameCloser::Backtick)
            }
        };
        if opens_substitution {
            let closer = if c == '$' {
                chars.next();
                FrameCloser::Paren
            } else {
                FrameCloser::Backtick
            };
            saved.push(SavedWord {
                word: std::mem::take(&mut word),
                word_started,
                segment: std::mem::take(&mut segment),
            });
            word_started = false;
            stack.push(ShellFrame::Command { closer, parens: 0 });
            continue;
        }
        match frame {
            ShellFrame::Single => {
                if c == '\'' {
                    stack.pop();
                } else {
                    word.push(c);
                }
            }
            ShellFrame::Double => match c {
                '"' => {
                    stack.pop();
                }
                '\\' => match chars.next() {
                    Some('\n') | None => {}
                    Some(next @ ('$' | '`' | '"' | '\\')) => word.push(next),
                    Some(next) => {
                        word.push('\\');
                        word.push(next);
                    }
                },
                other => word.push(other),
            },
            ShellFrame::Command { closer, parens } => {
                let closes = (c == ')' && parens == 0 && closer == FrameCloser::Paren)
                    || (c == '`' && closer == FrameCloser::Backtick);
                if closes {
                    end_word(&mut word, &mut word_started, &mut segment);
                    end_segment(&mut segment, &mut segments);
                    stack.pop();
                    if let Some(outer) = saved.pop() {
                        word = outer.word;
                        segment = outer.segment;
                    }
                    // Die Ersetzung ist Teil des umgebenden Wortes.
                    word_started = true;
                    continue;
                }
                match c {
                    '\'' => {
                        word_started = true;
                        stack.push(ShellFrame::Single);
                    }
                    '"' => {
                        word_started = true;
                        stack.push(ShellFrame::Double);
                    }
                    '\\' => match chars.next() {
                        Some('\n') | None => {}
                        Some(next) => word.push(next),
                    },
                    '#' if word.is_empty() && !word_started => {
                        while chars.peek().is_some_and(|next| *next != '\n') {
                            chars.next();
                        }
                    }
                    ';' | '&' | '|' | '\n' | '\r' => {
                        end_word(&mut word, &mut word_started, &mut segment);
                        end_segment(&mut segment, &mut segments);
                    }
                    '(' | ')' => {
                        end_word(&mut word, &mut word_started, &mut segment);
                        end_segment(&mut segment, &mut segments);
                        if let Some(ShellFrame::Command { parens, .. }) = stack.last_mut() {
                            if c == '(' {
                                *parens += 1;
                            } else {
                                *parens = parens.saturating_sub(1);
                            }
                        }
                    }
                    other if other.is_whitespace() => {
                        end_word(&mut word, &mut word_started, &mut segment);
                    }
                    other => word.push(other),
                }
            }
        }
    }
    end_word(&mut word, &mut word_started, &mut segment);
    end_segment(&mut segment, &mut segments);
    // Nicht geschlossene Ersetzungen: ihre äußeren Befehle zählen mit.
    while let Some(outer) = saved.pop() {
        let mut outer_segment = outer.segment;
        if outer.word_started || !outer.word.is_empty() {
            outer_segment.push(outer.word);
        }
        end_segment(&mut outer_segment, &mut segments);
    }
    segments
}

/// Findet ein Rechte-Werkzeug in **Befehlsposition** einer Shell-Zeile.
///
/// # Beschreibung
/// Zerlegt die Zeile anführungszeichengerecht in einfache Befehle
/// ([`command_segments`]: Trenner `;`, `&`, `&&`, `|`, `||`, `(`, `)`,
/// Zeilenende, `$(…)`, `` `…` ``) und prüft je Befehl das erste Wort nach
/// Zuweisungen (`VAR=wert`), Wrappern wie `env`/`exec`/`nohup`/`time`/
/// `xargs`/`timeout` samt deren Optionen/Zahlen und Schlüsselwörtern wie
/// `then`. `sh -c '…'`/`bash -c "…"` wird rekursiv geprüft. Ein Wort
/// **innerhalb** eines gequoteten Arguments (`rg 'sudo -|x'`,
/// `git commit -m 'use sudo'`) und ein Teilwort (`sudo_exec`,
/// `/etc/sudoers`) sind nie ein Treffer. Das ist eine Heuristik gegen
/// versehentliches und naives `sudo` über `shell.exec`, keine vollständige
/// Shell-Analyse — die eigentliche Grenze bleibt, dass `shell.exec` auf dem
/// Host jeden Befehlstext einzeln freigeben lässt und in der Sandbox
/// `no_new_privs` gilt.
///
/// # Rückgabe
/// Der Name des gefundenen Werkzeugs, sonst `None`.
#[must_use]
pub fn escalation_program(command: &str) -> Option<&'static str> {
    escalation_program_at_depth(command, 0)
}

/// [`escalation_program`] mit Rekursionstiefe für `sh -c`.
fn escalation_program_at_depth(command: &str, depth: u8) -> Option<&'static str> {
    for segment in command_segments(command) {
        let mut after_wrapper = false;
        let mut words = segment.iter();
        while let Some(word) = words.next() {
            let is_assignment = word
                .split_once('=')
                .is_some_and(|(name, _)| !name.is_empty() && !name.contains('/'));
            if is_assignment {
                continue;
            }
            if after_wrapper
                && (word.starts_with('-') || word.chars().all(|c| c.is_ascii_digit() || c == '.'))
            {
                continue;
            }
            if COMMAND_PREFIX_WORDS.contains(&basename(word)) {
                after_wrapper = true;
                continue;
            }
            if let Some(found) = escalation_name(word) {
                return Some(found);
            }
            if depth < MAX_SHELL_C_DEPTH && SHELL_INTERPRETERS.contains(&basename(word)) {
                // `bash -c 'cmd'`, `sh -lc "cmd"`: das Argument nach der
                // ersten Option mit `c` ist selbst eine Befehlszeile.
                let mut rest = words.by_ref().skip_while(|arg| {
                    !(arg.starts_with('-') && !arg.starts_with("--") && arg.contains('c'))
                });
                if rest.next().is_some() {
                    if let Some(script) = rest.next() {
                        if let Some(found) = escalation_program_at_depth(script, depth + 1) {
                            return Some(found);
                        }
                    }
                }
            }
            break;
        }
    }
    None
}

/// Meldung für `shell.exec`, wenn [`escalation_program`] anschlägt.
///
/// # Beschreibung
/// `shell.exec` kennt die Rolle des Aufrufers nicht; die Meldung nennt
/// deshalb alle drei Wege in Reihenfolge: selbst `host.sudo_exec` rufen
/// (nur `uia-shell-worker`/`host-process-worker` in der TUI), sonst an
/// `uia-shell-worker` delegieren, sonst den Schritt mit exaktem argv an den
/// Elternteil/die UIA zurückgeben. Sie sagt ausdrücklich, dass sudo
/// **möglich** ist — ohne diesen Satz gaben Modelle auf („sudo geht nicht“)
/// oder reichten den Befehl an die Nutzerin weiter.
#[must_use]
pub fn shell_escalation_message(program: &str) -> String {
    format!(
        "shell.exec: `{program}` läuft nicht über shell.exec — sudo selbst funktioniert aber: \
         Root-Befehle laufen über das Werkzeug `{SUDO_EXEC_TOOL}` mit exaktem argv (ohne \
         `{program}`) und Grund. Der Nutzer bestätigt den exakten Befehl im Freigabefenster \
         der TUI und gibt dort sein Passwort ein, falls sudo eines verlangt (passwortloses \
         sudo geht ebenso). So gehst du vor: Hast du `{SUDO_EXEC_TOOL}` (uia-shell-worker, \
         host-process-worker), rufe es jetzt auf. Sonst delegiere den Schritt mit \
         `transfer_to_uia-shell-worker` (exakter Befehl + Grund), wenn du das Werkzeug hast; \
         andernfalls gib ihn mit exaktem argv und Grund an deinen Elternteil bzw. die UIA \
         zurück (Ergebnis oder `parent.message`). Nie ein Passwort in Chat oder Befehl, nie \
         `sudo -S` oder `echo … | sudo`. Sag nie, sudo sei unmöglich. Nur ohne TUI (serve, \
         telegram, one-shot) fehlt dieser Weg — dann nenne dem Nutzer den exakten Befehl zum \
         Selbstausführen."
    )
}

/// Prüft argv und Grund.
///
/// # Errors
/// Eine für das Modell lesbare Meldung (ohne Wiedergabe verdächtiger
/// Zeichen).
fn validate_request(argv: &[String], reason: &str) -> Result<(), String> {
    let Some(program) = argv.first() else {
        return Err("argv darf nicht leer sein".to_owned());
    };
    if argv.len() > MAX_ARGV_LEN {
        return Err(format!("argv hat mehr als {MAX_ARGV_LEN} Elemente"));
    }
    if program.trim().is_empty() {
        return Err("argv[0] darf nicht leer sein".to_owned());
    }
    if program.starts_with('-') {
        return Err("argv[0] darf nicht mit '-' beginnen".to_owned());
    }
    if let Some(found) = escalation_name(program) {
        return Err(format!(
            "argv[0] ist `{found}` — nenne nur den eigentlichen Befehl, host.sudo_exec setzt \
             sudo selbst davor"
        ));
    }
    for (index, arg) in argv.iter().enumerate() {
        if arg.len() > MAX_ARG_BYTES {
            return Err(format!(
                "argv[{index}] ist länger als {MAX_ARG_BYTES} Bytes"
            ));
        }
        if arg.chars().any(is_deceptive_char) {
            return Err(format!(
                "argv[{index}] enthält Steuer- oder unsichtbare Formatzeichen"
            ));
        }
    }
    if reason.trim().is_empty() {
        return Err("reason darf nicht leer sein".to_owned());
    }
    if reason.chars().count() > MAX_REASON_CHARS {
        return Err(format!("reason ist länger als {MAX_REASON_CHARS} Zeichen"));
    }
    if reason.chars().any(is_deceptive_char) {
        return Err("reason enthält Steuer- oder unsichtbare Formatzeichen".to_owned());
    }
    Ok(())
}

/// Findet `sudo` an den festen Pfaden.
fn resolve_sudo() -> Option<PathBuf> {
    SUDO_CANDIDATES
        .iter()
        .map(Path::new)
        .find(|path| path.is_file())
        .map(Path::to_path_buf)
}

/// Frische, zufällige Einmal-Marke für `-p` (nur `[0-9a-f]`, damit `sudo`
/// keine `%`-Ersetzung vornimmt).
fn fresh_nonce() -> String {
    let token = uuid_like();
    format!("harw-sudo-{token}:")
}

/// 128 Bit Zufall als Hex, ohne neue Abhängigkeit: `RandomState` wird vom
/// Betriebssystem geseedet; zwei unabhängige Hasher liefern je 64 Bit.
fn uuid_like() -> String {
    use std::collections::hash_map::RandomState;
    use std::hash::{BuildHasher, Hasher};
    let mut parts = [0_u64; 2];
    for (index, part) in parts.iter_mut().enumerate() {
        let mut hasher = RandomState::new().build_hasher();
        hasher.write_usize(index);
        hasher.write_u128(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |elapsed| elapsed.as_nanos()),
        );
        *part = hasher.finish();
    }
    format!("{:016x}{:016x}", parts[0], parts[1])
}

/// Zählt vollständige Vorkommen von `needle` im fortlaufenden Strom; `tail`
/// hält höchstens `needle.len() - 1` Bytes über Chunk-Grenzen hinweg.
fn scan_for_marker(tail: &mut Vec<u8>, chunk: &[u8], needle: &[u8]) -> u32 {
    if needle.is_empty() {
        return 0;
    }
    tail.extend_from_slice(chunk);
    let mut count = 0_u32;
    let mut position = 0_usize;
    while let Some(found) = find_subslice(&tail[position..], needle) {
        count = count.saturating_add(1);
        position += found + needle.len();
    }
    let keep_from = position.max(tail.len().saturating_sub(needle.len() - 1));
    tail.drain(..keep_from);
    count
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

/// Entfernt alle Vorkommen von `needle`.
fn strip_marker(bytes: &[u8], needle: &[u8]) -> Vec<u8> {
    if needle.is_empty() {
        return bytes.to_vec();
    }
    let mut out = Vec::with_capacity(bytes.len());
    let mut rest = bytes;
    while let Some(found) = find_subslice(rest, needle) {
        out.extend_from_slice(&rest[..found]);
        rest = &rest[found + needle.len()..];
    }
    out.extend_from_slice(rest);
    out
}

/// Bereinigt die stderr für das Modell: Einmal-Marke und `sudo`-Prompt-Zeilen
/// entfernen.
fn clean_stderr(stderr: &[u8], marker: &[u8]) -> String {
    let stripped = strip_marker(stderr, marker);
    String::from_utf8_lossy(&stripped)
        .lines()
        .filter(|line| {
            let trimmed = line.trim_start();
            !PROMPT_LINE_PREFIXES
                .iter()
                .any(|prefix| trimmed.starts_with(prefix))
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// `true`, wenn stderr (klein geschrieben) eine Authentifizierungs-Meldung
/// von `sudo` trägt.
fn stderr_reports_auth_failure(stderr: &[u8]) -> bool {
    let lower = String::from_utf8_lossy(stderr).to_lowercase();
    AUTH_FAILURE_MARKERS
        .iter()
        .any(|marker| lower.contains(marker))
}

// ── Ausführung ────────────────────────────────────────────────────────────────

/// Welcher Weg läuft.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SudoMode {
    Passwordless,
    Password,
}

/// Gekappte stdout/stderr-Erfassung mit gemeinsamem Budget.
#[derive(Debug)]
struct SudoCapture {
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    retain: usize,
}

impl SudoCapture {
    fn new(limit: usize) -> Self {
        Self {
            stdout: Vec::new(),
            stderr: Vec::new(),
            retain: limit.saturating_add(1),
        }
    }

    fn retained(&self) -> usize {
        self.stdout.len() + self.stderr.len()
    }

    fn limit_exceeded(&self) -> bool {
        self.retained() >= self.retain
    }

    fn push(&mut self, to_stdout: bool, bytes: &[u8]) {
        let room = self.retain.saturating_sub(self.retained());
        let kept = &bytes[..bytes.len().min(room)];
        if to_stdout {
            self.stdout.extend_from_slice(kept);
        } else {
            self.stderr.extend_from_slice(kept);
        }
    }
}

/// Ergebnis eines Laufs.
#[derive(Debug)]
struct RunReport {
    status: Option<ExitStatus>,
    timed_out: bool,
    limit_exceeded: bool,
    prompts_seen: u32,
    password_sent: bool,
    capture: SudoCapture,
}

/// Wartet auf `cancel` oder nie.
async fn cancelled(cancel: Option<&CancelToken>) {
    match cancel {
        Some(cancel) => cancel.cancelled().await,
        None => std::future::pending::<()>().await,
    }
}

/// Wartet bis `deadline`, oder nie (`None`).
async fn sleep_until_opt(deadline: Option<tokio::time::Instant>) {
    match deadline {
        Some(deadline) => tokio::time::sleep_until(deadline).await,
        None => std::future::pending::<()>().await,
    }
}

/// Schreibt die Passwortzeile und schließt die Pipe (Drop).
async fn send_password(stdin: ChildStdin, line: &Zeroizing<Vec<u8>>) -> bool {
    let mut stdin = stdin;
    let written = tokio::time::timeout(PASSWORD_WRITE_TIMEOUT, stdin.write_all(line)).await;
    let ok = matches!(written, Ok(Ok(())));
    let _ = tokio::time::timeout(PASSWORD_WRITE_TIMEOUT, stdin.shutdown()).await;
    drop(stdin);
    ok
}

/// Ausführer eines `host.sudo_exec`-Aufrufs.
struct SudoExecExecutor {
    prompts: SudoPromptSender,
    worker: String,
    timeout_secs: u64,
    max_output_bytes: usize,
    limits: ShellLimits,
    prompt_timeout: Duration,
    password_window: Duration,
    audit: Arc<dyn SudoAuditSink>,
    sudo_path: Option<PathBuf>,
}

/// Eingabe des Werkzeugs; unbekannte Felder werden abgelehnt.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SudoExecArgs {
    /// Exaktes argv des Root-Befehls (ohne `sudo`).
    argv: Vec<String>,
    /// Begründung für das Freigabefenster.
    reason: String,
}

/// Der Teil eines Audit-Datensatzes, der beim Aufruf feststeht.
struct AuditBase<'a> {
    session: &'a str,
    argv: &'a [String],
    started: Instant,
}

impl SudoExecExecutor {
    fn audit(
        &self,
        base: &AuditBase<'_>,
        mode: &'static str,
        decision: &'static str,
        exit_code: Option<i32>,
    ) {
        let record = SudoAuditRecord {
            operator: std::env::var("USER").unwrap_or_else(|_| "unknown".to_owned()),
            session: base.session.to_owned(),
            worker: self.worker.clone(),
            argv: display_argv(base.argv),
            argv_hash: argv_hash(base.argv),
            mode,
            decision,
            exit_code: exit_code.map_or_else(|| "-".to_owned(), |code| code.to_string()),
            duration_ms: u64::try_from(base.started.elapsed().as_millis()).unwrap_or(u64::MAX),
        };
        self.audit.record(&record);
    }

    /// Der `sudo`-Pfad: Test-Override oder feste Kandidaten.
    fn sudo_binary(&self) -> Option<PathBuf> {
        self.sudo_path.clone().or_else(resolve_sudo)
    }

    /// `sudo -n -k true`: gelingt es, läuft sudo ohne Passwort.
    async fn probe_passwordless(&self, sudo: &Path) -> bool {
        let mut command = TokioCommand::new(sudo);
        command
            .args(["-n", "-k", "true"])
            .env_clear()
            .env("PATH", SUDO_ENV_PATH)
            .env("LANG", SUDO_ENV_LANG)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        let Ok(mut child) = command.spawn() else {
            return false;
        };
        match tokio::time::timeout(PASSWORDLESS_PROBE_TIMEOUT, child.wait()).await {
            Ok(Ok(status)) => status.success(),
            _ => {
                terminate(&mut child).await;
                false
            }
        }
    }

    /// Baut den vollständigen Start: `[prlimit … --] [setsid --wait] sudo …`.
    fn build_command(
        &self,
        sudo: &Path,
        mode: SudoMode,
        marker: &str,
        argv: &[String],
        cwd: &Path,
    ) -> Result<TokioCommand, String> {
        self.limits
            .validate()
            .map_err(|err| format!("host.sudo_exec: Ressourcengrenzen: {err}"))?;
        let prlimit = self
            .limits
            .resolve_prlimit()
            .map_err(|err| format!("host.sudo_exec: Ressourcengrenzen: {err}"))?;
        if prlimit.is_none() {
            warn!("host.sudo_exec runs WITHOUT rlimits: prlimit missing and require_rlimits=false");
        }
        let mut sudo_args: Vec<OsString> = match mode {
            SudoMode::Passwordless => vec!["-n".into(), "-k".into()],
            SudoMode::Password => vec!["-S".into(), "-k".into(), "-p".into(), marker.into()],
        };
        sudo_args.push("--".into());
        sudo_args.extend(argv.iter().map(OsString::from));

        let setsid = resolve_setsid();
        let (program, args) = match setsid {
            Some(setsid) => {
                let mut args: Vec<OsString> = vec!["--wait".into(), sudo.as_os_str().to_owned()];
                args.extend(sudo_args);
                (setsid.to_path_buf(), args)
            }
            None => (sudo.to_path_buf(), sudo_args),
        };
        let launch = launch_command(prlimit.as_deref(), &self.limits, &program, &args);
        let mut command = TokioCommand::new(&launch.program);
        command
            .args(&launch.args)
            .current_dir(cwd)
            .env_clear()
            .env("PATH", SUDO_ENV_PATH)
            .env("LANG", SUDO_ENV_LANG)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .stdin(match mode {
                SudoMode::Passwordless => Stdio::null(),
                SudoMode::Password => Stdio::piped(),
            });
        // Wie `run_host_command`: ohne setsid wenigstens eine eigene
        // Prozessgruppe; mit setsid bewusst nicht (siehe dort).
        if setsid.is_none() {
            command.process_group(0);
        }
        Ok(command)
    }

    /// Startet, schreibt ggf. das Passwort nach der Einmal-Marke, sammelt
    /// die Ausgabe gekappt und hält Zeitlimit und Abbruch ein.
    ///
    /// # Errors
    /// `Err(Some(msg))` bei Start-/I/O-Fehlern, `Err(None)` bei Abbruch.
    async fn run(
        &self,
        mut command: TokioCommand,
        secret: Option<SudoSecret>,
        marker: &str,
        cancel: Option<&CancelToken>,
    ) -> Result<RunReport, Option<String>> {
        let mut child: Child = command.spawn().map_err(|err| {
            warn!(error = %err, "host.sudo_exec spawn failed");
            Some(format!("host.sudo_exec: Start fehlgeschlagen: {err}"))
        })?;
        // Die Zeile wird einmal gebaut; das `SudoSecret` selbst wird sofort
        // genullt (Drop), die Zeile beim Verlassen dieser Funktion.
        let line: Option<Zeroizing<Vec<u8>>> = secret.as_ref().map(SudoSecret::line);
        drop(secret);
        let mut stdin = child.stdin.take();
        let (Some(mut stdout), Some(mut stderr)) = (child.stdout.take(), child.stderr.take())
        else {
            terminate(&mut child).await;
            return Err(Some(
                "host.sudo_exec: stdout/stderr-Pipes fehlen".to_owned(),
            ));
        };

        let started = tokio::time::Instant::now();
        let deadline = started + Duration::from_secs(self.timeout_secs);
        let mut window = stdin.as_ref().map(|_| started + self.password_window);
        let marker_bytes = marker.as_bytes();
        let mut capture = SudoCapture::new(self.max_output_bytes);
        let mut tail: Vec<u8> = Vec::new();
        let mut prompts_seen = 0_u32;
        let mut password_sent = false;
        let mut stdout_open = true;
        let mut stderr_open = true;
        let mut out_buf = [0_u8; READ_CHUNK_BYTES];
        let mut err_buf = [0_u8; READ_CHUNK_BYTES];
        let mut timed_out = false;

        while stdout_open || stderr_open {
            if capture.limit_exceeded() {
                break;
            }
            tokio::select! {
                read = stdout.read(&mut out_buf), if stdout_open => {
                    match read {
                        Ok(0) => stdout_open = false,
                        Ok(count) => capture.push(true, &out_buf[..count]),
                        Err(err) => {
                            terminate(&mut child).await;
                            return Err(Some(format!("host.sudo_exec: I/O-Fehler: {err}")));
                        }
                    }
                }
                read = stderr.read(&mut err_buf), if stderr_open => {
                    match read {
                        Ok(0) => stderr_open = false,
                        Ok(count) => {
                            let chunk = &err_buf[..count];
                            capture.push(false, chunk);
                            prompts_seen = prompts_seen
                                .saturating_add(scan_for_marker(&mut tail, chunk, marker_bytes));
                            // Genau ein Versuch: nur auf die erste echte
                            // Passwortabfrage antworten, danach Pipe zu.
                            if prompts_seen >= 1 && !password_sent {
                                if let (Some(pipe), Some(line)) = (stdin.take(), line.as_ref()) {
                                    password_sent = send_password(pipe, line).await;
                                }
                                window = None;
                            }
                        }
                        Err(err) => {
                            terminate(&mut child).await;
                            return Err(Some(format!("host.sudo_exec: I/O-Fehler: {err}")));
                        }
                    }
                }
                () = sleep_until_opt(window) => {
                    // Keine Passwortabfrage in der Frist: Pipe ohne Passwort
                    // schließen (EOF für den Befehl bzw. Fehlschlag für sudo).
                    drop(stdin.take());
                    window = None;
                }
                () = tokio::time::sleep_until(deadline) => {
                    timed_out = true;
                    break;
                }
                () = cancelled(cancel) => {
                    drop(stdin.take());
                    terminate(&mut child).await;
                    info!("host.sudo_exec cancelled");
                    return Err(None);
                }
            }
        }
        drop(stdin.take());
        drop(line);

        let limit_exceeded = capture.limit_exceeded();
        let status = if timed_out || limit_exceeded {
            terminate(&mut child).await
        } else {
            tokio::select! {
                waited = tokio::time::timeout_at(deadline, child.wait()) => match waited {
                    Ok(Ok(status)) => Some(status),
                    Ok(Err(err)) => {
                        terminate(&mut child).await;
                        return Err(Some(format!("host.sudo_exec: Warten fehlgeschlagen: {err}")));
                    }
                    Err(_elapsed) => {
                        timed_out = true;
                        terminate(&mut child).await
                    }
                },
                () = cancelled(cancel) => {
                    terminate(&mut child).await;
                    return Err(None);
                }
            }
        };
        Ok(RunReport {
            status,
            timed_out,
            limit_exceeded,
            prompts_seen,
            password_sent,
            capture,
        })
    }

    /// Wertet einen Lauf aus und baut die Modell-Ausgabe.
    fn finish(
        &self,
        base: &AuditBase<'_>,
        mode: SudoMode,
        audit_mode: &'static str,
        marker: &str,
        report: RunReport,
        on_auth_failure: Option<SudoAuthFailureHook>,
    ) -> ToolOutput {
        let exit_code = report.status.and_then(|status| status.code());
        let auth_failed = mode == SudoMode::Password
            && (report.prompts_seen >= 2
                || (report.prompts_seen >= 1 && !report.password_sent)
                || (exit_code == Some(1) && stderr_reports_auth_failure(&report.capture.stderr)));
        if auth_failed {
            if let Some(hook) = on_auth_failure {
                hook.fire();
            }
            self.audit(base, audit_mode, "auth_failed", exit_code);
            warn!(
                session = base.session,
                "host.sudo_exec authentication failed"
            );
            return ToolOutput::error(AUTH_FAILED_MSG);
        }
        if mode == SudoMode::Passwordless
            && exit_code == Some(1)
            && stderr_reports_auth_failure(&report.capture.stderr)
        {
            self.audit(base, audit_mode, "password_required", exit_code);
            return ToolOutput::error(PASSWORD_REQUIRED_MSG);
        }
        let marker_bytes = marker.as_bytes();
        let stdout = String::from_utf8_lossy(&strip_marker(&report.capture.stdout, marker_bytes))
            .into_owned();
        let stderr = clean_stderr(&report.capture.stderr, marker_bytes);
        if report.timed_out {
            self.audit(base, audit_mode, "timeout", exit_code);
            warn!(timeout_secs = self.timeout_secs, "host.sudo_exec timed out");
            return ToolOutput::error(format!(
                "[host] host.sudo_exec: Zeitlimit von {}s überschritten; der Prozess wurde \
                 beendet (Root-Kindprozesse können weiterlaufen). Teilausgabe:\n[stdout]\n{stdout}\n\
                 [stderr]\n{stderr}",
                self.timeout_secs
            ));
        }
        self.audit(base, audit_mode, "approved", exit_code);
        let mode_label = match mode {
            SudoMode::Passwordless => "passwordless",
            SudoMode::Password => "password",
        };
        info!(
            exit_code = exit_code.unwrap_or(-1),
            truncated = report.limit_exceeded,
            "host.sudo_exec completed"
        );
        ToolOutput::json(json!({
            "exit_code": exit_code.unwrap_or(-1),
            "stdout": stdout,
            "stderr": stderr,
            "truncated": report.limit_exceeded,
            "executed_on": "host",
            "privileged": true,
            "mode": mode_label,
        }))
    }

    /// Der Ablauf nach dem Parsen.
    async fn run_call(
        &self,
        args: SudoExecArgs,
        sandbox: &SandboxSpec,
        session: &str,
        cancel: Option<&CancelToken>,
    ) -> Result<ToolOutput, ToolsError> {
        let base = AuditBase {
            session,
            argv: &args.argv,
            started: Instant::now(),
        };
        if let Err(message) = validate_request(&args.argv, &args.reason) {
            self.audit(&base, "none", "invalid", None);
            return Ok(ToolOutput::error(format!("host.sudo_exec: {message}")));
        }
        let Some(sudo) = self.sudo_binary() else {
            self.audit(&base, "none", "error", None);
            return Ok(ToolOutput::error(
                "host.sudo_exec: sudo ist weder unter /usr/bin/sudo noch unter /bin/sudo installiert",
            ));
        };
        if self.prompts.is_closed() {
            self.audit(&base, "none", "no_ui", None);
            return Ok(ToolOutput::error(NO_UI_MSG));
        }
        let passwordless = self.probe_passwordless(&sudo).await;
        let cwd = sandbox.workspace().canonical_root().to_path_buf();

        let (prompt, answer) = SudoPrompt::new(
            session.to_owned(),
            self.worker.clone(),
            args.argv.clone(),
            cwd.clone(),
            args.reason.clone(),
            passwordless,
        );
        if self.prompts.send(prompt).is_err() {
            self.audit(&base, "none", "no_ui", None);
            warn!(session, "host.sudo_exec: prompt channel closed");
            return Ok(ToolOutput::error(NO_UI_MSG));
        }
        let reply = tokio::select! {
            waited = tokio::time::timeout(self.prompt_timeout, answer.recv()) => match waited {
                Ok(reply) => reply,
                Err(_elapsed) => {
                    warn!(session, "host.sudo_exec: prompt timed out");
                    None
                }
            },
            () = cancelled(cancel) => {
                self.audit(&base, "none", "cancelled", None);
                return Err(ToolsError::Cancelled);
            }
        };
        let Some(SudoReply {
            answer,
            on_auth_failure,
        }) = reply
        else {
            self.audit(&base, "none", "denied", None);
            return Ok(ToolOutput::error(DENIED_MSG));
        };

        let (mode, audit_mode, secret) = match (passwordless, answer) {
            // Passwortlos: ein mitgeschicktes Geheimnis wird verworfen
            // (Drop → genullt) und nie geschrieben.
            (true, _) => (SudoMode::Passwordless, "passwordless", None),
            (false, SudoAnswer::ApproveOnce) => {
                self.audit(&base, "none", "denied", None);
                return Ok(ToolOutput::error(DENIED_MSG));
            }
            (false, SudoAnswer::ApproveSession { secret }) => {
                (SudoMode::Password, "password_session_reused", Some(secret))
            }
            (false, SudoAnswer::Password { secret, remember }) => (
                SudoMode::Password,
                if remember {
                    "password_session_new"
                } else {
                    "password_once"
                },
                Some(secret),
            ),
        };

        let marker = fresh_nonce();
        let command = match self.build_command(&sudo, mode, &marker, &args.argv, &cwd) {
            Ok(command) => command,
            Err(message) => {
                self.audit(&base, audit_mode, "error", None);
                return Ok(ToolOutput::error(message));
            }
        };
        match self.run(command, secret, &marker, cancel).await {
            Ok(report) => {
                Ok(self.finish(&base, mode, audit_mode, &marker, report, on_auth_failure))
            }
            Err(Some(message)) => {
                self.audit(&base, audit_mode, "error", None);
                Ok(ToolOutput::error(message))
            }
            Err(None) => {
                self.audit(&base, audit_mode, "cancelled", None);
                Err(ToolsError::Cancelled)
            }
        }
    }
}

impl ToolExecutor for SudoExecExecutor {
    /// Führt `host.sudo_exec` aus.
    ///
    /// # Beschreibung
    /// 1. Argumente parsen (`deny_unknown_fields`).
    /// 2. `ExecuteProcess` prüfen.
    /// 3. argv/Grund prüfen, `sudo` an festen Pfaden suchen.
    /// 4. Passwortlos-Probe, Frage an das Freigabefenster, höchstens
    ///    [`SUDO_PROMPT_TIMEOUT`] warten.
    /// 5. Nur nach ausdrücklicher Freigabe ausführen.
    ///
    /// # Errors
    /// [`ToolsError::InvalidArguments`] bei unpassenden Argumenten,
    /// [`ToolsError::Cancelled`] bei Abbruch; alles andere als [`ToolOutput`].
    fn execute<'a>(
        &'a self,
        context: &'a ToolExecutionContext,
        call: &'a ToolCall,
    ) -> ToolExecutorFuture<'a> {
        Box::pin(async move {
            let args: SudoExecArgs =
                serde_json::from_value(call.arguments.clone()).map_err(|err| {
                    ToolsError::InvalidArguments {
                        name: SUDO_EXEC_TOOL.to_owned(),
                        reason: err.to_string(),
                    }
                })?;
            if let Some(denied) = harw_tools::sandbox_guard::require_permission(
                context,
                Permission::ExecuteProcess,
                SUDO_EXEC_TOOL,
            ) {
                warn!("host.sudo_exec denied: ExecuteProcess permission missing");
                return Ok(denied);
            }
            self.run_call(
                args,
                context.sandbox(),
                context.session_id().as_str(),
                context.cancel(),
            )
            .await
        })
    }
}

// ── Provider ──────────────────────────────────────────────────────────────────

/// Registriert `host.sudo_exec`.
///
/// # Beschreibung
/// Ohne [`SudoPromptSender`] lässt sich der Provider nicht bauen — es gibt
/// kein Werkzeug ohne Freigabefenster. Vorgaben: Zeitlimit 300 s,
/// Ausgabebudget 64 KiB, rlimits wie `shell.exec`, aber mit 300 s CPU-Zeit,
/// 4 GiB Adressraum/Dateigröße und 1024 Deskriptoren, Wartezeit auf die
/// Entscheidung [`SUDO_PROMPT_TIMEOUT`], Audit über [`TracingSudoAudit`].
///
/// # Nebenläufigkeit
/// `Send + Sync`; [`ToolProvider::parallel_safe`] ist `false`.
pub struct SudoToolProvider {
    prompts: SudoPromptSender,
    worker: String,
    timeout_secs: u64,
    max_output_bytes: usize,
    limits: ShellLimits,
    prompt_timeout: Duration,
    password_window: Duration,
    audit: Arc<dyn SudoAuditSink>,
    #[cfg(test)]
    sudo_path: Option<PathBuf>,
}

impl fmt::Debug for SudoToolProvider {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SudoToolProvider")
            .field("worker", &self.worker)
            .field("timeout_secs", &self.timeout_secs)
            .field("prompt_timeout", &self.prompt_timeout)
            .finish_non_exhaustive()
    }
}

impl SudoToolProvider {
    /// Baut den Provider.
    ///
    /// # Argumente
    /// - `prompts` ([`SudoPromptSender`]): Sendeseite zum Freigabefenster.
    /// - `worker` (`impl Into<String>`): Rolle des Workers (Anzeige/Audit).
    #[must_use]
    pub fn new(prompts: SudoPromptSender, worker: impl Into<String>) -> Self {
        Self {
            prompts,
            worker: worker.into(),
            timeout_secs: DEFAULT_TIMEOUT_SECS,
            max_output_bytes: DEFAULT_MAX_OUTPUT_BYTES,
            limits: default_sudo_limits(),
            prompt_timeout: SUDO_PROMPT_TIMEOUT,
            password_window: DEFAULT_PASSWORD_WINDOW,
            audit: Arc::new(TracingSudoAudit),
            #[cfg(test)]
            sudo_path: None,
        }
    }

    /// Setzt das Zeitlimit eines Root-Befehls (mindestens 1 s).
    #[must_use]
    pub fn with_timeout_secs(mut self, timeout_secs: u64) -> Self {
        self.timeout_secs = timeout_secs.max(1);
        self
    }

    /// Setzt die rlimits des Starts.
    #[must_use]
    pub fn with_limits(mut self, limits: ShellLimits) -> Self {
        self.limits = limits;
        self
    }

    /// Setzt die Wartezeit auf die Entscheidung (höchstens
    /// [`SUDO_PROMPT_TIMEOUT`]).
    #[must_use]
    pub fn with_prompt_timeout(mut self, timeout: Duration) -> Self {
        self.prompt_timeout = timeout.min(SUDO_PROMPT_TIMEOUT);
        self
    }

    /// Setzt das Audit-Ziel.
    #[must_use]
    pub fn with_audit_sink(mut self, audit: Arc<dyn SudoAuditSink>) -> Self {
        self.audit = audit;
        self
    }

    /// Nur Tests: ein Fake-`sudo` statt der festen Pfade. Im Produktivpfad
    /// gibt es keinen Override (weder per Umgebung noch per Konfiguration).
    #[cfg(test)]
    #[must_use]
    pub(crate) fn with_sudo_path(mut self, path: PathBuf) -> Self {
        self.sudo_path = Some(path);
        self
    }

    /// Nur Tests: kürzere Frist für die Passwortabfrage.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn with_password_window(mut self, window: Duration) -> Self {
        self.password_window = window;
        self
    }

    /// Das Parameterschema: `argv` (Array aus Strings) und `reason`.
    fn parameter_schema() -> JsonSchema {
        let mut properties = BTreeMap::new();
        properties.insert(
            "argv".to_owned(),
            JsonSchema {
                schema_type: Some(JsonSchemaType::Array),
                description: Some(
                    "Exact argv of the command to run as root, e.g. [\"apt-get\", \"install\", \
                     \"-y\", \"ripgrep\"]. No shell: no pipes, redirections or globbing. Do not \
                     include sudo."
                        .to_owned(),
                ),
                items: Some(Box::new(JsonSchema {
                    schema_type: Some(JsonSchemaType::String),
                    ..Default::default()
                })),
                ..Default::default()
            },
        );
        properties.insert(
            "reason".to_owned(),
            JsonSchema {
                schema_type: Some(JsonSchemaType::String),
                description: Some(
                    "Short reason shown to the user in the approval window.".to_owned(),
                ),
                ..Default::default()
            },
        );
        JsonSchema {
            schema_type: Some(JsonSchemaType::Object),
            properties: Some(properties),
            required: Some(vec!["argv".to_owned(), "reason".to_owned()]),
            additional_properties: Some(Box::new(AdditionalProperties::Bool(false))),
            ..Default::default()
        }
    }
}

// `host.sudo_exec` ist das einzige Werkzeug; der Executor trägt Kanal, Worker,
// Limits, Zeitfenster und Audit-Senke des Providers. `parallel_safe: none` —
// ein Aufruf als root ist nie kommutativ.
harw_tools::tool_provider! {
    impl for SudoToolProvider as provider, parallel_safe: none {
        SUDO_EXEC_TOOL => {
            spec: ToolSpec::Function(FunctionToolSpec {
                name: ToolName::new(SUDO_EXEC_TOOL),
                description: "Run ONE command as root on the local host via sudo. sudo works: the \
                user sees the exact argv and your reason in a TUI approval window, confirms and \
                types their sudo password there if sudo asks (passwordless sudo works too). The \
                password never reaches you — never ask for it in chat, put it into a command or \
                use `sudo -S`/`echo … | sudo`. Returns exit_code/stdout/stderr, or an error if \
                the user denied it or authentication failed — then do not retry on your own, ask \
                the user. Never call sudo via shell.exec; never say sudo is impossible while this \
                tool is available."
                .to_owned(),
                parameters: SudoToolProvider::parameter_schema(),
                strict: true,
            }),
            executor: SudoExecExecutor {
                prompts: provider.prompts.clone(),
                worker: provider.worker.clone(),
                timeout_secs: provider.timeout_secs,
                max_output_bytes: provider.max_output_bytes,
                limits: provider.limits,
                prompt_timeout: provider.prompt_timeout,
                password_window: provider.password_window,
                audit: Arc::clone(&provider.audit),
                #[cfg(test)]
                sudo_path: provider.sudo_path.clone(),
                #[cfg(not(test))]
                sudo_path: None,
            },
        },
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult, ctx};
    use harw_authority::{PermissionSet, WorkspaceRegistration, WorkspaceRegistry};
    use harw_extension_api::contributors::ToolProvider;
    use harw_types::{SessionId, TenantId, ToolCallId, TurnId, WorkspaceId};
    use serde_json::Value;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicBool, Ordering};
    use tempfile::TempDir;

    const PASSWORD: &str = "hunter2-geheim";

    fn sandbox(dir: &TempDir, permissions: &[Permission]) -> TestResult<SandboxSpec> {
        let workspace = dir.path().join("project");
        fs::create_dir_all(&workspace).map_err(ctx("workspace"))?;
        let registry = WorkspaceRegistry::build(
            dir.path(),
            [WorkspaceRegistration {
                tenant: TenantId::from_str("tenant"),
                workspace: WorkspaceId::from_str("project"),
                root: workspace,
            }],
        )
        .map_err(ctx("registry"))?;
        let binding = registry
            .resolve(
                &TenantId::from_str("tenant"),
                &WorkspaceId::from_str("project"),
            )
            .map_err(ctx("binding"))?;
        Ok(SandboxSpec::from_resolved(
            binding,
            PermissionSet::from_policy(permissions.iter().copied()),
        ))
    }

    fn call(arguments: Value) -> ToolCall {
        ToolCall {
            id: ToolCallId::new(),
            name: ToolName::new(SUDO_EXEC_TOOL),
            arguments,
        }
    }

    fn secret(text: &str) -> TestResult<SudoSecret> {
        SudoSecret::from_zeroizing(Zeroizing::new(text.as_bytes().to_vec())).map_err(ctx("secret"))
    }

    /// Sammelt Audit-Datensätze.
    #[derive(Default)]
    struct CollectingAudit(Mutex<Vec<SudoAuditRecord>>);

    impl SudoAuditSink for CollectingAudit {
        fn record(&self, record: &SudoAuditRecord) {
            if let Ok(mut records) = self.0.lock() {
                records.push(record.clone());
            }
        }
    }

    /// Fake-`sudo` als Shell-Skript. `passwordless` schaltet `-n` frei;
    /// `prompts` steuert, ob `-S` nach dem Passwort fragt.
    fn fake_sudo(dir: &Path, passwordless: bool, prompts: bool) -> TestResult<PathBuf> {
        let path = dir.join(if passwordless {
            "sudo-nopasswd"
        } else if prompts {
            "sudo-passwd"
        } else {
            "sudo-silent"
        });
        let n_branch = if passwordless {
            "exec \"$@\""
        } else {
            "echo 'sudo: a password is required' >&2; exit 1"
        };
        let s_branch = if prompts {
            format!(
                "printf '%s' \"$prompt\" >&2\n\
                 IFS= read -r pw || pw=''\n\
                 if [ \"$pw\" = '{pw}' ]; then exec \"$@\"; fi\n\
                 echo 'Sorry, try again.' >&2\n\
                 printf '%s' \"$prompt\" >&2\n\
                 IFS= read -r pw2 || true\n\
                 echo 'sudo: no password was provided' >&2\n\
                 echo 'sudo: 1 incorrect password attempt' >&2\n\
                 exit 1",
                pw = PASSWORD
            )
        } else {
            "exec \"$@\"".to_owned()
        };
        let script = format!(
            "#!/bin/sh\n\
             mode=''\nprompt=''\n\
             while [ $# -gt 0 ]; do\n\
               case \"$1\" in\n\
                 -n) mode=n; shift;;\n\
                 -S) mode=S; shift;;\n\
                 -k) shift;;\n\
                 -p) prompt=\"$2\"; shift 2;;\n\
                 --) shift; break;;\n\
                 *) break;;\n\
               esac\n\
             done\n\
             if [ \"$mode\" = n ]; then {n_branch}; fi\n\
             {s_branch}\n"
        );
        fs::write(&path, script).map_err(ctx("fake sudo"))?;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755))
            .map_err(ctx("fake sudo mode"))?;
        Ok(path)
    }

    fn test_limits() -> ShellLimits {
        ShellLimits {
            require_rlimits: false,
            ..ShellLimits::default()
        }
    }

    /// Baut Provider + Kanal mit Fake-`sudo`.
    fn provider_with(
        sudo: PathBuf,
        audit: Arc<CollectingAudit>,
    ) -> (SudoToolProvider, SudoPromptReceiver) {
        let (sender, receiver) = sudo_prompt_channel();
        let provider = SudoToolProvider::new(sender, "uia-shell-worker")
            .with_sudo_path(sudo)
            .with_limits(test_limits())
            .with_timeout_secs(20)
            .with_password_window(Duration::from_millis(300))
            .with_audit_sink(audit);
        (provider, receiver)
    }

    async fn execute(
        provider: &SudoToolProvider,
        spec: SandboxSpec,
        arguments: Value,
    ) -> TestResult<ToolOutput> {
        let context = ToolExecutionContext::new(SessionId::new(), TurnId::new(), spec);
        let executor = provider
            .executor(&ToolName::new(SUDO_EXEC_TOOL))
            .ok_or(TestError::Missing("executor"))?;
        executor
            .execute(&context, &call(arguments))
            .await
            .map_err(ctx("execute"))
    }

    fn rendered(output: &ToolOutput) -> String {
        match output {
            ToolOutput::Json { content } => content.to_string(),
            ToolOutput::Error { message } => message.clone(),
            ToolOutput::Text { content } => content.clone(),
        }
    }

    fn full_rights() -> [Permission; 2] {
        [Permission::ReadWorkspace, Permission::ExecuteProcess]
    }

    // ── Geheimnis ─────────────────────────────────────────────────────────

    #[test]
    fn test_secret_debug_and_answer_debug_never_show_the_password() -> TestResult {
        let secret = secret(PASSWORD)?;
        assert_eq!(format!("{secret:?}"), "SudoSecret(<redacted>)");
        let answer = SudoAnswer::Password {
            secret,
            remember: true,
        };
        let debugged = format!("{answer:?}");
        assert!(!debugged.contains(PASSWORD), "{debugged}");
        Ok(())
    }

    #[test]
    fn test_secret_rejects_empty_newline_and_oversized_input() {
        assert_eq!(
            SudoSecret::from_zeroizing(Zeroizing::new(Vec::new())).err(),
            Some(SudoSecretError::Empty)
        );
        assert_eq!(
            SudoSecret::from_zeroizing(Zeroizing::new(b"a\nb".to_vec())).err(),
            Some(SudoSecretError::ForbiddenByte)
        );
        assert_eq!(
            SudoSecret::from_zeroizing(Zeroizing::new(vec![b'x'; SUDO_MAX_SECRET_BYTES + 1])).err(),
            Some(SudoSecretError::TooLong)
        );
    }

    /// Nullung beim Drop: `SudoSecret` ist `ZeroizeOnDrop`, und ein
    /// ausdrückliches `zeroize` leert den Inhalt.
    #[test]
    fn test_secret_is_zeroize_on_drop_and_zeroize_clears_it() -> TestResult {
        fn assert_zeroize_on_drop<T: ZeroizeOnDrop>() {}
        assert_zeroize_on_drop::<SudoSecret>();
        let mut secret = secret(PASSWORD)?;
        let line = secret.line();
        assert_eq!(line.as_slice(), format!("{PASSWORD}\n").as_bytes());
        assert_eq!(line.capacity(), PASSWORD.len() + 1, "keine Umkopie");
        secret.zeroize();
        assert!(secret.0.is_empty());
        let clone = secret.try_clone();
        assert!(clone.0.is_empty());
        Ok(())
    }

    // ── Prüfungen ─────────────────────────────────────────────────────────

    #[test]
    fn test_escalation_program_finds_sudo_in_command_position() {
        for command in [
            "sudo ls",
            "  /usr/bin/sudo -u root id",
            "FOO=1 sudo id",
            "env -i sudo id",
            "cd / && sudo rm -rf x",
            "true; doas id",
            "echo $(pkexec id)",
            "nohup nice -n 5 sudo id",
            "timeout 10 su -c id",
            "s\"u\"do id",
            "if true; then run0 id; fi",
            "sudo apt install x",
            "cd /; sudo -i",
            "echo x | sudo tee f",
            "env FOO=1 sudo ls",
            "true || sudo id",
            "echo \"$(sudo id)\"",
            "echo `doas id`",
            "(sudo id)",
            "xargs -0 sudo rm < list",
            "time sudo id",
            "exec sudo id",
            "FOO='a b' sudo id",
            "bash -c 'sudo id'",
            "sh -lc \"apt update && sudo apt upgrade\"",
            "ls\nsudo id",
        ] {
            assert!(escalation_program(command).is_some(), "{command}");
        }
        for command in [
            "ls -la",
            "grep sudo /etc/group",
            "apt show sudo",
            "echo sudo",
            "cat /etc/sudoers.d/x",
            "git commit -m 'use sudo later'",
            // Realer Fehlalarm: `sudo` nur im gequoteten Suchmuster.
            "rg -n 'sudo_exec|passwordless|passwd|Command::new|sudo -|sudo-Freigabe|sudo wird|\
             sudo.*abgelehnt' ~/Harwness/harw-tui/src/sudo_dialog.rs",
            "grep -E \"x|sudo -S\" f",
            "echo 'a; sudo id'",
            "echo \"a && sudo id\"",
            "rg sudo_exec src/",
            "echo x # ; sudo id",
            "bash -c 'echo sudo'",
            "bash script.sh sudo",
        ] {
            assert_eq!(escalation_program(command), None, "{command}");
        }
    }

    #[test]
    fn test_command_segments_respect_quotes_and_substitutions() {
        assert_eq!(
            command_segments("rg -n 'a|sudo -' f && echo \"x;y\" | wc"),
            vec![
                vec![
                    "rg".to_owned(),
                    "-n".to_owned(),
                    "a|sudo -".to_owned(),
                    "f".to_owned()
                ],
                vec!["echo".to_owned(), "x;y".to_owned()],
                vec!["wc".to_owned()],
            ]
        );
        assert_eq!(
            command_segments("echo \"$(id -u)\" s\"u\"do"),
            vec![
                vec!["id".to_owned(), "-u".to_owned()],
                vec!["echo".to_owned(), String::new(), "sudo".to_owned()],
            ]
        );
    }

    #[test]
    fn test_shell_escalation_message_is_actionable() {
        let text = shell_escalation_message("sudo");
        for phrase in [
            "sudo selbst funktioniert",
            SUDO_EXEC_TOOL,
            "gibt dort sein Passwort ein",
            "passwortloses sudo geht ebenso",
            "`transfer_to_uia-shell-worker`",
            "an deinen Elternteil bzw. die UIA",
            "exaktem argv",
            "nie `sudo -S`",
            "Sag nie, sudo sei unmöglich",
            "Nur ohne TUI",
        ] {
            assert!(text.contains(phrase), "fehlt „{phrase}“: {text}");
        }
    }

    #[test]
    fn test_validate_request_rejects_unsafe_argv_and_reason() {
        let ok = vec!["apt-get".to_owned(), "update".to_owned()];
        assert!(validate_request(&ok, "Paketlisten").is_ok());
        assert!(validate_request(&[], "x").is_err());
        assert!(validate_request(&["sudo".to_owned(), "id".to_owned()], "x").is_err());
        assert!(validate_request(&["/usr/bin/doas".to_owned()], "x").is_err());
        assert!(validate_request(&["-u".to_owned()], "x").is_err());
        assert!(validate_request(&["echo".to_owned(), "a\u{1b}[2J".to_owned()], "x").is_err());
        assert!(validate_request(&["echo".to_owned(), "a\u{202E}b".to_owned()], "x").is_err());
        assert!(validate_request(&["echo".to_owned(), "a\nb".to_owned()], "x").is_err());
        assert!(validate_request(&ok, "").is_err());
        assert!(validate_request(&ok, "zeile\nzwei").is_err());
        let too_many = vec!["x".to_owned(); MAX_ARGV_LEN + 1];
        assert!(validate_request(&too_many, "x").is_err());
    }

    #[test]
    fn test_marker_scan_finds_split_markers_and_counts_repeats() {
        let marker = b"harw-sudo-abc:";
        let mut tail = Vec::new();
        assert_eq!(scan_for_marker(&mut tail, b"xx harw-su", marker), 0);
        assert_eq!(scan_for_marker(&mut tail, b"do-abc: yy", marker), 1);
        assert_eq!(
            scan_for_marker(&mut tail, b"Sorry\nharw-sudo-abc:", marker),
            1
        );
        assert!(tail.len() < marker.len());
        assert_eq!(strip_marker(b"aharw-sudo-abc:b", marker), b"ab".to_vec());
    }

    #[test]
    fn test_clean_stderr_drops_marker_and_prompt_lines() {
        let cleaned = clean_stderr(
            b"harw-sudo-x:Sorry, try again.\n[sudo] password for alice: \nreal error\n",
            b"harw-sudo-x:",
        );
        assert_eq!(cleaned, "real error");
    }

    #[test]
    fn test_audit_record_redacts_to_fields_without_any_secret_field() -> TestResult {
        let record = SudoAuditRecord {
            operator: "alice".to_owned(),
            session: "s1".to_owned(),
            worker: "uia-shell-worker".to_owned(),
            argv: display_argv(&["apt-get".to_owned(), "install".to_owned(), "a b".to_owned()]),
            argv_hash: argv_hash(&["apt-get".to_owned()]),
            mode: "password_once",
            decision: "approved",
            exit_code: "0".to_owned(),
            duration_ms: 12,
        };
        let harw_observe::Redacted::Shown(text) = harw_observe::Redact::redact(&record) else {
            return Err(TestError::Unexpected(
                "Redact muss Shown liefern".to_owned(),
            ));
        };
        assert!(text.contains("apt-get install 'a b'"), "{text}");
        assert!(text.contains("password_once"), "{text}");
        assert_eq!(argv_hash(&["a".to_owned(), "b".to_owned()]).len(), 64);
        assert_ne!(
            argv_hash(&["ab".to_owned()]),
            argv_hash(&["a".to_owned(), "b".to_owned()]),
            "Längenpräfix trennt Elemente"
        );
        Ok(())
    }

    #[test]
    fn test_provider_lists_one_strict_tool_and_is_not_parallel_safe() {
        let (sender, _receiver) = sudo_prompt_channel();
        let provider = SudoToolProvider::new(sender, "uia-shell-worker");
        let specs = provider.tools();
        assert_eq!(specs.len(), 1);
        assert_eq!(specs[0].name(), SUDO_EXEC_TOOL);
        assert!(!provider.parallel_safe(&ToolName::new(SUDO_EXEC_TOOL)));
        assert!(provider.executor(&ToolName::new("shell.exec")).is_none());
    }

    // ── Freigabe-Kette (fail-closed) ─────────────────────────────────────

    #[tokio::test]
    async fn test_unknown_fields_are_rejected() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let audit = Arc::new(CollectingAudit::default());
        let (provider, _receiver) = provider_with(fake_sudo(dir.path(), true, false)?, audit);
        let context = ToolExecutionContext::new(
            SessionId::new(),
            TurnId::new(),
            sandbox(&dir, &full_rights())?,
        );
        let executor = provider
            .executor(&ToolName::new(SUDO_EXEC_TOOL))
            .ok_or(TestError::Missing("executor"))?;
        let result = executor
            .execute(
                &context,
                &call(json!({"argv": ["id"], "reason": "x", "password": "p"})),
            )
            .await;
        assert!(matches!(result, Err(ToolsError::InvalidArguments { .. })));
        Ok(())
    }

    #[tokio::test]
    async fn test_without_execute_process_nothing_is_asked() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let audit = Arc::new(CollectingAudit::default());
        let (provider, mut receiver) = provider_with(fake_sudo(dir.path(), true, false)?, audit);
        let output = execute(
            &provider,
            sandbox(&dir, &[Permission::ReadWorkspace])?,
            json!({"argv": ["id"], "reason": "x"}),
        )
        .await?;
        assert!(matches!(output, ToolOutput::Error { .. }));
        assert!(receiver.try_recv().is_err(), "keine Frage ohne Recht");
        Ok(())
    }

    #[tokio::test]
    async fn test_closed_channel_fails_closed_without_running() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let marker = dir.path().join("ran");
        let audit = Arc::new(CollectingAudit::default());
        let (provider, receiver) =
            provider_with(fake_sudo(dir.path(), true, false)?, Arc::clone(&audit));
        drop(receiver);
        let output = execute(
            &provider,
            sandbox(&dir, &full_rights())?,
            json!({"argv": ["touch", marker.display().to_string()], "reason": "x"}),
        )
        .await?;
        assert_eq!(rendered(&output), NO_UI_MSG);
        assert!(!marker.exists(), "ohne Fenster darf nichts laufen");
        let records = audit.0.lock().map_err(|_| TestError::Missing("lock"))?;
        assert_eq!(records.last().map(|record| record.decision), Some("no_ui"));
        Ok(())
    }

    #[tokio::test]
    async fn test_dropped_or_denied_prompt_is_a_denial() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let marker = dir.path().join("ran");
        let audit = Arc::new(CollectingAudit::default());
        let (provider, mut receiver) =
            provider_with(fake_sudo(dir.path(), true, false)?, Arc::clone(&audit));
        let answering = tokio::spawn(async move {
            if let Some(prompt) = receiver.recv().await {
                prompt.deny();
            }
        });
        let output = execute(
            &provider,
            sandbox(&dir, &full_rights())?,
            json!({"argv": ["touch", marker.display().to_string()], "reason": "x"}),
        )
        .await?;
        answering.await.map_err(ctx("answer task"))?;
        assert_eq!(rendered(&output), DENIED_MSG);
        assert!(!marker.exists());
        Ok(())
    }

    #[tokio::test]
    async fn test_prompt_timeout_is_a_denial() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let audit = Arc::new(CollectingAudit::default());
        let (provider, _receiver) = provider_with(fake_sudo(dir.path(), true, false)?, audit);
        let provider = provider.with_prompt_timeout(Duration::from_millis(50));
        let output = execute(
            &provider,
            sandbox(&dir, &full_rights())?,
            json!({"argv": ["id"], "reason": "x"}),
        )
        .await?;
        assert_eq!(rendered(&output), DENIED_MSG);
        Ok(())
    }

    // ── Fake-sudo: passwortlos und mit Passwort ──────────────────────────

    #[tokio::test]
    async fn test_passwordless_path_shows_plain_approval_and_runs_argv() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let audit = Arc::new(CollectingAudit::default());
        let (provider, mut receiver) =
            provider_with(fake_sudo(dir.path(), true, false)?, Arc::clone(&audit));
        let answering = tokio::spawn(async move {
            let prompt = receiver.recv().await?;
            let passwordless = prompt.passwordless();
            let argv = prompt.argv().to_vec();
            prompt.approve(SudoAnswer::ApproveOnce);
            Some((passwordless, argv))
        });
        let output = execute(
            &provider,
            sandbox(&dir, &full_rights())?,
            json!({"argv": ["echo", "hallo root"], "reason": "Test"}),
        )
        .await?;
        let (passwordless, argv) = answering
            .await
            .map_err(ctx("answer task"))?
            .ok_or(TestError::Missing("prompt"))?;
        assert!(passwordless);
        assert_eq!(argv, vec!["echo".to_owned(), "hallo root".to_owned()]);
        let ToolOutput::Json { content } = &output else {
            return Err(TestError::Unexpected(rendered(&output)));
        };
        assert_eq!(content["exit_code"], json!(0));
        assert_eq!(content["mode"], json!("passwordless"));
        assert!(
            content["stdout"]
                .as_str()
                .is_some_and(|out| out.contains("hallo root"))
        );
        let records = audit.0.lock().map_err(|_| TestError::Missing("lock"))?;
        assert_eq!(
            records.last().map(|record| record.mode),
            Some("passwordless")
        );
        Ok(())
    }

    #[tokio::test]
    async fn test_password_path_runs_and_the_password_never_reaches_the_output() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let audit = Arc::new(CollectingAudit::default());
        let (provider, mut receiver) =
            provider_with(fake_sudo(dir.path(), false, true)?, Arc::clone(&audit));
        let password = secret(PASSWORD)?;
        let answering = tokio::spawn(async move {
            let prompt = receiver.recv().await?;
            let passwordless = prompt.passwordless();
            prompt.approve(SudoAnswer::Password {
                secret: password,
                remember: false,
            });
            Some(passwordless)
        });
        // `cat` liest stdin: nach der Passwortzeile ist die Pipe zu, es darf
        // nichts vom Passwort ankommen.
        let output = execute(
            &provider,
            sandbox(&dir, &full_rights())?,
            json!({"argv": ["sh", "-c", "cat; echo fertig"], "reason": "Test"}),
        )
        .await?;
        let passwordless = answering
            .await
            .map_err(ctx("answer task"))?
            .ok_or(TestError::Missing("prompt"))?;
        assert!(!passwordless);
        let text = rendered(&output);
        assert!(text.contains("fertig"), "{text}");
        assert!(!text.contains(PASSWORD), "Passwort im Ergebnis: {text}");
        assert!(
            !text.contains("harw-sudo-"),
            "Einmal-Marke im Ergebnis: {text}"
        );
        let records = audit.0.lock().map_err(|_| TestError::Missing("lock"))?;
        let record = records.last().ok_or(TestError::Missing("audit"))?;
        assert_eq!(record.mode, "password_once");
        assert_eq!(record.decision, "approved");
        assert!(!format!("{record:?}").contains(PASSWORD));
        Ok(())
    }

    #[tokio::test]
    async fn test_wrong_password_reports_only_auth_failure_and_fires_the_hook() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let audit = Arc::new(CollectingAudit::default());
        let (provider, mut receiver) =
            provider_with(fake_sudo(dir.path(), false, true)?, Arc::clone(&audit));
        let fired = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&fired);
        let wrong = secret("falsch-123")?;
        let answering = tokio::spawn(async move {
            if let Some(prompt) = receiver.recv().await {
                prompt.approve_with_failure_hook(
                    SudoAnswer::ApproveSession { secret: wrong },
                    SudoAuthFailureHook::new(move || flag.store(true, Ordering::SeqCst)),
                );
            }
        });
        let output = execute(
            &provider,
            sandbox(&dir, &full_rights())?,
            json!({"argv": ["echo", "nie"], "reason": "Test"}),
        )
        .await?;
        answering.await.map_err(ctx("answer task"))?;
        assert_eq!(rendered(&output), AUTH_FAILED_MSG);
        assert!(fired.load(Ordering::SeqCst), "Rückruf muss feuern");
        let records = audit.0.lock().map_err(|_| TestError::Missing("lock"))?;
        assert_eq!(
            records.last().map(|record| record.decision),
            Some("auth_failed")
        );
        Ok(())
    }

    /// Fragt sudo nicht (z. B. `NOPASSWD` nur für diesen Befehl), wird das
    /// Passwort nie geschrieben — `cat` sieht nach der Frist nur EOF.
    #[tokio::test]
    async fn test_password_is_never_written_when_sudo_does_not_prompt() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let audit = Arc::new(CollectingAudit::default());
        let (provider, mut receiver) = provider_with(fake_sudo(dir.path(), false, false)?, audit);
        let password = secret(PASSWORD)?;
        let answering = tokio::spawn(async move {
            if let Some(prompt) = receiver.recv().await {
                prompt.approve(SudoAnswer::Password {
                    secret: password,
                    remember: false,
                });
            }
        });
        let output = execute(
            &provider,
            sandbox(&dir, &full_rights())?,
            json!({"argv": ["cat"], "reason": "Test"}),
        )
        .await?;
        answering.await.map_err(ctx("answer task"))?;
        let text = rendered(&output);
        assert!(!text.contains(PASSWORD), "Passwort ausgeleitet: {text}");
        Ok(())
    }

    #[tokio::test]
    async fn test_approve_once_without_secret_is_denied_on_the_password_path() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let marker = dir.path().join("ran");
        let audit = Arc::new(CollectingAudit::default());
        let (provider, mut receiver) = provider_with(fake_sudo(dir.path(), false, true)?, audit);
        let answering = tokio::spawn(async move {
            if let Some(prompt) = receiver.recv().await {
                prompt.approve(SudoAnswer::ApproveOnce);
            }
        });
        let output = execute(
            &provider,
            sandbox(&dir, &full_rights())?,
            json!({"argv": ["touch", marker.display().to_string()], "reason": "x"}),
        )
        .await?;
        answering.await.map_err(ctx("answer task"))?;
        assert_eq!(rendered(&output), DENIED_MSG);
        assert!(!marker.exists());
        Ok(())
    }

    // ── Tracing enthält das Passwort nie ─────────────────────────────────

    /// Minimaler Subscriber, der jedes Feld jedes Ereignisses und Spans als
    /// Text sammelt (ohne neue Abhängigkeit).
    struct CaptureSubscriber(Arc<Mutex<String>>);

    struct CaptureVisitor(Arc<Mutex<String>>);

    impl tracing::field::Visit for CaptureVisitor {
        fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn fmt::Debug) {
            if let Ok(mut text) = self.0.lock() {
                text.push_str(&format!("{}={value:?};", field.name()));
            }
        }
    }

    impl tracing::Subscriber for CaptureSubscriber {
        fn enabled(&self, _metadata: &tracing::Metadata<'_>) -> bool {
            true
        }

        fn new_span(&self, span: &tracing::span::Attributes<'_>) -> tracing::span::Id {
            span.record(&mut CaptureVisitor(Arc::clone(&self.0)));
            tracing::span::Id::from_u64(1)
        }

        fn record(&self, _span: &tracing::span::Id, values: &tracing::span::Record<'_>) {
            values.record(&mut CaptureVisitor(Arc::clone(&self.0)));
        }

        fn record_follows_from(&self, _span: &tracing::span::Id, _follows: &tracing::span::Id) {}

        fn event(&self, event: &tracing::Event<'_>) {
            if let Ok(mut text) = self.0.lock() {
                text.push_str(event.metadata().target());
                text.push(' ');
            }
            event.record(&mut CaptureVisitor(Arc::clone(&self.0)));
        }

        fn enter(&self, _span: &tracing::span::Id) {}

        fn exit(&self, _span: &tracing::span::Id) {}
    }

    #[tokio::test]
    async fn test_tracing_and_audit_never_contain_the_password() -> TestResult {
        let captured = Arc::new(Mutex::new(String::new()));
        let _guard = tracing::subscriber::set_default(CaptureSubscriber(Arc::clone(&captured)));
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let (sender, mut receiver) = sudo_prompt_channel();
        // Vorgabe-Audit (tracing), damit auch der Audit-Datensatz erfasst wird.
        let provider = SudoToolProvider::new(sender, "uia-shell-worker")
            .with_sudo_path(fake_sudo(dir.path(), false, true)?)
            .with_limits(test_limits())
            .with_timeout_secs(20)
            .with_password_window(Duration::from_millis(300));
        let answering = tokio::spawn(async move {
            for password in [PASSWORD, "falsch-zweites-geheimnis"] {
                let Some(prompt) = receiver.recv().await else {
                    return;
                };
                let Ok(secret) =
                    SudoSecret::from_zeroizing(Zeroizing::new(password.as_bytes().to_vec()))
                else {
                    return;
                };
                prompt.approve(SudoAnswer::Password {
                    secret,
                    remember: true,
                });
            }
        });
        for _ in 0..2 {
            execute(
                &provider,
                sandbox(&dir, &full_rights())?,
                json!({"argv": ["echo", "ok"], "reason": "Test"}),
            )
            .await?;
        }
        answering.await.map_err(ctx("answer task"))?;
        let text = captured
            .lock()
            .map_err(|_| TestError::Missing("capture lock"))?
            .clone();
        assert!(
            text.contains("harw::audit"),
            "Audit wird aufgezeichnet: {text}"
        );
        assert!(!text.contains(PASSWORD), "Passwort im Tracing: {text}");
        assert!(!text.contains("falsch-zweites-geheimnis"), "{text}");
        Ok(())
    }

    // ── shell.exec lehnt sudo ab ─────────────────────────────────────────

    #[tokio::test]
    async fn test_shell_exec_rejects_sudo_with_a_hint_to_host_sudo_exec() -> TestResult {
        use crate::exec::ShellToolProvider;
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let provider = ShellToolProvider::new();
        let executor = provider
            .executor(&ToolName::new("shell.exec"))
            .ok_or(TestError::Missing("shell executor"))?;
        let context = ToolExecutionContext::new(
            SessionId::new(),
            TurnId::new(),
            sandbox(&dir, &full_rights())?,
        );
        for command in ["sudo apt-get update", "cd / && doas id", "pkexec id"] {
            let output = executor
                .execute(
                    &context,
                    &ToolCall {
                        id: ToolCallId::new(),
                        name: ToolName::new("shell.exec"),
                        arguments: json!({ "command": command }),
                    },
                )
                .await
                .map_err(ctx("shell.exec"))?;
            let text = rendered(&output);
            assert!(matches!(output, ToolOutput::Error { .. }), "{command}");
            assert!(text.contains(SUDO_EXEC_TOOL), "{command}: {text}");
            assert!(
                text.contains("sudo selbst funktioniert"),
                "{command}: {text}"
            );
            assert!(
                text.contains("transfer_to_uia-shell-worker"),
                "{command}: {text}"
            );
        }
        Ok(())
    }
}
