//! Die Freigabekette eines montierten Laufs (`ApprovalChain`).
//!
//! # Verantwortlichkeit
//! Dieses Modul besitzt genau eine Sache: **welche** [`ApprovalHandler`] ein
//! Einstieg in **welcher Reihenfolge** registriert und was ein Kind davon
//! erbt. Es entscheidet keinen einzelnen Aufruf — das tun die Handler selbst,
//! aggregiert von `harw_core::turn_loop::check_approval` (`Deny` > `AskUser` >
//! `Allow`, Welle W1-05).
//!
//! # Schlüsseltypen
//! - [`ApprovalChain`] — die Kette samt ihrer [`ApprovalModeCell`].
//!
//! # Warum es diesen Typ gibt
//! Vor dieser Welle baute ausschließlich `harw-cli` eine
//! [`ConfigApprovalPolicy`], und zwar **nur** innerhalb des
//! `PlanningStartup` — also nur, wenn `[tools.plan].enabled = true` war
//! (Befunde G-010/F-154). Wer `[policy].require_approval_for` setzte, aber
//! das Plan-Werkzeug nicht einschaltete, bekam seine Pflichtliste schlicht
//! nicht: eine Konfiguration, die mehr Rückfragen anordnet, blieb wirkungslos.
//! [`ApprovalChain::for_root`] koppelt die Politik deshalb ausschließlich an
//! die Liste selbst.
//!
//! Der zweite Befund (F-018) betrifft die Kinder: eine Kind-Sitzung montierte
//! ihre Registry aus den Voreinstellungen und sah die Config-Politik des
//! Wurzelprozesses nie. Ein Fan-out konnte damit tun, was dem Elternteil
//! ausdrücklich unter Vorbehalt stand. [`ApprovalChain::for_child`] gibt die
//! Config-Politik deshalb unverändert weiter und **senkt** dabei zugleich den
//! Freigabemodus, statt ihn zu erben (G-009).
//!
//! # Nebenläufigkeit
//! [`ApprovalChain`] hält nur `Arc`-Zeiger und eine [`ApprovalModeCell`];
//! Klonen kopiert keine Innereien. Alle Handler sind `Send + Sync`.
//!
//! # Fehler
//! Das Modul erzeugt keine Fehler: eine Kette lässt sich aus jeder
//! Konfiguration bilden, auch aus der leeren.

use std::sync::Arc;

use harw_config::ResolvedConfig;
use harw_core::ConfigApprovalPolicy;
use harw_extension_api::approval_mode::{ApprovalMode, ApprovalModeCell};
use harw_extension_api::contributors::ApprovalHandlerKind;
use harw_extension_api::{ApprovalHandler, ExtensionRegistryBuilder};
use harw_registry_defaults::DefaultApprovalPolicy;

/// Stabiler Kurzname der Konfigurationspolitik in [`ApprovalChain::snapshot`].
///
/// `ConfigApprovalPolicy` überschreibt `ApprovalHandler::label` selbst nicht
/// (sie meldete `"unnamed"`/[`ApprovalHandlerKind::Other`]). Die Kette weiß
/// aber, welchen Handler sie an welche Stelle gesetzt hat, und benennt ihn
/// deshalb hier — ohne fremden Code zu ändern.
pub const CONFIG_POLICY_LABEL: &str = "config-policy";

/// Stabiler Kurzname der eingebauten Standardpolitik.
pub const DEFAULT_POLICY_LABEL: &str = "default-policy";

