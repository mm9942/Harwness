//! Lokale Bestätigungs-UI für Host-Profil-Permit-Anfragen.
//!
//! # Verantwortungsbereich
//! Dieses Modul ist die Gegenseite zu
//! [`harw_sandbox::ProcessPermitLedger::issue_after_local_approval`] für genau
//! einen Fall: eine Anfrage von [`harw_tool_shell::ShellExecutor`], den
//! `Host`-Profil-Worker (`host-process-worker.toml`) für die laufende
//! Sitzung freizugeben. Es kennt weder den Freigabe-Kanal für normale
//! Werkzeugaufrufe (`crate::approval`) noch dessen Ratatui-Panel
//! (`crate::approval_dialog`) — beide bleiben unverändert. Ein Host-Permit
//! ist keine Freigabe für einen einzelnen Aufruf, sondern eine
//! sitzungsweite Grundsatzentscheidung ("darf dieser Worker in dieser
//! Sitzung überhaupt den Host erreichen?"); deshalb ein eigener,
//! **einmal je Sitzung** gestellter Dialog statt einer Wiederverwendung des
//! Werkzeug-Freigabe-Panels.
//!
//! # Schlüsseltypen
//! - [`HostPermitPrompt`] — die Frage, die beim Renderer ankommt.
//! - [`HostPermitPromptReceiver`] — die Empfängerseite, die der Renderer pollt.
//! - [`HostPermitDialog`] — stellt Fragen, sammelt Antworten ein und trägt
//!   eine Zustimmung in [`harw_sandbox::HostPermitSessionRegistry`] ein.
//!
//! # Sicherheitsregel: Ablehnung ist der Default
//! Wie in `crate::approval`: ein geschlossener Fragekanal, eine
//! fallengelassene Antwort oder ein Zeitablauf sind **keine** Zustimmung.
//! [`HostPermitDialog::ensure_session_approved`] trägt nur bei einer
//! ausdrücklichen [`HostPermitPrompt::approve`] etwas in die Registry ein.
//!
//! # Ablauf
//! 1. Bevor ein Worker mit `SandboxProfile::Host` seinen ersten Befehl
//!    ausführt, ruft die Runtime (künftige Fan-in-Stelle für Sub-Worker,
//!    siehe Abschlussbericht) [`HostPermitDialog::ensure_session_approved`]
//!    auf.
//! 2. Hat die Sitzung bereits zugestimmt
//!    ([`harw_sandbox::HostPermitSessionRegistry::is_session_approved`]),
//!    kehrt die Methode sofort mit `true` zurück — **kein** erneuter Dialog.
//! 3. Sonst geht eine [`HostPermitPrompt`] über einen `mpsc`-Kanal an den
//!    Renderer; die Methode wartet `await`-basiert auf genau eine Antwort.
//! 4. Bei Zustimmung trägt die Methode die Sitzung über
//!    [`harw_sandbox::HostPermitSessionRegistry::mark_session_approved`] mit
//!    der konfigurierten Lease-Dauer ein — danach fragt
//!    [`harw_tool_shell::ShellExecutor::authorize_host_command`] für **keinen**
//!    weiteren Befehl dieser Sitzung erneut (siehe Kommentar in
//!    `host-process-worker.toml`).
//!
//! # Nebenläufigkeit
//! [`HostPermitDialog`] ist `Send + Sync` (ein `Arc<HostPermitSessionRegistry>`
//! plus ein `mpsc::UnboundedSender`). Der Renderer-Thread wird nie blockiert:
//! die Frage geht über einen ungepufferten Kanal, gewartet wird ausschließlich
//! auf einem `tokio::sync::oneshot`.
//!
//! # Fehlertypen
//! Keine eigenen — jeder Fehlerfall (toter Kanal, fallengelassene Antwort,
//! Zeitablauf) wird zu `false` (Ablehnung), nie zu `Err`.
//!
//! # Bekannte Lücke: kein Renderer pollt [`HostPermitPromptReceiver`] (Stand dieser Welle)
//! Dieses Modul liefert die Sende- und Wartelogik ([`HostPermitDialog`]) und
//! den Frage-/Antwortvertrag ([`HostPermitPrompt`]); es baut aber **keinen**
//! Ratatui-Panel-Konsumenten. Kein heute existierender Renderer-Task ruft
//! `HostPermitPromptReceiver::recv` ab — anders als beim strukturell
//! identischen `crate::approval_dialog`-Panel für normale Werkzeugfreigaben.
//! Praktische Folge: Ein Worker mit `SandboxProfile::Host` löst zwar
//! [`HostPermitDialog::ensure_session_approved`] korrekt aus und die
//! [`HostPermitPrompt`] verlässt den Kanal fehlerfrei — aber ohne einen
//! empfangenden Renderer bleibt sie unbeantwortet im Kanal-Puffer liegen, bis
//! [`DEFAULT_HOST_PERMIT_TIMEOUT`] abläuft und `ensure_session_approved`
//! `false` liefert (Sicherheitsregel „Ablehnung ist der Default“ greift also
//! bereits fail-closed, aber jede Host-Anfrage scheitert heute faktisch immer
//! am Timeout, nie an einer echten Nutzerentscheidung). Das Anschließen eines
//! tatsächlichen Poll-Punkts an `HostPermitPromptReceiver` ist eine noch
//! offene Folgearbeit dieser Welle, keine bereits erledigte Aufgabe dieses
//! Moduls — siehe Abschlussbericht.
//!
//! # Examples
//! ```rust,no_run
//! use std::sync::Arc;
//! use std::time::Duration;
//! use harw_sandbox::HostPermitSessionRegistry;
//! use harw_tui::host_permit_dialog::HostPermitDialog;
//!
//! # async fn demo() {
//! let registry = Arc::new(HostPermitSessionRegistry::default());
//! let (dialog, mut prompts) = HostPermitDialog::new(Arc::clone(&registry), Duration::from_secs(3600));
//!
//! // Renderer-Seite (in einer echten TUI ein eigener Task/Poll-Punkt):
//! tokio::spawn(async move {
//!     if let Some(prompt) = prompts.recv().await {
//!         prompt.approve();
//!     }
//! });
//!
//! let approved = dialog
//!     .ensure_session_approved("session-1", "host-process-worker@1", "tmux ls")
//!     .await;
//! assert!(approved);
//! # }
//! ```

