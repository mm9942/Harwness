//! Fragekanal für Host-Profil-Permit-Anfragen zwischen [`crate::exec::ShellExecutor`]
//! und einer anzeigenden Oberfläche (z. B. `harw-tui`).
//!
//! Spec source: `/home/mia/.claude/plans/recursive-cooking-lobster.md`, Teil B5
//! (`SANDBOX_LEASE_WORKER_DEFINITION`) und Teil B3 (`HostPermitHandles`).
//!
//! # Verantwortungsbereich
//! Dieses Modul besitzt genau den Vertrag, über den [`crate::exec::ShellExecutor`]
//! (Sendeseite, [`HostPermitPromptSender`]) und ein Renderer (Empfängerseite,
//! [`HostPermitPromptReceiver`]) eine einzelne Host-Permit-Frage austauschen —
//! nicht die Entscheidungslogik selbst (die bleibt in
//! [`crate::exec::ShellExecutor::authorize_host_command`]) und nicht die
//! Anzeige (die liegt beim Renderer, z. B. `harw_tui::host_permit_dialog`).
//!
//! # Warum dieser Vertrag hier liegt
//! `harw-tool-shell` darf nicht von `harw-tui` abhängen (sonst entstünde ein
//! Kreislauf: `harw-tui` hängt bereits von `harw-tool-shell` ab), muss aber
//! trotzdem eine Frage an eine Oberfläche schicken können, die weit außerhalb
//! dieses Crates lebt. Dieses Modul ist deshalb der **eine** geteilte Ort für
//! Frage- und Antworttyp: `harw-tool-shell` (Produzent, sendet über
//! [`HostPermitPromptSender`]), `harw-runtime` (baut den Kanal und hält beide
//! Enden, siehe `harw_runtime::RuntimeAssembly::host_permit_prompt_sender`/
//! `take_host_permit_prompts`) und `harw-tui` (Konsument, pollt
//! [`HostPermitPromptReceiver`]) hängen alle bereits von `harw-tool-shell`
//! bzw. können das gefahrlos, ohne dass `harw-tool-shell` selbst irgendeine
//! UI-Abhängigkeit bekommt.
//!
//! # Schlüsseltypen
//! - [`HostPermitVariant`] — die zwei Freigabevarianten (Einzelfreigabe /
//!   Sitzungsphase), siehe `docs/design/mediated-process-execution.md`
//!   Abschnitt „Semantisch angefragter Host-Modus".
//! - [`HostPermitPrompt`] — die Frage, die beim Renderer ankommt.
//! - [`HostPermitPromptSender`] / [`HostPermitPromptReceiver`] — die beiden
//!   Enden des `mpsc`-Kanals.
//! - [`HostPermitHandles`] — gebündelter ServiceMap-Eintrag (Ledger,
//!   Registry, Fragekanal), siehe Plan Teil B3.
//! - [`SANDBOX_LEASE_WORKER_DEFINITION`] — Worker-Definition der
//!   `sandbox-lease`-Operation (Plan Teil B5).
//!
//! # Sicherheitsregel: Ablehnung ist der Default
//! Ein geschlossener Fragekanal, eine fallengelassene Antwort oder ein
//! Zeitablauf sind **keine** Zustimmung. Nur [`HostPermitPrompt::approve`]
//! verbucht eine Zustimmung; [`HostPermitPrompt::deny`] und jeder Drop des
//! Werts ohne Antwort verbuchen eine Ablehnung. Diese Regel wird von
//! [`crate::exec::ShellExecutor::authorize_host_command`] durchgesetzt, nicht
//! von diesem Modul — hier gibt es keinen Pfad, der stillschweigend zu
//! [`Some`] wird.
//!
//! # Concurrency
//! [`HostPermitPrompt`] ist `Send`, nicht `Sync` (der innere
//! `oneshot::Sender` ist an einen Besitzer gebunden) und wird beim
//! Beantworten **konsumiert**. [`HostPermitPromptSender`] ist `Clone + Send +
//! Sync` (ein `tokio::sync::mpsc::UnboundedSender`).

use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use harw_sandbox::{HostPermitSessionRegistry, ProcessPermitLedger};
use tokio::sync::{mpsc, oneshot};