/// Die Freigabekette eines montierten Laufs, in Auswertungsreihenfolge.
///
/// # Beschreibung
/// Die Reihenfolge ist Config → Default → Responder:
/// 1. **Config** ([`ConfigApprovalPolicy`]) aus `[policy].require_approval_for`
///    — vorhanden genau dann, wenn die Liste nicht leer ist.
/// 2. **Default** ([`DefaultApprovalPolicy`]) über der [`ApprovalModeCell`]
///    dieses Laufs — die fail-closed Grundlinie.
/// 3. **Responder** — der interaktive oder kanalgebundene Handler des
///    Einstiegs, falls es überhaupt jemanden zu fragen gibt.
///
/// Die Reihenfolge ist Dokumentation, keine Priorität: `check_approval`
/// befragt seit W1-05 **jeden** Handler und aggregiert `Deny` > `AskUser` >
/// `Allow`. Ein angehängter Handler kann darum nur weiter einschränken, nie
/// lockern. Genau deshalb ist es unschädlich, die Config-Politik immer
/// vorneweg zu stellen.
#[derive(Clone)]
pub struct ApprovalChain {
    /// Politik aus `[policy].require_approval_for`; `None` bei leerer Liste.
    config: Option<Arc<ConfigApprovalPolicy>>,
    /// Die Werkzeugnamen hinter `config`, sortiert und dublettenfrei.
    ///
    /// [`ConfigApprovalPolicy`] bietet keinen Lesezugriff auf ihre Liste
    /// (nur `requires_approval(&ToolCall)`); für
    /// [`crate::spec::RightsSnapshot::config_policy_tools`] hält die Kette
    /// die Namen deshalb selbst.
    config_tools: Vec<String>,
    /// Die eingebaute Standardpolitik über [`Self::mode`].
    default: Arc<DefaultApprovalPolicy>,
    /// Wer gefragt wird, wenn ein Handler [`harw_extension_api::ApprovalDecision::AskUser`]
    /// liefert; `None` in jedem Lauf, in dem niemand anwesend ist.
    responder: Option<Arc<dyn ApprovalHandler>>,
    /// Der Freigabemodus dieses Laufs. Geteilt mit [`Self::default`], damit
    /// eine Umschaltung sofort wirkt.
    mode: ApprovalModeCell,
}

impl std::fmt::Debug for ApprovalChain {
    /// Zeigt die Gestalt der Kette, nicht die Innereien der Handler
    /// (`Arc<dyn ApprovalHandler>` ist nicht `Debug`, und `Debug` als
    /// Supertrait zu verlangen würde jeden Implementierer binden).
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ApprovalChain")
            .field("config_tools", &self.config_tools)
            .field("responder", &self.responder.as_ref().map(|r| r.label()))
            .field("mode", &self.mode.get())
            .finish()
    }
}

impl ApprovalChain {
    /// Baut die Kette eines **Wurzel**-Einstiegs.
    ///
    /// # Beschreibung
    /// Die [`ConfigApprovalPolicy`] entsteht **immer**, sobald
    /// `[policy].require_approval_for` nicht leer ist — unabhängig von
    /// `[tools.plan].enabled` und von jedem anderen Schalter (G-010/F-154).
    /// Eine leere Liste erzeugt keinen Handler: ein Handler, der jeden Aufruf
    /// durchwinkt, wäre nur Rauschen in der Kette und in jedem Snapshot.
    ///
    /// # Arguments
    /// - `config` (`&ResolvedConfig`): die gemergte Konfiguration; gelesen
    ///   wird ausschließlich `harness.policy.require_approval_for`.
    /// - `mode` ([`ApprovalModeCell`]): die Modus-Zelle dieses Laufs.
    ///   Eigentum geht über; ein Klon beim Aufrufer bleibt der Schalter
    ///   (`/permissions set`), mit dem sich der Modus umstellen lässt.
    /// - `responder` (`Option<Arc<dyn ApprovalHandler>>`): der interaktive
    ///   bzw. kanalgebundene Handler des Einstiegs, oder `None`.
    ///
    /// # Rückgabe
    /// Die Kette in der Reihenfolge Config → Default → Responder.
    #[must_use]
    pub fn for_root(
        config: &ResolvedConfig,
        mode: ApprovalModeCell,
        responder: Option<Arc<dyn ApprovalHandler>>,
    ) -> Self {
        let section = &config.harness.policy;
        let mut config_tools: Vec<String> = section.require_approval_for.clone();
        config_tools.sort();
        config_tools.dedup();

        let config_policy = if config_tools.is_empty() {
            None
        } else {
            Some(Arc::new(ConfigApprovalPolicy::new(config_tools.clone())))
        };

        Self {
            config: config_policy,
            config_tools,
            default: Arc::new(DefaultApprovalPolicy::new(mode.clone())),
            responder,
            mode,
        }
    }