use std::sync::Arc;
use std::time::Duration;

use harw_sandbox::HostPermitSessionRegistry;
use tokio::sync::{mpsc, oneshot};

/// Voreingestellte Wartezeit auf **eine** Nutzerentscheidung.
///
/// Läuft sie ab, gilt das als Ablehnung (siehe
/// [`HostPermitDialog::ensure_session_approved`]).
pub const DEFAULT_HOST_PERMIT_TIMEOUT: Duration = Duration::from_secs(300);

/// Empfängerseite des Fragekanals — der Renderer pollt sie.
///
/// # Bekannte Lücke
/// Stand dieser Welle ruft **kein** Renderer-Task `recv()` auf einem Wert
/// dieses Typs auf (siehe Moduldoku, Abschnitt „Bekannte Lücke"). Jede über
/// diesen Kanal gesendete [`HostPermitPrompt`] bleibt deshalb praktisch bis
/// zum Timeout unbeantwortet liegen — [`HostPermitDialog::ensure_session_approved`]
/// bleibt dabei sicher (Timeout zählt als Ablehnung), aber ohne einen echten
/// Konsumenten kann ein Nutzer heute keiner Host-Anfrage tatsächlich
/// zustimmen.
pub type HostPermitPromptReceiver = mpsc::UnboundedReceiver<HostPermitPrompt>;

/// Eine einzelne Host-Permit-Frage auf dem Weg zum Renderer.
///
/// # Concurrency
/// `Send`; nicht `Sync` (der `oneshot::Sender` ist an einen Besitzer
/// gebunden). Der Wert wird beim Beantworten **konsumiert**.
pub struct HostPermitPrompt {
    session: String,
    worker_definition: String,
    command_hint: String,
    responder: oneshot::Sender<bool>,
}

impl HostPermitPrompt {
    /// Die Sitzung, für die Host-Zugriff angefragt wird.
    #[must_use]
    pub fn session(&self) -> &str {
        &self.session
    }

    /// Die exakte Worker-Definition, die Host-Ausführung deklariert
    /// (z. B. `"host-process-worker@1"`).
    #[must_use]
    pub fn worker_definition(&self) -> &str {
        &self.worker_definition
    }

    /// Der Befehl, der die Anfrage ausgelöst hat — als Kontext für den
    /// Nutzer, nicht als Teil der später tatsächlich autorisierten
    /// Anfrage: die Zustimmung gilt der Sitzung, nicht diesem einen Text.
    #[must_use]
    pub fn command_hint(&self) -> &str {
        &self.command_hint
    }

    /// Beantwortet die Frage mit einer Zustimmung.
    ///
    /// # Returns
    /// `true`, wenn die Antwort den wartenden Aufrufer erreicht hat.
    pub fn approve(self) -> bool {
        self.answer(true)
    }

    /// Beantwortet die Frage mit einer Ablehnung.
    ///
    /// # Returns
    /// `true`, wenn die Antwort den wartenden Aufrufer erreicht hat.
    pub fn deny(self) -> bool {
        self.answer(false)
    }