/// Die zwei Freigabevarianten aus
/// `docs/design/mediated-process-execution.md` Abschnitt „Semantisch
/// angefragter Host-Modus".
///
/// # Description
/// [`Self::SingleExecution`] genehmigt genau den einen kanonischen Auftrag,
/// der die Anfrage ausgelöst hat, und ist nach einer Ausführung verbraucht.
/// [`Self::SessionLease`] genehmigt eine begrenzte Host-Arbeitsphase: weitere,
/// auch abweichende Aufträge derselben Sitzung erhalten ohne erneute
/// Rückfrage ihren eigenen Permit, bis die Phase abläuft oder ausdrücklich
/// beendet wird.
///
/// Welche Variante ein Aufrufer als [`HostPermitPrompt::preselected_variant`]
/// vorschlägt, ändert nie, was tatsächlich genehmigt wird — das entscheidet
/// ausschließlich [`HostPermitPrompt::approve`].
///
/// Die Vorgabe ([`Default`]) ist [`Self::SingleExecution`] — die engste
/// Variante, für jeden Aufrufer, der keine sitzungsbezogene Information
/// (etwa den aktiven `InteractionMode`) auswerten kann oder will (siehe
/// `ShellToolProvider::with_preselected_permit_variant`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum HostPermitVariant {
    /// Variante 1: genehmigt genau den einen Auftrag, der die Anfrage
    /// ausgelöst hat; nach einer Ausführung verbraucht.
    #[default]
    SingleExecution,
    /// Variante 2: genehmigt eine begrenzte Host-Arbeitsphase für die
    /// laufende Sitzung.
    SessionLease,
}

impl HostPermitVariant {
    /// Deutschsprachige Anzeigebeschriftung für eine anzeigende Oberfläche.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::SingleExecution => "Einmalig für diesen Auftrag",
            Self::SessionLease => "Host-Arbeitsphase für diese Sitzung",
        }
    }
}

/// Sendeseite des Fragekanals — hält [`crate::exec::ShellExecutor`].
pub type HostPermitPromptSender = mpsc::UnboundedSender<HostPermitPrompt>;

/// Empfängerseite des Fragekanals — pollt der Renderer.
pub type HostPermitPromptReceiver = mpsc::UnboundedReceiver<HostPermitPrompt>;

/// Baut einen frischen, ungepufferten Fragekanal.
///
/// # Returns
/// `(sender, receiver)` — der Sender wandert in einen [`ShellToolProvider`]
/// (`with_host_permit_prompts`), der Empfänger an den Renderer.
///
/// [`ShellToolProvider`]: crate::exec::ShellToolProvider
#[must_use]
pub fn host_permit_prompt_channel() -> (HostPermitPromptSender, HostPermitPromptReceiver) {
    mpsc::unbounded_channel()
}

/// Einzige Worker-Definition der `sandbox-lease`-Operation (Plan
/// `recursive-cooking-lobster.md` Teil B5, `harw-ops/src/sandbox_lease.rs`):
/// eine über das Modell-Tool direkt angefragte Host-Freigabe trägt diesen
/// Wert als [`HostPermitPrompt::worker_definition`], statt eines
/// eingebetteten Host-Profil-Workers wie [`HostPermitPrompt`] es sonst
/// üblicherweise transportiert — damit kann eine anzeigende Oberfläche den
/// Dialogtext einer Sitzungsfreigabe-Anfrage von dem eines einzelnen
/// Host-Profil-Befehls unterscheiden (Plan Teil B6).
pub const SANDBOX_LEASE_WORKER_DEFINITION: &str = "sandbox-lease";