    /// Leitet die Kette einer **Kind**-Sitzung oder eines Job-Workers ab.
    ///
    /// # Beschreibung
    /// Drei Unterschiede zur Elternkette, jeder davon einschränkend:
    /// - Die **Config-Politik wird unverändert weitergereicht** (derselbe
    ///   `Arc`). Vorher montierte ein Kind seine Registry aus den
    ///   Voreinstellungen und sah `[policy].require_approval_for` nie — der
    ///   Fan-out durfte, was dem Elternteil unter Vorbehalt stand (F-018).
    /// - Der Freigabemodus liegt in einer **eigenen** Zelle
    ///   ([`ApprovalModeCell::detached`]): ein `set` im Kind erreicht die
    ///   Wurzel nicht und umgekehrt (G-009). [`ApprovalMode::FullAccess`]
    ///   wird dabei **nie** vererbt, sondern auf [`ApprovalMode::Delegated`]
    ///   gesenkt — „ich vertraue diesem Turn" ist eine Aussage über den Turn,
    ///   den eine Person vor sich sieht, nicht über beliebig viele Kinder,
    ///   die sie nie zu Gesicht bekommt. [`ApprovalMode::AlwaysAsk`] bleibt
    ///   erhalten: eine Absenkung auf `Delegated` wäre eine Lockerung.
    /// - Es gibt **keinen Responder**. Ein Kind und ein Job-Worker fragen
    ///   niemanden; sie haben keine Oberfläche, an der eine Antwort ankäme.
    ///
    /// # Wie `AskUser` im Kind endet
    /// Die Kette entscheidet das nicht. Ein [`ApprovalDecision::AskUser`]
    /// ohne Responder wird von der
    /// [`crate::spec::AskResolution`] des **Einstiegs** aufgelöst, unter dem
    /// das Kind läuft: `RejectTurn` bricht den Turn ab, `BlockJob` hält den
    /// Job an, `Fail` lässt den Lauf mit einem Fehler enden. In keinem Fall
    /// wird eine offene Rückfrage stillschweigend zu `Allow`.
    ///
    /// [`ApprovalDecision::AskUser`]: harw_extension_api::ApprovalDecision::AskUser
    ///
    /// # Rückgabe
    /// Die Kind-Kette; die Elternkette bleibt unberührt.
    #[must_use]
    pub fn for_child(&self) -> Self {
        let child_mode = self.mode.detached();
        if child_mode.get() == ApprovalMode::FullAccess {
            child_mode.set(ApprovalMode::Delegated);
        }

        Self {
            config: self.config.clone(),
            config_tools: self.config_tools.clone(),
            default: Arc::new(DefaultApprovalPolicy::new(child_mode.clone())),
            responder: None,
            mode: child_mode,
        }
    }

    /// Die Handler dieser Kette in Auswertungsreihenfolge.
    ///
    /// # Rückgabe
    /// Config (falls vorhanden), dann Default, dann Responder (falls
    /// vorhanden). Es werden nur `Arc`-Zeiger geklont.
    #[must_use]
    pub fn handlers(&self) -> Vec<Arc<dyn ApprovalHandler>> {
        let mut handlers: Vec<Arc<dyn ApprovalHandler>> = Vec::with_capacity(3);
        if let Some(config) = &self.config {
            handlers.push(Arc::clone(config) as Arc<dyn ApprovalHandler>);
        }
        handlers.push(Arc::clone(&self.default) as Arc<dyn ApprovalHandler>);
        if let Some(responder) = &self.responder {
            handlers.push(Arc::clone(responder));
        }
        handlers
    }

    /// Registriert die Handler dieser Kette am `builder`.
    ///
    /// # Arguments
    /// - `builder` ([`ExtensionRegistryBuilder`]): der Bauer; Eigentum geht
    ///   über, weil `ExtensionRegistryBuilder::approval_handler` `self`
    ///   verbraucht.
    ///
    /// # Rückgabe
    /// Denselben Bauer, um die Handler aus [`Self::handlers`] in ebendieser
    /// Reihenfolge erweitert. Bereits vorhandene Handler bleiben unberührt
    /// und stehen weiterhin vorne.
    #[must_use]
    pub fn install(&self, builder: ExtensionRegistryBuilder) -> ExtensionRegistryBuilder {
        let mut builder = builder;
        for handler in self.handlers() {
            builder = builder.approval_handler(handler);
        }
        builder
    }

