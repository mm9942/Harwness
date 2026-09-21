//! Fragekanal für Host-Profil-Permit-Anfragen zwischen [`crate::exec::ShellExecutor`]
//! und einer anzeigenden Oberfläche (z. B. `harw-tui`).
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

use std::path::{Path, PathBuf};

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
            },
            answer,
        )
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

    #[test]
    fn test_default_variant_is_single_execution() {
        assert_eq!(HostPermitVariant::default(), HostPermitVariant::SingleExecution);
    }

    #[test]
    fn test_label_is_distinct_per_variant() {
        assert_ne!(
            HostPermitVariant::SingleExecution.label(),
            HostPermitVariant::SessionLease.label()
        );
    }

    #[tokio::test]
    async fn test_prompt_approve_delivers_the_chosen_variant() {
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
        assert_eq!(prompt.preselected_variant(), HostPermitVariant::SingleExecution);

        assert!(prompt.approve(HostPermitVariant::SessionLease));
        let decision = answer.await.expect("responder must deliver an answer");
        assert_eq!(decision, Some(HostPermitVariant::SessionLease));
    }

    #[tokio::test]
    async fn test_prompt_deny_delivers_none() {
        let (prompt, answer) = HostPermitPrompt::new(
            "s1".to_owned(),
            "host-process-worker@1".to_owned(),
            "echo hi".to_owned(),
            PathBuf::from("/workspace"),
            HostPermitVariant::SingleExecution,
        );
        assert!(prompt.deny());
        let decision = answer.await.expect("responder must deliver an answer");
        assert_eq!(decision, None);
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
        assert!(answer.await.is_err(), "a dropped prompt must close the oneshot channel");
    }

    #[tokio::test]
    async fn test_channel_delivers_a_sent_prompt() {
        let (sender, mut receiver) = host_permit_prompt_channel();
        let (prompt, _answer) = HostPermitPrompt::new(
            "s1".to_owned(),
            "host-process-worker@1".to_owned(),
            "echo hi".to_owned(),
            PathBuf::from("/workspace"),
            HostPermitVariant::SessionLease,
        );
        assert!(sender.send(prompt).is_ok());
        let received = receiver.recv().await.expect("prompt must arrive");
        assert_eq!(received.session(), "s1");
        assert_eq!(received.preselected_variant(), HostPermitVariant::SessionLease);
    }
}