/// Gebündelte Host-Permit-Handles — ein einzelner ServiceMap-Eintrag
/// (`Arc<HostPermitHandles>`, Plan Teil B3), statt Ledger, Registry und
/// Fragekanal einzeln durch jede Verdrahtungsebene zu reichen.
///
/// # Description
/// Trägt genau die drei Teile, die [`ShellToolProvider`] für Host-Profil-
/// und Sitzungsfreigabe-Ausführung braucht:
/// [`ShellToolProvider::with_permit_ledger`], [`ShellToolProvider::with_host_permit_registry`]
/// und [`ShellToolProvider::with_host_permit_prompts`]. `prompts` ist
/// `Option`, weil manche Verdrahtungsebenen (z. B. eine kopflose Ausführung
/// ohne UI) bewusst keine Oberfläche anhängen — das bleibt fail-closed, wie
/// bei [`ShellToolProvider::host_permit_prompts`] dokumentiert.
///
/// # Concurrency
/// `Clone`: `ledger`/`registry` klonen nur den `Arc`-Zeiger
/// ([`Arc::clone`]), `prompts` klont den `mpsc::UnboundedSender`. `Debug` ist
/// manuell implementiert, damit `prompts` nie mehr als `"<sender>"` zeigt —
/// der Sender selbst trägt keine sensiblen Daten, aber sein `Debug`-Format
/// ist kanalinterner Implementierungsdetail, keine für Logs gedachte
/// Information.
///
/// [`ShellToolProvider`]: crate::exec::ShellToolProvider
/// [`ShellToolProvider::with_permit_ledger`]: crate::exec::ShellToolProvider::with_permit_ledger
/// [`ShellToolProvider::with_host_permit_registry`]: crate::exec::ShellToolProvider::with_host_permit_registry
/// [`ShellToolProvider::with_host_permit_prompts`]: crate::exec::ShellToolProvider::with_host_permit_prompts
/// [`ShellToolProvider::host_permit_prompts`]: crate::exec::ShellToolProvider::host_permit_prompts
#[derive(Clone)]
pub struct HostPermitHandles {
    /// Der Permit-Ledger für Host-Profil-Ausführung.
    pub ledger: Arc<ProcessPermitLedger>,
    /// Die sitzungsseitige Zuordnung von UI-Zustimmungen zu Permits (Session-
    /// und Einmalfreigaben, siehe [`HostPermitSessionRegistry`]).
    pub registry: Arc<HostPermitSessionRegistry>,
    /// Sendeseite des Host-Permit-Fragekanals; `None`, wenn keine
    /// anzeigende Oberfläche angehängt ist (fail-closed, siehe Moduldoku).
    pub prompts: Option<HostPermitPromptSender>,
}

impl fmt::Debug for HostPermitHandles {
    /// Zeigt `ledger`/`registry` über ihr eigenes `Debug`, aber `prompts`
    /// nur als `"<sender>"` (bzw. `None`) — nie den internen Kanalzustand.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HostPermitHandles")
            .field("ledger", &self.ledger)
            .field("registry", &self.registry)
            .field("prompts", &self.prompts.as_ref().map(|_sender| "<sender>"))
            .finish()
    }
}

/// Eine einzelne Host-Permit-Frage auf dem Weg zum Renderer.
///
/// # Concurrency
/// `Send`; nicht `Sync` (der `oneshot::Sender` ist an einen Besitzer
/// gebunden). Der Wert wird beim Beantworten **konsumiert**.
#[derive(Debug)]
pub struct HostPermitPrompt {
    session: String,
    worker_definition: String,
    command: String,
    workspace: PathBuf,
    preselected: HostPermitVariant,
    responder: oneshot::Sender<Option<HostPermitVariant>>,
    /// Runde 5, Teil N: wer fragt (Rolle, Kind-ID, Baum-Pfad) — nur bei einer
    /// Host-Mode-Anfrage aus dem Agentenbaum gesetzt.
    requester: Option<crate::host_escalation::HostRequester>,
    /// Runde 5, Teil N: der vom Modell genannte Grund einer Host-Mode-Anfrage
    /// (reine Anzeige, nie ausgewertet).
    reason: Option<String>,
}

impl HostPermitPrompt {
    /// Baut eine neue Frage plus die zugehörige Antwort-Empfängerseite.
    ///
    /// # Arguments
    /// - `session` (`String`): die Sitzung, für die Host-Zugriff angefragt wird.
    /// - `worker_definition` (`String`): die eingebettete Worker-Definition,
    ///   die Host-Ausführung deklariert.
    /// - `command` (`String`): der exakte kanonische Befehl, der die Anfrage
    ///   ausgelöst hat.
    /// - `workspace` (`PathBuf`): die kanonische Workspace-Wurzel des
    ///   auslösenden Aufrufs.
    /// - `preselected` ([`HostPermitVariant`]): reine Anzeige-Vorauswahl.
    ///
    /// # Returns
    /// Die Frage und den `oneshot::Receiver`, über den der Aufrufer
    /// `await`-basiert auf genau eine Antwort wartet.
    #[must_use]
    pub fn new(
        session: String,
        worker_definition: String,
        command: String,
        workspace: PathBuf,
        preselected: HostPermitVariant,
    ) -> (Self, oneshot::Receiver<Option<HostPermitVariant>>) {
        let (responder, answer) = oneshot::channel();
        (
            Self {
                session,
                worker_definition,
                command,
                workspace,
                preselected,
                responder,
                requester: None,
                reason: None,
            },
            answer,
        )
    }

    /// Runde 5, Teil N: hängt an, wer fragt (Rolle, Kind-ID, Baum-Pfad).
    /// Reine Anzeige — ändert nie, was [`Self::approve`] genehmigt.
    #[must_use]
    pub fn with_requester(mut self, requester: crate::host_escalation::HostRequester) -> Self {
        self.requester = Some(requester);
        self
    }