    /// Die Kette als `(label, kind)`-Paare für
    /// [`crate::spec::RightsSnapshot::approval_chain`].
    ///
    /// # Beschreibung
    /// Für die beiden eingebauten Politiken vergibt die Kette die Namen
    /// selbst ([`CONFIG_POLICY_LABEL`], [`DEFAULT_POLICY_LABEL`]) — beide
    /// überschreiben `ApprovalHandler::label`/`kind` nicht und meldeten sonst
    /// `"unnamed"`/[`ApprovalHandlerKind::Other`]. Der Responder wird
    /// dagegen gefragt: nur er selbst weiß, ob er ein TUI-Prompt
    /// ([`ApprovalHandlerKind::Interactive`]), ein Kanal
    /// ([`ApprovalHandlerKind::Channel`]) oder etwas anderes ist.
    ///
    /// # Rückgabe
    /// Dieselbe Reihenfolge wie [`Self::handlers`].
    #[must_use]
    pub fn snapshot(&self) -> Vec<(&'static str, ApprovalHandlerKind)> {
        let mut entries: Vec<(&'static str, ApprovalHandlerKind)> = Vec::with_capacity(3);
        if self.config.is_some() {
            entries.push((CONFIG_POLICY_LABEL, ApprovalHandlerKind::ConfigPolicy));
        }
        entries.push((DEFAULT_POLICY_LABEL, ApprovalHandlerKind::DefaultPolicy));
        if let Some(responder) = &self.responder {
            entries.push((responder.label(), responder.kind()));
        }
        entries
    }

    /// Die Modus-Zelle dieser Kette.
    ///
    /// # Rückgabe
    /// Eine Referenz; ein Klon davon ist der Schalter, mit dem sich der Modus
    /// dieses Laufs umstellen lässt — und **nur** dieses Laufs, denn
    /// [`Self::for_child`] löst die Zelle.
    #[must_use]
    pub fn mode(&self) -> &ApprovalModeCell {
        &self.mode
    }

    /// Die Werkzeuge aus `[policy].require_approval_for`.
    ///
    /// # Rückgabe
    /// Sortiert und dublettenfrei; leer, wenn keine Config-Politik in der
    /// Kette steht.
    #[must_use]
    pub fn config_policy_tools(&self) -> Vec<String> {
        self.config_tools.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use harw_config::ResolvedConfig;
    use harw_extension_api::{ApprovalDecision, ExtFuture, ToolCall, ToolName};
    use std::task::{Context, Poll, Waker};

    /// Liest ein [`ExtFuture`] synchron aus.
    ///
    /// Dieses Crate zieht `tokio` nur mit dem Feature `rt` (kein `macros`),
    /// `#[tokio::test]` steht also nicht zur Verfügung. Die Zukunft eines
    /// `review` hat ohnehin keinen `.await`-Punkt und ist beim ersten `poll`
    /// fertig; `Waker::noop` genügt darum — und das Crate verbietet `unsafe`,
    /// ein handgebauter `RawWaker` wäre hier gar nicht erlaubt.
    fn block_on<T>(mut future: ExtFuture<'_, T>) -> T {
        let mut cx = Context::from_waker(Waker::noop());
        match future.as_mut().poll(&mut cx) {
            Poll::Ready(value) => value,
            Poll::Pending => panic!("review() wurde beim ersten poll nicht fertig"),
        }
    }

    fn call(name: &str) -> ToolCall {
        ToolCall {
            id: Default::default(),
            name: ToolName::new(name),
            arguments: Default::default(),
        }
    }

    /// Ein Responder-Doppel: entscheidet nichts, meldet aber Art und Namen,
    /// damit [`ApprovalChain::snapshot`] geprüft werden kann.
    #[derive(Debug)]
    struct TestResponder;

    impl ApprovalHandler for TestResponder {
        fn review<'a>(&'a self, _call: &'a ToolCall) -> ExtFuture<'a, ApprovalDecision> {
            Box::pin(async { ApprovalDecision::Allow })
        }

        fn kind(&self) -> ApprovalHandlerKind {
            ApprovalHandlerKind::Interactive
        }

        fn label(&self) -> &'static str {
            "test-responder"
        }
    }

    /// Baut eine Konfiguration mit `require_approval_for = tools` und
    /// **ausgeschaltetem** `[tools.plan]` — die Lage aus G-010/F-154.
    fn config_with(tools: &[&str]) -> ResolvedConfig {
        let mut config = ResolvedConfig::default();
        assert!(
            !config.harness.tools.plan.enabled,
            "der Test lebt davon, dass [tools.plan] aus ist"
        );
        config.harness.policy.require_approval_for =
            tools.iter().map(|t| (*t).to_owned()).collect();
        config
    }

    fn root_chain(tools: &[&str], mode: ApprovalMode) -> ApprovalChain {
        ApprovalChain::for_root(&config_with(tools), ApprovalModeCell::new(mode), None)
    }

    /// G-010/F-154: Die Pflichtliste wirkt ohne jeden anderen Schalter.
    #[test]
    fn config_policy_is_built_without_the_plan_flag() {
        let chain = root_chain(&["fs.write"], ApprovalMode::Delegated);

        assert_eq!(chain.config_policy_tools(), vec!["fs.write".to_owned()]);
        assert_eq!(
            chain.snapshot(),
            vec![
                (CONFIG_POLICY_LABEL, ApprovalHandlerKind::ConfigPolicy),
                (DEFAULT_POLICY_LABEL, ApprovalHandlerKind::DefaultPolicy),
            ]
        );
        assert_eq!(chain.handlers().len(), 2);
    }

    /// Die Config-Politik greift auch dann, wenn die Standardpolitik das
    /// Werkzeug längst durchwinken würde — sonst wäre die Liste zahnlos.
    #[test]
    fn config_policy_restricts_a_tool_the_default_policy_would_allow() {
        let chain = root_chain(&["fs.read"], ApprovalMode::FullAccess);
        let handlers = chain.handlers();

        assert!(matches!(
            block_on(handlers[0].review(&call("fs.read"))),
            ApprovalDecision::AskUser(_)
        ));
        assert!(matches!(
            block_on(handlers[1].review(&call("fs.read"))),
            ApprovalDecision::Allow
        ));
    }

    /// Eine leere Liste erzeugt keinen Handler — kein Rauschen im Snapshot.
    #[test]
    fn an_empty_policy_list_adds_no_config_handler() {
        let chain = root_chain(&[], ApprovalMode::Delegated);

        assert!(chain.config_policy_tools().is_empty());
        assert_eq!(chain.handlers().len(), 1);
        assert_eq!(
            chain.snapshot(),
            vec![(DEFAULT_POLICY_LABEL, ApprovalHandlerKind::DefaultPolicy)]
        );
    }

    #[test]
    fn config_policy_tools_are_sorted_and_deduplicated() {
        let chain = root_chain(&["shell.exec", "fs.write", "shell.exec"], ApprovalMode::Delegated);

        assert_eq!(
            chain.config_policy_tools(),
            vec!["fs.write".to_owned(), "shell.exec".to_owned()]
        );
    }

    #[test]
    fn snapshot_reports_the_responder_kind_it_declares() {
        let chain = ApprovalChain::for_root(
            &config_with(&["fs.write"]),
            ApprovalModeCell::new(ApprovalMode::Delegated),
            Some(Arc::new(TestResponder)),
        );

        assert_eq!(
            chain.snapshot(),
            vec![
                (CONFIG_POLICY_LABEL, ApprovalHandlerKind::ConfigPolicy),
                (DEFAULT_POLICY_LABEL, ApprovalHandlerKind::DefaultPolicy),
                ("test-responder", ApprovalHandlerKind::Interactive),
            ]
        );
        assert_eq!(chain.handlers().len(), 3);
    }

    /// G-009: `FullAccess` der Wurzel erreicht das Kind nie.
    #[test]
    fn a_child_never_inherits_full_access() {
        let root = ApprovalChain::for_root(
            &config_with(&["fs.write"]),
            ApprovalModeCell::new(ApprovalMode::FullAccess),
            Some(Arc::new(TestResponder)),
        );
        let child = root.for_child();

        assert_eq!(child.mode().get(), ApprovalMode::Delegated);
        assert_eq!(root.mode().get(), ApprovalMode::FullAccess);

        // Die Kind-Politik muss die gesenkte Zelle wirklich lesen.
        let handlers = child.handlers();
        assert!(matches!(
            block_on(handlers[1].review(&call("shell.exec"))),
            ApprovalDecision::AskUser(_)
        ));
    }

    /// Der strengere Modus bleibt erhalten — eine Absenkung auf `Delegated`
    /// wäre eine Lockerung.
    #[test]
    fn a_child_keeps_the_stricter_always_ask_mode() {
        let child = root_chain(&[], ApprovalMode::AlwaysAsk).for_child();

        assert_eq!(child.mode().get(), ApprovalMode::AlwaysAsk);
        assert!(matches!(
            block_on(child.handlers()[0].review(&call("fs.read"))),
            ApprovalDecision::AskUser(_)
        ));
    }

    /// Die Zellen sind entkoppelt: ein `set` läuft in keine Richtung über.
    #[test]
    fn child_and_root_mode_cells_are_independent() {
        let root = root_chain(&[], ApprovalMode::Delegated);
        let child = root.for_child();

        root.mode().set(ApprovalMode::FullAccess);
        assert_eq!(child.mode().get(), ApprovalMode::Delegated);

        child.mode().set(ApprovalMode::AlwaysAsk);
        assert_eq!(root.mode().get(), ApprovalMode::FullAccess);
    }

    /// F-018: Die Config-Politik der Wurzel gilt auch im Kind.
    #[test]
    fn a_child_inherits_the_config_policy() {
        let root = root_chain(&["fs.write"], ApprovalMode::Delegated);
        let child = root.for_child();

        assert_eq!(child.config_policy_tools(), vec!["fs.write".to_owned()]);
        assert!(matches!(
            block_on(child.handlers()[0].review(&call("fs.write"))),
            ApprovalDecision::AskUser(_)
        ));
    }

    /// Kinder fragen niemanden — der Responder wird nicht weitergereicht.
    #[test]
    fn a_child_has_no_responder() {
        let root = ApprovalChain::for_root(
            &config_with(&[]),
            ApprovalModeCell::new(ApprovalMode::Delegated),
            Some(Arc::new(TestResponder)),
        );
        let child = root.for_child();

        assert_eq!(
            child.snapshot(),
            vec![(DEFAULT_POLICY_LABEL, ApprovalHandlerKind::DefaultPolicy)]
        );
        assert_eq!(child.handlers().len(), 1);
    }

    /// `install` registriert in genau der Reihenfolge aus [`ApprovalChain::handlers`].
    /// Prüfbar ist das über die Registry-API: die eingebauten Politiken melden
    /// keinen eigenen `label`, wohl aber unterscheidbares Verhalten.
    #[test]
    fn install_registers_the_handlers_in_order() {
        let chain = ApprovalChain::for_root(
            // `fs.read` ist in `AUTO_APPROVED_TOOLS`; nur die Config-Politik
            // stellt es unter Vorbehalt.
            &config_with(&["fs.read"]),
            ApprovalModeCell::new(ApprovalMode::Delegated),
            Some(Arc::new(TestResponder)),
        );

        let registry = chain.install(ExtensionRegistryBuilder::default()).build();
        let handlers = registry.approval_handlers();

        assert_eq!(handlers.len(), 3);
        assert!(
            matches!(
                block_on(handlers[0].review(&call("fs.read"))),
                ApprovalDecision::AskUser(_)
            ),
            "Platz 0 gehört der Config-Politik"
        );
        assert!(
            matches!(
                block_on(handlers[1].review(&call("fs.read"))),
                ApprovalDecision::Allow
            ),
            "Platz 1 gehört der Standardpolitik"
        );
        assert_eq!(
            handlers[2].kind(),
            ApprovalHandlerKind::Interactive,
            "Platz 2 gehört dem Responder"
        );
    }

    /// `install` hängt an, statt zu ersetzen.
    #[test]
    fn install_keeps_handlers_that_were_already_registered() {
        let builder = ExtensionRegistryBuilder::default()
            .approval_handler(Arc::new(TestResponder) as Arc<dyn ApprovalHandler>);
        let chain = root_chain(&[], ApprovalMode::Delegated);

        let registry = chain.install(builder).build();
        let handlers = registry.approval_handlers();

        assert_eq!(handlers.len(), 2);
        assert_eq!(handlers[0].kind(), ApprovalHandlerKind::Interactive);
        assert_eq!(handlers[1].label(), "unnamed", "die Standardpolitik benennt sich nicht selbst");
    }

    /// Die Zelle der Wurzel bleibt der Schalter: `set` erreicht die bereits
    /// montierte Politik sofort.
    #[test]
    fn the_root_mode_cell_stays_the_switch_for_the_installed_policy() {
        let cell = ApprovalModeCell::new(ApprovalMode::Delegated);
        let chain = ApprovalChain::for_root(&config_with(&[]), cell.clone(), None);
        let registry = chain.install(ExtensionRegistryBuilder::default()).build();
        let handler = Arc::clone(&registry.approval_handlers()[0]);

        assert!(matches!(
            block_on(handler.review(&call("shell.exec"))),
            ApprovalDecision::AskUser(_)
        ));

        cell.set(ApprovalMode::FullAccess);
        assert!(
            matches!(
                block_on(handler.review(&call("shell.exec"))),
                ApprovalDecision::Allow
            ),
            "die Umschaltung muss die montierte Registry erreichen"
        );
    }
}