    fn answer(self, approved: bool) -> bool {
        let session = self.session.clone();
        match self.responder.send(approved) {
            Ok(()) => {
                tracing::debug!(session = %session, approved, "tui.host_permit.answer_delivered");
                true
            }
            Err(_) => {
                tracing::warn!(session = %session, approved, "tui.host_permit.answer_undeliverable");
                false
            }
        }
    }
}

/// Stellt Host-Permit-Fragen und trägt Zustimmungen in die
/// [`HostPermitSessionRegistry`] der laufenden Sitzung ein.
///
/// # Concurrency
/// `Send + Sync`.
pub struct HostPermitDialog {
    registry: Arc<HostPermitSessionRegistry>,
    prompts: mpsc::UnboundedSender<HostPermitPrompt>,
    lease_ttl: Duration,
    timeout: Duration,
}

impl HostPermitDialog {
    /// Baut einen Dialog samt Empfängerseite für den Renderer.
    ///
    /// # Arguments
    /// - `registry` (`Arc<HostPermitSessionRegistry>`): dieselbe Instanz, die
    ///   [`harw_tool_shell::ShellExecutor::authorize_host_command`] über
    ///   `with_host_permit_registry` liest — sonst sieht der Executor die
    ///   Zustimmung nie.
    /// - `lease_ttl` (`Duration`): wie lange eine Zustimmung ohne erneute
    ///   Rückfrage gilt (Sitzungsende bleibt die harte Obergrenze, siehe
    ///   `host-process-worker.toml`).
    ///
    /// # Returns
    /// Den Dialog und die Empfängerseite, die der Renderer pollt.
    #[must_use]
    pub fn new(
        registry: Arc<HostPermitSessionRegistry>,
        lease_ttl: Duration,
    ) -> (Self, HostPermitPromptReceiver) {
        Self::with_timeout(registry, lease_ttl, DEFAULT_HOST_PERMIT_TIMEOUT)
    }

    /// Wie [`Self::new`], mit einer expliziten Antwortfrist statt
    /// [`DEFAULT_HOST_PERMIT_TIMEOUT`] (für Tests und abweichende Vorgaben).
    #[must_use]
    pub fn with_timeout(
        registry: Arc<HostPermitSessionRegistry>,
        lease_ttl: Duration,
        timeout: Duration,
    ) -> (Self, HostPermitPromptReceiver) {
        let (prompts, receiver) = mpsc::unbounded_channel();
        (
            Self {
                registry,
                prompts,
                lease_ttl,
                timeout,
            },
            receiver,
        )
    }