    /// Runde 5, Teil N: hängt den Grund einer Host-Mode-Anfrage an (reine
    /// Anzeige).
    #[must_use]
    pub fn with_reason(mut self, reason: impl Into<String>) -> Self {
        self.reason = Some(reason.into());
        self
    }

    /// Wer fragt — `Some` nur bei einer Host-Mode-Anfrage aus dem Agentenbaum
    /// (`shell.exec` mit `request_host`).
    #[must_use]
    pub fn requester(&self) -> Option<&crate::host_escalation::HostRequester> {
        self.requester.as_ref()
    }

    /// Der Grund einer Host-Mode-Anfrage, falls angegeben.
    #[must_use]
    pub fn reason(&self) -> Option<&str> {
        self.reason.as_deref()
    }

    /// Die Sitzung, für die Host-Zugriff angefragt wird.
    #[must_use]
    pub fn session(&self) -> &str {
        &self.session
    }

    /// Die exakte eingebettete Worker-Definition, die Host-Ausführung
    /// deklariert (z. B. `"host-process-worker@1"`).
    #[must_use]
    pub fn worker_definition(&self) -> &str {
        &self.worker_definition
    }

    /// Der exakte kanonische Befehl, der die Anfrage ausgelöst hat. Teil der
    /// später tatsächlich autorisierten Anfrage: eine
    /// [`HostPermitVariant::SingleExecution`]-Zustimmung bindet den
    /// ausgestellten Permit exakt an diesen Text.
    #[must_use]
    pub fn command(&self) -> &str {
        &self.command
    }

    /// Die kanonische Workspace-Wurzel des auslösenden Aufrufs.
    #[must_use]
    pub fn workspace(&self) -> &Path {
        &self.workspace
    }

    /// Die vom Aufrufer vorgeschlagene Variante. Reine Anzeige-Vorauswahl —
    /// ändert nie, was [`Self::approve`] tatsächlich genehmigt.
    #[must_use]
    pub fn preselected_variant(&self) -> HostPermitVariant {
        self.preselected
    }

    /// Beantwortet die Frage mit einer Zustimmung zu `variant`.
    ///
    /// # Returns
    /// `true`, wenn die Antwort den wartenden Aufrufer erreicht hat.
    pub fn approve(self, variant: HostPermitVariant) -> bool {
        self.answer(Some(variant))
    }

    /// Beantwortet die Frage mit einer Ablehnung.
    ///
    /// # Returns
    /// `true`, wenn die Antwort den wartenden Aufrufer erreicht hat.
    pub fn deny(self) -> bool {
        self.answer(None)
    }

    fn answer(self, decision: Option<HostPermitVariant>) -> bool {
        let session = self.session.clone();
        let approved = decision.is_some();
        match self.responder.send(decision) {
            Ok(()) => {
                tracing::debug!(session = %session, approved, "shell.host_permit.answer_delivered");
                true
            }
            Err(_) => {
                tracing::warn!(session = %session, approved, "shell.host_permit.answer_undeliverable");
                false
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult, ctx};

    #[test]
    fn test_default_variant_is_single_execution() {
        assert_eq!(
            HostPermitVariant::default(),
            HostPermitVariant::SingleExecution
        );
    }

    #[test]
    fn test_label_is_distinct_per_variant() {
        assert_ne!(
            HostPermitVariant::SingleExecution.label(),
            HostPermitVariant::SessionLease.label()
        );
    }

    #[tokio::test]
    async fn test_prompt_approve_delivers_the_chosen_variant() -> TestResult {
        let (prompt, answer) = HostPermitPrompt::new(
            "s1".to_owned(),
            "host-process-worker@1".to_owned(),
            "echo hi".to_owned(),
            PathBuf::from("/workspace"),
            HostPermitVariant::SingleExecution,
        );
        assert_eq!(prompt.session(), "s1");
        assert_eq!(prompt.command(), "echo hi");
        assert_eq!(prompt.workspace(), Path::new("/workspace"));
        assert_eq!(
            prompt.preselected_variant(),
            HostPermitVariant::SingleExecution
        );

        assert!(prompt.approve(HostPermitVariant::SessionLease));
        let decision = answer
            .await
            .map_err(ctx("responder must deliver an answer"))?;
        assert_eq!(decision, Some(HostPermitVariant::SessionLease));
        Ok(())
    }

    #[tokio::test]
    async fn test_prompt_deny_delivers_none() -> TestResult {
        let (prompt, answer) = HostPermitPrompt::new(
            "s1".to_owned(),
            "host-process-worker@1".to_owned(),
            "echo hi".to_owned(),
            PathBuf::from("/workspace"),
            HostPermitVariant::SingleExecution,
        );
        assert!(prompt.deny());
        let decision = answer
            .await
            .map_err(ctx("responder must deliver an answer"))?;
        assert_eq!(decision, None);
        Ok(())
    }

    #[tokio::test]
    async fn test_dropped_prompt_closes_the_answer_channel() {
        let (prompt, answer) = HostPermitPrompt::new(
            "s1".to_owned(),
            "host-process-worker@1".to_owned(),
            "echo hi".to_owned(),
            PathBuf::from("/workspace"),
            HostPermitVariant::SingleExecution,
        );
        drop(prompt);
        assert!(
            answer.await.is_err(),
            "a dropped prompt must close the oneshot channel"
        );
    }

    #[tokio::test]
    async fn test_channel_delivers_a_sent_prompt() -> TestResult {
        let (sender, mut receiver) = host_permit_prompt_channel();
        let (prompt, _answer) = HostPermitPrompt::new(
            "s1".to_owned(),
            "host-process-worker@1".to_owned(),
            "echo hi".to_owned(),
            PathBuf::from("/workspace"),
            HostPermitVariant::SessionLease,
        );
        assert!(sender.send(prompt).is_ok());
        let received = receiver
            .recv()
            .await
            .ok_or(TestError::Missing("prompt must arrive"))?;
        assert_eq!(received.session(), "s1");
        assert_eq!(
            received.preselected_variant(),
            HostPermitVariant::SessionLease
        );
        Ok(())
    }

    #[test]
    fn test_requester_and_reason_are_carried_but_default_to_none() {
        let (plain, _answer) = HostPermitPrompt::new(
            "s1".to_owned(),
            "host-process-worker@1".to_owned(),
            "echo hi".to_owned(),
            PathBuf::from("/workspace"),
            HostPermitVariant::SingleExecution,
        );
        assert!(plain.requester().is_none());
        assert!(plain.reason().is_none());
        let requester = crate::host_escalation::HostRequester {
            role: "executor".to_owned(),
            session: "child-1".to_owned(),
            path: "uia › root-orchestrator › executor".to_owned(),
        };
        let prompt = plain
            .with_requester(requester.clone())
            .with_reason("cargo fetch braucht Netz");
        assert_eq!(prompt.requester(), Some(&requester));
        assert_eq!(prompt.reason(), Some("cargo fetch braucht Netz"));
    }

    #[test]
    fn test_sandbox_lease_worker_definition_is_the_documented_literal() {
        assert_eq!(SANDBOX_LEASE_WORKER_DEFINITION, "sandbox-lease");
    }

    #[test]
    fn test_host_permit_handles_clone_shares_the_same_ledger_and_registry() {
        let handles = HostPermitHandles {
            ledger: Arc::new(ProcessPermitLedger::default()),
            registry: Arc::new(HostPermitSessionRegistry::default()),
            prompts: None,
        };
        let cloned = handles.clone();
        assert!(Arc::ptr_eq(&handles.ledger, &cloned.ledger));
        assert!(Arc::ptr_eq(&handles.registry, &cloned.registry));
    }

    #[test]
    fn test_host_permit_handles_debug_hides_sender_details() {
        let (sender, _receiver) = host_permit_prompt_channel();
        let handles = HostPermitHandles {
            ledger: Arc::new(ProcessPermitLedger::default()),
            registry: Arc::new(HostPermitSessionRegistry::default()),
            prompts: Some(sender),
        };
        let debugged = format!("{handles:?}");
        assert!(
            debugged.contains("\"<sender>\""),
            "prompts must be redacted to \"<sender>\", got: {debugged}"
        );
        assert!(
            !debugged.to_lowercase().contains("unboundedsender"),
            "the sender's own Debug internals must not leak, got: {debugged}"
        );
    }

    #[test]
    fn test_host_permit_handles_debug_shows_none_without_a_sender() {
        let handles = HostPermitHandles {
            ledger: Arc::new(ProcessPermitLedger::default()),
            registry: Arc::new(HostPermitSessionRegistry::default()),
            prompts: None,
        };
        let debugged = format!("{handles:?}");
        assert!(debugged.contains("prompts: None"), "{debugged}");
    }
}