    /// Stellt sicher, dass `session_id` für Host-Ausführung zugestimmt hat —
    /// fragt höchstens einmal je Sitzung.
    ///
    /// # Beschreibung
    /// Hat die Sitzung bereits zugestimmt, kehrt diese Methode ohne Frage
    /// sofort mit `true` zurück (siehe Moduldoku, Ablauf Schritt 2). Sonst
    /// öffnet sie eine [`HostPermitPrompt`] und wartet bis zu `timeout` auf
    /// genau eine Antwort; jeder Fehlerfall — toter Kanal, fallengelassene
    /// Antwort, Zeitablauf — zählt als Ablehnung. Bei Zustimmung trägt sie
    /// die Sitzung mit der konfigurierten Lease-Dauer in die Registry ein.
    ///
    /// # Arguments
    /// - `session_id` (`&str`): die anfragende Sitzung.
    /// - `worker_definition` (`&str`): die anfragende Worker-Definition.
    /// - `command_hint` (`&str`): der Befehl, der die Anfrage ausgelöst hat
    ///   (nur zur Anzeige, siehe [`HostPermitPrompt::command_hint`]).
    ///
    /// # Returns
    /// `true`, wenn die Sitzung nach diesem Aufruf zugestimmt hat.
    ///
    /// # Concurrency
    /// Blockiert den Renderer-Thread nicht; wartet `await`-basiert.
    pub async fn ensure_session_approved(
        &self,
        session_id: &str,
        worker_definition: &str,
        command_hint: &str,
    ) -> bool {
        if self.registry.is_session_approved(session_id) {
            return true;
        }

        let (responder, answer) = oneshot::channel();
        let prompt = HostPermitPrompt {
            session: session_id.to_owned(),
            worker_definition: worker_definition.to_owned(),
            command_hint: command_hint.to_owned(),
            responder,
        };
        if self.prompts.send(prompt).is_err() {
            tracing::warn!(session_id, "tui.host_permit.prompt_undeliverable");
            return false;
        }

        let approved = match tokio::time::timeout(self.timeout, answer).await {
            Ok(Ok(approved)) => approved,
            Ok(Err(_)) => {
                tracing::warn!(session_id, "tui.host_permit.answer_dropped");
                false
            }
            Err(_elapsed) => {
                tracing::warn!(session_id, "tui.host_permit.timed_out");
                false
            }
        };

        if approved {
            self.registry.mark_session_approved(session_id, self.lease_ttl);
        }
        approved
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_ensure_session_approved_already_approved_session_is_not_asked_again() {
        let registry = Arc::new(HostPermitSessionRegistry::default());
        registry.mark_session_approved("s1", Duration::from_secs(60));
        let (dialog, mut prompts) =
            HostPermitDialog::with_timeout(Arc::clone(&registry), Duration::from_secs(60), Duration::from_millis(50));

        let approved = dialog
            .ensure_session_approved("s1", "host-process-worker@1", "echo hi")
            .await;

        assert!(approved);
        assert!(prompts.try_recv().is_err(), "no prompt must be sent for an already-approved session");
    }

    #[tokio::test]
    async fn test_ensure_session_approved_explicit_approval_marks_the_session() {
        let registry = Arc::new(HostPermitSessionRegistry::default());
        let (dialog, mut prompts) =
            HostPermitDialog::with_timeout(Arc::clone(&registry), Duration::from_secs(60), Duration::from_secs(5));

        let responder = tokio::spawn(async move {
            let prompt = prompts.recv().await.expect("prompt must arrive");
            assert_eq!(prompt.session(), "s1");
            assert_eq!(prompt.worker_definition(), "host-process-worker@1");
            assert_eq!(prompt.command_hint(), "echo hi");
            assert!(prompt.approve());
        });

        let approved = dialog
            .ensure_session_approved("s1", "host-process-worker@1", "echo hi")
            .await;

        responder.await.expect("responder task must not panic");
        assert!(approved);
        assert!(registry.is_session_approved("s1"));
    }

    #[tokio::test]
    async fn test_ensure_session_approved_explicit_denial_does_not_mark_the_session() {
        let registry = Arc::new(HostPermitSessionRegistry::default());
        let (dialog, mut prompts) =
            HostPermitDialog::with_timeout(Arc::clone(&registry), Duration::from_secs(60), Duration::from_secs(5));

        tokio::spawn(async move {
            let prompt = prompts.recv().await.expect("prompt must arrive");
            assert!(prompt.deny());
        });

        let approved = dialog
            .ensure_session_approved("s1", "host-process-worker@1", "rm -rf /")
            .await;

        assert!(!approved);
        assert!(!registry.is_session_approved("s1"));
    }

    #[tokio::test]
    async fn test_ensure_session_approved_dropped_prompt_fails_closed() {
        let registry = Arc::new(HostPermitSessionRegistry::default());
        let (dialog, mut prompts) =
            HostPermitDialog::with_timeout(Arc::clone(&registry), Duration::from_secs(60), Duration::from_secs(5));

        tokio::spawn(async move {
            let _prompt = prompts.recv().await.expect("prompt must arrive");
            // Bewusst nicht beantworten: der `responder` wird beim Drop von
            // `_prompt` fallengelassen.
        });

        let approved = dialog
            .ensure_session_approved("s1", "host-process-worker@1", "echo hi")
            .await;

        assert!(!approved);
        assert!(!registry.is_session_approved("s1"));
    }

    #[tokio::test]
    async fn test_ensure_session_approved_closed_channel_fails_closed() {
        let registry = Arc::new(HostPermitSessionRegistry::default());
        let (dialog, prompts) =
            HostPermitDialog::with_timeout(Arc::clone(&registry), Duration::from_secs(60), Duration::from_secs(5));
        drop(prompts);

        let approved = dialog
            .ensure_session_approved("s1", "host-process-worker@1", "echo hi")
            .await;

        assert!(!approved);
        assert!(!registry.is_session_approved("s1"));
    }

    #[tokio::test]
    async fn test_ensure_session_approved_timeout_fails_closed() {
        let registry = Arc::new(HostPermitSessionRegistry::default());
        let (dialog, mut prompts) =
            HostPermitDialog::with_timeout(Arc::clone(&registry), Duration::from_secs(60), Duration::from_millis(20));

        // Hält die Frage offen, ohne zu antworten, länger als das Timeout.
        let _keep_open = tokio::spawn(async move {
            let prompt = prompts.recv().await.expect("prompt must arrive");
            tokio::time::sleep(Duration::from_secs(5)).await;
            drop(prompt);
        });

        let approved = dialog
            .ensure_session_approved("s1", "host-process-worker@1", "echo hi")
            .await;

        assert!(!approved);
        assert!(!registry.is_session_approved("s1"));
    }
}
