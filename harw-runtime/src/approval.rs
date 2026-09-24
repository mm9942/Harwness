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
//! Config-Politik deshalb unverändert weiter; den Freigabemodus übernimmt das
//! Kind live von der Elternkette, aber **gedeckelt** auf
//! [`ApprovalMode::Delegated`] (G-009).
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
use harw_extension_api::allow_rules::{AllowRuleSet, RuleDecision};
use harw_extension_api::approval_mode::{ApprovalMode, ApprovalModeCell};
use harw_extension_api::contributors::ApprovalHandlerKind;
use harw_extension_api::{
    ApprovalDecision, ApprovalHandler, ExtFuture, ExtensionRegistry, ExtensionRegistryBuilder,
    ToolCall,
};
use harw_registry_defaults::DefaultApprovalPolicy;

use crate::spec::AskResolution;

/// Stabiler Kurzname der Konfigurationspolitik in [`ApprovalChain::snapshot`].
///
/// `ConfigApprovalPolicy` überschreibt `ApprovalHandler::label` selbst nicht
/// (sie meldete `"unnamed"`/[`ApprovalHandlerKind::Other`]). Die Kette weiß
/// aber, welchen Handler sie an welche Stelle gesetzt hat, und benennt ihn
/// deshalb hier — ohne fremden Code zu ändern.
pub const CONFIG_POLICY_LABEL: &str = "config-policy";

/// Stabiler Kurzname der eingebauten Standardpolitik.
pub const DEFAULT_POLICY_LABEL: &str = "default-policy";

/// Löst `AskUser` deterministisch auf, wo niemand antworten kann.
///
/// # Warum es diesen Handler gibt
/// [`crate::spec::EntryProfile::ask`] deklarierte bis W2c nur, *wie* eine
/// Rückfrage ohne anwesende Person enden soll — durchgesetzt hat es niemand
/// (Befund Z2c-02). Ein Einstieg mit [`AskResolution::Fail`] montierte
/// dieselbe Kette wie die TUI: ein `AskUser` blieb einfach stehen, und wer
/// dem Lauf einen Responder mitgab, bekam eine Rückfragefläche, die die
/// Vertragstabelle ihm verweigert.
///
/// # Entscheidung
/// Der Handler beantwortet **nur** die Aufrufe mit [`ApprovalDecision::Deny`],
/// bei denen die übrige Kette dieses Laufs `AskUser` liefern würde; alle
/// anderen lässt er mit [`ApprovalDecision::Allow`] unberührt. Das ist
/// notwendig: `check_approval` aggregiert `Deny` > `AskUser` > `Allow` über
/// **alle** Handler (`harw-core/src/turn_loop.rs:490-509`), ein pauschales
/// `Deny` würde also jeden einzelnen Werkzeugaufruf ablehnen, nicht nur die
/// rückfragepflichtigen.
///
/// Die Vorhersage ist keine zweite Politik, sondern dieselbe Rechnung aus
/// denselben Quellen:
/// - [`ConfigApprovalPolicy::requires_approval`] (`harw-core/src/policy.rs:34`)
///   auf **derselben** `Arc`-Instanz, die auch in der Kette steht;
/// - [`DefaultApprovalPolicy::requires_explicit_approval`]
///   (`harw-registry-defaults/src/lib.rs:208`) unter dem Modus **derselben**
///   [`ApprovalModeCell`] (`harw-registry-defaults/src/lib.rs:213-234`).
///
/// Beide sind `pub` und reine Prädikate; es wird kein fremder `review`
/// aufgerufen, also auch keine zweite `ItemId` vergeben — die Invariante
/// „ein Handler wird je Aufruf höchstens einmal befragt" bleibt unberührt.
///
/// # Abgrenzung zu W4a
/// Der Unterschied zwischen [`AskResolution::RejectTurn`],
/// [`AskResolution::BlockJob`] und [`AskResolution::Fail`] ist eine Aussage
/// über den **Turn-Loop** (Turn abbrechen, Job anhalten, Lauf beenden) und
/// gehört dorthin (Welle W4a). Hier unterscheidet sie nur die Begründung und
/// das Label; die Wirkung auf den einzelnen Aufruf ist in allen drei Fällen
/// dieselbe und fail-closed: nicht ausführen.
pub struct AskResolutionPolicy {
    /// Die Auflösung des Einstiegs; bestimmt Label und Begründung.
    resolution: AskResolution,
    /// Dieselbe Zelle, aus der die [`DefaultApprovalPolicy`] dieses Laufs liest.
    mode: ApprovalModeCell,
    /// Dieselbe Config-Politik, die in der Kette steht; `None` bei leerer Liste.
    config: Option<Arc<ConfigApprovalPolicy>>,
    /// Dieselbe [`AllowRuleSet`], die auch die [`DefaultApprovalPolicy`] dieser
    /// Kette befragt (Contract §2/§4) — ohne sie würde die Vorhersage eine
    /// Allow-Regel als Rückfrage missdeuten und eine Deny-Regel übersehen.
    rules: AllowRuleSet,
}

impl std::fmt::Debug for AskResolutionPolicy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AskResolutionPolicy")
            .field("resolution", &self.resolution)
            .field("mode", &self.mode.get())
            .field("config", &self.config.is_some())
            .finish()
    }
}

impl AskResolutionPolicy {
    /// Baut die Politik für einen Einstieg — oder keine.
    ///
    /// # Rückgabe
    /// `None` für [`AskResolution::Interactive`]: dort gibt es eine anwesende
    /// Person, und genau deren Responder beantwortet die Rückfrage.
    #[must_use]
    fn new(
        resolution: AskResolution,
        mode: ApprovalModeCell,
        config: Option<Arc<ConfigApprovalPolicy>>,
        rules: AllowRuleSet,
    ) -> Option<Self> {
        match resolution {
            AskResolution::Interactive => None,
            AskResolution::RejectTurn | AskResolution::BlockJob | AskResolution::Fail => {
                Some(Self {
                    resolution,
                    mode,
                    config,
                    rules,
                })
            }
        }
    }

    /// Die Auflösung, die dieser Handler durchsetzt.
    #[must_use]
    pub const fn resolution(&self) -> AskResolution {
        self.resolution
    }

    /// Der stabile Kurzname einer Auflösung: `ask:<auflösung>`.
    ///
    /// # Rückgabe
    /// `"ask:interactive"`, `"ask:reject-turn"`, `"ask:block-job"` oder
    /// `"ask:fail"`. Der Wert steht in
    /// [`crate::spec::RightsSnapshot::approval_chain`] und ist damit Teil der
    /// Diagnosefläche — er darf sich nicht mehr ändern.
    #[must_use]
    pub const fn label_for(resolution: AskResolution) -> &'static str {
        match resolution {
            AskResolution::Interactive => "ask:interactive",
            AskResolution::RejectTurn => "ask:reject-turn",
            AskResolution::BlockJob => "ask:block-job",
            AskResolution::Fail => "ask:fail",
        }
    }

    /// Ob die übrige Kette dieses Laufs für `call` `AskUser` liefern würde.
    ///
    /// # Beschreibung
    /// Rechnet seit Contract §2/§4 dieselbe Reihenfolge nach, die
    /// [`DefaultApprovalPolicy::review`] tatsächlich anwendet: eine passende
    /// `Deny`-Regel würde dort **immer** `AskUser` liefern (fail-closed), eine
    /// passende `Allow`-Regel **immer** `Allow` — beides unabhängig vom Modus.
    /// Ohne diesen Abgleich würde die Vorhersage eine Allow-Regel als
    /// Rückfrage missdeuten (und einen an sich freigegebenen Aufruf hier
    /// fälschlich ablehnen) oder eine Deny-Regel übersehen.
    fn would_ask(&self, call: &ToolCall) -> bool {
        if self
            .config
            .as_ref()
            .is_some_and(|policy| policy.requires_approval(call))
        {
            return true;
        }
        match self.rules.evaluate(call.name.as_str(), &call.arguments) {
            Some(RuleDecision::Deny) => return true,
            Some(RuleDecision::Allow) => return false,
            None => {}
        }
        match self.mode.get() {
            ApprovalMode::AlwaysAsk => true,
            ApprovalMode::Delegated => DefaultApprovalPolicy::requires_explicit_approval(call),
            ApprovalMode::FullAccess => false,
        }
    }

    /// Die Begründung, die im `Deny` landet und im Protokoll auftaucht.
    fn reason(&self, call: &ToolCall) -> String {
        let outcome = match self.resolution {
            // Nur zur Vollständigkeit: `new` baut für `Interactive` nichts.
            AskResolution::Interactive => "an interactive responder must answer it",
            AskResolution::RejectTurn => "this entry rejects the turn instead (W4a)",
            AskResolution::BlockJob => {
                "this entry blocks the job until a durable approval exists (W4a)"
            }
            AskResolution::Fail => "this entry fails the run instead (W4a)",
        };
        format!(
            "'{}' needs an approval, but no one can answer it here: {outcome}",
            call.name.as_str()
        )
    }
}

impl ApprovalHandler for AskResolutionPolicy {
    fn review<'a>(&'a self, call: &'a ToolCall) -> ExtFuture<'a, ApprovalDecision> {
        let denial = if self.would_ask(call) {
            Some(self.reason(call))
        } else {
            None
        };
        Box::pin(async move { denial.map_or(ApprovalDecision::Allow, ApprovalDecision::Deny) })
    }

    /// [`ApprovalHandlerKind::Other`]: keine der deklarierten Kategorien passt
    /// — dieser Handler ist weder Konfigurationspolitik noch die eingebaute
    /// Standardpolitik, und er fragt niemanden.
    fn kind(&self) -> ApprovalHandlerKind {
        ApprovalHandlerKind::Other
    }

    fn label(&self) -> &'static str {
        Self::label_for(self.resolution)
    }
}

/// Die Freigabekette eines montierten Laufs, in Auswertungsreihenfolge.
///
/// # Beschreibung
/// Die Reihenfolge ist Config → Default → Ask-Auflösung → Responder:
/// 1. **Config** ([`ConfigApprovalPolicy`]) aus `[policy].require_approval_for`
///    — vorhanden genau dann, wenn die Liste nicht leer ist.
/// 2. **Default** ([`DefaultApprovalPolicy`]) über der [`ApprovalModeCell`]
///    dieses Laufs — die fail-closed Grundlinie.
/// 3. **Ask-Auflösung** ([`AskResolutionPolicy`]) — vorhanden für jeden
///    Einstieg, dessen [`AskResolution`] **nicht** `Interactive` ist.
/// 4. **Responder** — der interaktive Handler des Einstiegs; nur
///    [`AskResolution::Interactive`] darf einen mitbringen
///    (`RuntimeAssembly::new_root_session` weist jeden anderen ab).
///
/// Punkt 3 und 4 schließen einander aus: entweder jemand kann antworten, oder
/// die Rückfrage wird deterministisch aufgelöst.
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
    /// Die Ask-Auflösung des Einstiegs; `None` genau bei
    /// [`AskResolution::Interactive`].
    ask: Option<Arc<AskResolutionPolicy>>,
    /// Wer gefragt wird, wenn ein Handler [`harw_extension_api::ApprovalDecision::AskUser`]
    /// liefert; `None` in jedem Lauf, in dem niemand anwesend ist.
    responder: Option<Arc<dyn ApprovalHandler>>,
    /// Der Freigabemodus dieses Laufs. Geteilt mit [`Self::default`], damit
    /// eine Umschaltung sofort wirkt.
    mode: ApprovalModeCell,
    /// Die geteilten Freigaberegeln dieses Laufs (Contract §2/§4). Geteilt
    /// mit [`Self::default`] (der [`DefaultApprovalPolicy`], die sie befragt)
    /// und mit [`Self::ask`] (dessen Vorhersage dieselbe Regelmenge braucht).
    rules: AllowRuleSet,
}

impl std::fmt::Debug for ApprovalChain {
    /// Zeigt die Gestalt der Kette, nicht die Innereien der Handler
    /// (`Arc<dyn ApprovalHandler>` ist nicht `Debug`, und `Debug` als
    /// Supertrait zu verlangen würde jeden Implementierer binden).
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ApprovalChain")
            .field("config_tools", &self.config_tools)
            .field("ask", &self.ask.as_ref().map(|a| a.label()))
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
    /// - `ask` ([`AskResolution`]): die Auflösung aus
    ///   [`crate::spec::EntryKind::profile`]. Alles außer
    ///   [`AskResolution::Interactive`] hängt eine [`AskResolutionPolicy`] an
    ///   — ohne sie bliebe die Deklaration wirkungslos (Z2c-02).
    /// - `mode` ([`ApprovalModeCell`]): die Modus-Zelle dieses Laufs.
    ///   Eigentum geht über; ein Klon beim Aufrufer bleibt der Schalter
    ///   (`/permissions set`), mit dem sich der Modus umstellen lässt.
    /// - `responder` (`Option<Arc<dyn ApprovalHandler>>`): der interaktive
    ///   Handler des Einstiegs, oder `None`.
    /// - `rules` ([`AllowRuleSet`]): die geteilten Freigaberegeln dieses
    ///   Laufs (Contract §2/§4, Plan Schritt 4), etwa aus globaler und
    ///   Projekt-Konfiguration gesät. Eigentum geht über; ein Klon beim
    ///   Aufrufer bleibt der Schalter, mit dem `/permissions` neue Regeln
    ///   anlegt.
    ///
    /// # Rückgabe
    /// Die Kette in der Reihenfolge Config → Default → Ask-Auflösung →
    /// Responder.
    #[must_use]
    pub fn for_root(
        config: &ResolvedConfig,
        ask: AskResolution,
        mode: ApprovalModeCell,
        responder: Option<Arc<dyn ApprovalHandler>>,
        rules: AllowRuleSet,
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
            ask: AskResolutionPolicy::new(ask, mode.clone(), config_policy.clone(), rules.clone())
                .map(Arc::new),
            config: config_policy,
            config_tools,
            default: Arc::new(DefaultApprovalPolicy::with_rules(
                mode.clone(),
                rules.clone(),
            )),
            responder,
            mode,
            rules,
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
    /// - Der Freigabemodus liegt in einer **Folgezelle**
    ///   ([`ApprovalModeCell::follower`]) mit Obergrenze
    ///   [`ApprovalMode::Delegated`]: eine Umstellung der Elternzelle erreicht
    ///   laufende Kinder sofort, ein `set` im Kind koppelt nur dieses Kind ab
    ///   und erreicht weder die Wurzel noch Geschwister. [`ApprovalMode::FullAccess`]
    ///   wird dabei **nie** vererbt, sondern durch die Deckelung auf
    ///   [`ApprovalMode::Delegated`] gesenkt (G-009) — „ich vertraue diesem
    ///   Turn" ist eine Aussage über den Turn, den eine Person vor sich sieht,
    ///   nicht über beliebig viele Kinder, die sie nie zu Gesicht bekommt.
    ///   [`ApprovalMode::AlwaysAsk`] bleibt erhalten: die Deckelung lockert
    ///   nie.
    /// - Es gibt **keinen Responder**. Ein Kind und ein Job-Worker fragen
    ///   niemanden; sie haben keine Oberfläche, an der eine Antwort ankäme.
    /// - Die **Freigaberegeln werden unverändert weitergereicht** (dieselbe
    ///   [`AllowRuleSet`], kein `detached()`) — nach derselben Begründung wie
    ///   die Config-Politik: eine Regel kann einem Kind nie **mehr** erlauben,
    ///   als seine eigene Werkzeugfläche (Registry-Profil, Sandbox-Rechte)
    ///   ohnehin zulässt (`a_child_never_inherits_full_access` gilt sinngemäß
    ///   auch hier — nur die *Rückfrage* für einen bereits erlaubten Aufruf
    ///   entfällt, keine neue Fähigkeit entsteht). Eine `Deny`-Regel wirkt im
    ///   Kind genauso einschränkend wie in der Wurzel.
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
        let child_mode = self.mode.follower(ApprovalMode::Delegated);

        Self {
            // Die Ask-Auflösung ist die des Einstiegs und gilt für das Kind
            // unverändert; nur die Zelle ist die Folgezelle des Kindes, damit die
            // Vorhersage denselben Modus liest wie die Kind-Standardpolitik.
            ask: self
                .ask
                .as_ref()
                .and_then(|ask| {
                    AskResolutionPolicy::new(
                        ask.resolution(),
                        child_mode.clone(),
                        self.config.clone(),
                        self.rules.clone(),
                    )
                })
                .map(Arc::new),
            config: self.config.clone(),
            config_tools: self.config_tools.clone(),
            default: Arc::new(DefaultApprovalPolicy::with_rules(
                child_mode.clone(),
                self.rules.clone(),
            )),
            responder: None,
            mode: child_mode,
            rules: self.rules.clone(),
        }
    }

    /// Die geteilten Freigaberegeln dieser Kette.
    ///
    /// # Rückgabe
    /// Ein Klon der [`AllowRuleSet`]; da sie ihren Zustand über einen
    /// `Arc<RwLock<_>>` teilt, wirkt eine über diesen Klon hinzugefügte Regel
    /// sofort auf die bereits montierte [`DefaultApprovalPolicy`] dieser
    /// Kette — genau wie [`Self::mode`] für den Freigabemodus.
    #[must_use]
    pub fn rules(&self) -> &AllowRuleSet {
        &self.rules
    }

    /// Die Handler dieser Kette in Auswertungsreihenfolge.
    ///
    /// # Rückgabe
    /// Config (falls vorhanden), dann Default, dann Ask-Auflösung (falls
    /// vorhanden), dann Responder (falls vorhanden). Es werden nur
    /// `Arc`-Zeiger geklont.
    #[must_use]
    pub fn handlers(&self) -> Vec<Arc<dyn ApprovalHandler>> {
        self.handlers_with_default(true)
    }

    /// [`Self::handlers`], wahlweise **ohne** die eigene Standardpolitik.
    ///
    /// # Arguments
    /// - `with_default` (`bool`): `false` lässt [`DefaultApprovalPolicy`] aus,
    ///   weil die Zielregistry sie bereits trägt (siehe [`Self::install`]).
    fn handlers_with_default(&self, with_default: bool) -> Vec<Arc<dyn ApprovalHandler>> {
        let mut handlers: Vec<Arc<dyn ApprovalHandler>> = Vec::with_capacity(4);
        if let Some(config) = &self.config {
            handlers.push(Arc::clone(config) as Arc<dyn ApprovalHandler>);
        }
        if with_default {
            handlers.push(Arc::clone(&self.default) as Arc<dyn ApprovalHandler>);
        }
        if let Some(ask) = &self.ask {
            handlers.push(Arc::clone(ask) as Arc<dyn ApprovalHandler>);
        }
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
    ///
    /// Für eine Registry aus `assemble_registry_for_project` ist
    /// [`Self::install_over_default`] die richtige Form: sie trägt die
    /// Standardpolitik schon.
    #[must_use]
    pub fn install(&self, builder: ExtensionRegistryBuilder) -> ExtensionRegistryBuilder {
        let mut builder = builder;
        for handler in self.handlers() {
            builder = builder.approval_handler(handler);
        }
        builder
    }

    /// Wie [`Self::install`], für eine Registry, die bereits ihre regellose
    /// Standardpolitik trägt.
    ///
    /// # Warum es diese zweite Form gibt
    /// `assemble_registry_for_project` registriert **genau einen**
    /// `ApprovalHandler`, eine regellose [`DefaultApprovalPolicy`] über der
    /// [`ApprovalModeCell`], die ihr der Aufrufer übergeben hat
    /// (`harw-registry-defaults/src/profile.rs:921-922`; der dortige Test hält
    /// `approval_handlers().len() == 1` fest, `:1196-1197`). Hängte die Kette
    /// ihre eigene daneben, stünden zwei davon in jeder Wurzel- und
    /// Kindregistry. Das ist zudem nicht wirkungsgleich: die mitgebrachte
    /// Politik kennt keine Regeln und kann daher eine konfigurierte
    /// `AllowRuleSet` noch zu `AskUser` eskalieren. Bei genau diesem bekannten
    /// Montagepunkt wird sie deshalb ersetzt.
    ///
    /// # Warum die Provenienz ein Parameter ist und keine Erkennung
    /// Ablesen lässt sich die Art **nicht**: weder [`DefaultApprovalPolicy`]
    /// noch [`ConfigApprovalPolicy`] überschreiben `ApprovalHandler::kind`,
    /// beide melden die Vorgabe [`ApprovalHandlerKind::Other`]
    /// (`harw-extension-api/src/contributors.rs:321-330`); ein
    /// `kind() == DefaultPolicy` gibt es dort schlicht nicht zu finden. Die
    /// Aussage „diese Registry kommt von `assemble_registry_for_project`"
    /// steht deshalb an der Aufrufstelle, wo sie sichtbar belegt ist
    /// (`assembly.rs`, `children.rs`).
    ///
    /// Geprüft wird sie trotzdem: trägt `assembled` **nicht** genau einen
    /// Handler, stammt sie nicht von dort, und diese Kette installiert ihre
    /// eigene Standardpolitik doch — die fail-closed Grundlinie darf nie
    /// fehlen.
    ///
    /// # Arguments
    /// - `assembled` ([`ExtensionRegistry`]): die montierte Registry;
    ///   Eigentum geht über, weil `into_builder` sie verbraucht.
    ///
    /// # Rückgabe
    /// Den Bauer der Registry, erweitert um die Handler aus
    /// [`Self::handlers`]. Die Reihenfolge ist
    /// Dokumentation, keine Priorität: `check_approval` befragt jeden Handler
    /// und aggregiert `Deny` > `AskUser` > `Allow`.
    #[must_use]
    pub fn install_over_default(&self, assembled: ExtensionRegistry) -> ExtensionRegistryBuilder {
        let carries_default = assembled.approval_handlers().len() == 1;
        if !carries_default {
            tracing::warn!(
                handlers = assembled.approval_handlers().len(),
                "runtime.approval_chain.unexpected_registry_shape"
            );
        }
        let mut builder = assembled.into_builder();
        if carries_default {
            builder = builder.clear_approval_handlers();
        }
        for handler in self.handlers_with_default(true) {
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
    /// Dieselbe Reihenfolge wie [`Self::handlers`]. Die Standardpolitik
    /// erscheint **genau einmal**, auch wenn [`Self::install`] ihre eigene
    /// weglässt: die Zielregistry trägt dann die von
    /// `assemble_registry_for_project`, und das ist dieselbe Politik über
    /// derselben Zelle.
    #[must_use]
    pub fn snapshot(&self) -> Vec<(&'static str, ApprovalHandlerKind)> {
        let mut entries: Vec<(&'static str, ApprovalHandlerKind)> = Vec::with_capacity(4);
        if self.config.is_some() {
            entries.push((CONFIG_POLICY_LABEL, ApprovalHandlerKind::ConfigPolicy));
        }
        entries.push((DEFAULT_POLICY_LABEL, ApprovalHandlerKind::DefaultPolicy));
        if let Some(ask) = &self.ask {
            entries.push((ask.label(), ask.kind()));
        }
        if let Some(responder) = &self.responder {
            entries.push((responder.label(), responder.kind()));
        }
        entries
    }

    /// Die Ask-Auflösung, die diese Kette durchsetzt.
    ///
    /// # Rückgabe
    /// `Some(resolution)` für jeden Einstieg außer
    /// [`AskResolution::Interactive`], dessen Rückfragen ein Responder
    /// beantwortet.
    #[must_use]
    pub fn ask_resolution(&self) -> Option<AskResolution> {
        self.ask.as_ref().map(|ask| ask.resolution())
    }

    /// Die Modus-Zelle dieser Kette.
    ///
    /// # Rückgabe
    /// Eine Referenz; ein Klon davon ist der Schalter, mit dem sich der Modus
    /// dieses Laufs umstellen lässt. Kinder aus [`Self::for_child`] folgen
    /// der Umstellung live, gedeckelt auf [`ApprovalMode::Delegated`]; ein
    /// `set` auf der Zelle eines Kindes erreicht die Elternkette nie.
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
    use crate::test_support::{TestError, TestResult};
    use harw_config::ResolvedConfig;
    use harw_extension_api::allow_rules::{ApprovalRule, RuleScope};
    use harw_extension_api::{ApprovalDecision, ExtFuture, ToolCall, ToolName};
    use std::task::{Context, Poll, Waker};

    /// Liest ein [`ExtFuture`] synchron aus.
    ///
    /// Dieses Crate zieht `tokio` nur mit dem Feature `rt` (kein `macros`),
    /// `#[tokio::test]` steht also nicht zur Verfügung. Die Zukunft eines
    /// `review` hat ohnehin keinen `.await`-Punkt und ist beim ersten `poll`
    /// fertig; `Waker::noop` genügt darum — und das Crate verbietet `unsafe`,
    /// ein handgebauter `RawWaker` wäre hier gar nicht erlaubt.
    fn block_on<T>(mut future: ExtFuture<'_, T>) -> TestResult<T> {
        let mut cx = Context::from_waker(Waker::noop());
        match future.as_mut().poll(&mut cx) {
            Poll::Ready(value) => Ok(value),
            Poll::Pending => Err(TestError::Unexpected(
                "review() wurde beim ersten poll nicht fertig".into(),
            )),
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
        config.harness.tools.plan.enabled = false;
        assert!(
            !config.harness.tools.plan.enabled,
            "der Test lebt von einer explizit deaktivierten Planungsfläche"
        );
        config.harness.policy.require_approval_for =
            tools.iter().map(|t| (*t).to_owned()).collect();
        config
    }

    fn root_chain(tools: &[&str], mode: ApprovalMode) -> ApprovalChain {
        ApprovalChain::for_root(
            &config_with(tools),
            AskResolution::Interactive,
            ApprovalModeCell::new(mode),
            None,
            AllowRuleSet::new(),
        )
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
    fn config_policy_restricts_a_tool_the_default_policy_would_allow() -> TestResult {
        let chain = root_chain(&["fs.read"], ApprovalMode::FullAccess);
        let handlers = chain.handlers();

        assert!(matches!(
            block_on(handlers[0].review(&call("fs.read")))?,
            ApprovalDecision::AskUser(_)
        ));
        assert!(matches!(
            block_on(handlers[1].review(&call("fs.read")))?,
            ApprovalDecision::Allow
        ));
        Ok(())
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
        let chain = root_chain(
            &["shell.exec", "fs.write", "shell.exec"],
            ApprovalMode::Delegated,
        );

        assert_eq!(
            chain.config_policy_tools(),
            vec!["fs.write".to_owned(), "shell.exec".to_owned()]
        );
    }

    #[test]
    fn snapshot_reports_the_responder_kind_it_declares() {
        let chain = ApprovalChain::for_root(
            &config_with(&["fs.write"]),
            AskResolution::Interactive,
            ApprovalModeCell::new(ApprovalMode::Delegated),
            Some(Arc::new(TestResponder)),
            AllowRuleSet::new(),
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
    fn a_child_never_inherits_full_access() -> TestResult {
        let root = ApprovalChain::for_root(
            &config_with(&["fs.write"]),
            AskResolution::Interactive,
            ApprovalModeCell::new(ApprovalMode::FullAccess),
            Some(Arc::new(TestResponder)),
            AllowRuleSet::new(),
        );
        let child = root.for_child();

        assert_eq!(child.mode().get(), ApprovalMode::Delegated);
        assert_eq!(root.mode().get(), ApprovalMode::FullAccess);

        // Die Kind-Politik muss die gesenkte Zelle wirklich lesen.
        let handlers = child.handlers();
        assert!(matches!(
            block_on(handlers[1].review(&call("shell.exec")))?,
            ApprovalDecision::AskUser(_)
        ));
        Ok(())
    }

    /// Der strengere Modus bleibt erhalten — eine Absenkung auf `Delegated`
    /// wäre eine Lockerung.
    #[test]
    fn a_child_keeps_the_stricter_always_ask_mode() -> TestResult {
        let child = root_chain(&[], ApprovalMode::AlwaysAsk).for_child();

        assert_eq!(child.mode().get(), ApprovalMode::AlwaysAsk);
        assert!(matches!(
            block_on(child.handlers()[0].review(&call("fs.read")))?,
            ApprovalDecision::AskUser(_)
        ));
        Ok(())
    }

    /// Eine Umstellung der Wurzel erreicht laufende Kinder live — gedeckelt
    /// auf `Delegated`.
    #[test]
    fn root_mode_changes_reach_children_capped_at_delegated() {
        let root = root_chain(&[], ApprovalMode::Delegated);
        let child = root.for_child();
        assert!(child.mode().is_follower());

        root.mode().set(ApprovalMode::AlwaysAsk);
        assert_eq!(child.mode().get(), ApprovalMode::AlwaysAsk);

        root.mode().set(ApprovalMode::FullAccess);
        assert_eq!(child.mode().get(), ApprovalMode::Delegated);

        let grandchild = child.for_child();
        root.mode().set(ApprovalMode::AlwaysAsk);
        assert_eq!(grandchild.mode().get(), ApprovalMode::AlwaysAsk);
    }

    /// Ein `set` im Kind läuft weder zur Wurzel noch zu Geschwistern über.
    #[test]
    fn a_child_set_reaches_neither_root_nor_siblings() {
        let root = root_chain(&[], ApprovalMode::Delegated);
        let child = root.for_child();
        let sibling = root.for_child();

        child.mode().set(ApprovalMode::AlwaysAsk);

        assert_eq!(child.mode().get(), ApprovalMode::AlwaysAsk);
        assert_eq!(sibling.mode().get(), ApprovalMode::Delegated);
        assert_eq!(root.mode().get(), ApprovalMode::Delegated);

        // Das abgekoppelte Kind folgt der Wurzel nicht mehr, das Geschwister
        // schon.
        root.mode().set(ApprovalMode::FullAccess);
        assert_eq!(child.mode().get(), ApprovalMode::AlwaysAsk);
        assert_eq!(sibling.mode().get(), ApprovalMode::Delegated);
    }

    /// Auch ein lokales `set` im Kind durchbricht die Deckelung nicht.
    #[test]
    fn a_child_cannot_raise_itself_to_full_access() {
        let root = root_chain(&[], ApprovalMode::FullAccess);
        let child = root.for_child();

        child.mode().set(ApprovalMode::FullAccess);

        assert_eq!(child.mode().get(), ApprovalMode::Delegated);
    }

    /// F-018: Die Config-Politik der Wurzel gilt auch im Kind.
    #[test]
    fn a_child_inherits_the_config_policy() -> TestResult {
        let root = root_chain(&["fs.write"], ApprovalMode::Delegated);
        let child = root.for_child();

        assert_eq!(child.config_policy_tools(), vec!["fs.write".to_owned()]);
        assert!(matches!(
            block_on(child.handlers()[0].review(&call("fs.write")))?,
            ApprovalDecision::AskUser(_)
        ));
        Ok(())
    }

    /// Kinder fragen niemanden — der Responder wird nicht weitergereicht.
    #[test]
    fn a_child_has_no_responder() {
        let root = ApprovalChain::for_root(
            &config_with(&[]),
            AskResolution::Interactive,
            ApprovalModeCell::new(ApprovalMode::Delegated),
            Some(Arc::new(TestResponder)),
            AllowRuleSet::new(),
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
    fn install_registers_the_handlers_in_order() -> TestResult {
        let chain = ApprovalChain::for_root(
            // `fs.read` ist in `AUTO_APPROVED_TOOLS`; nur die Config-Politik
            // stellt es unter Vorbehalt.
            &config_with(&["fs.read"]),
            AskResolution::Interactive,
            ApprovalModeCell::new(ApprovalMode::Delegated),
            Some(Arc::new(TestResponder)),
            AllowRuleSet::new(),
        );

        let registry = chain.install(ExtensionRegistryBuilder::default()).build();
        let handlers = registry.approval_handlers();

        assert_eq!(handlers.len(), 3);
        assert!(
            matches!(
                block_on(handlers[0].review(&call("fs.read")))?,
                ApprovalDecision::AskUser(_)
            ),
            "Platz 0 gehört der Config-Politik"
        );
        assert!(
            matches!(
                block_on(handlers[1].review(&call("fs.read")))?,
                ApprovalDecision::Allow
            ),
            "Platz 1 gehört der Standardpolitik"
        );
        assert_eq!(
            handlers[2].kind(),
            ApprovalHandlerKind::Interactive,
            "Platz 2 gehört dem Responder"
        );
        Ok(())
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
        assert_eq!(
            handlers[1].label(),
            "unnamed",
            "die Standardpolitik benennt sich nicht selbst"
        );
    }

    /// Die Zelle der Wurzel bleibt der Schalter: `set` erreicht die bereits
    /// montierte Politik sofort.
    #[test]
    fn the_root_mode_cell_stays_the_switch_for_the_installed_policy() -> TestResult {
        let cell = ApprovalModeCell::new(ApprovalMode::Delegated);
        let chain = ApprovalChain::for_root(
            &config_with(&[]),
            AskResolution::Interactive,
            cell.clone(),
            None,
            AllowRuleSet::new(),
        );
        let registry = chain.install(ExtensionRegistryBuilder::default()).build();
        let handler = Arc::clone(&registry.approval_handlers()[0]);

        assert!(matches!(
            block_on(handler.review(&call("shell.exec")))?,
            ApprovalDecision::AskUser(_)
        ));

        cell.set(ApprovalMode::FullAccess);
        assert!(
            matches!(
                block_on(handler.review(&call("shell.exec")))?,
                ApprovalDecision::Allow
            ),
            "die Umschaltung muss die montierte Registry erreichen"
        );
        Ok(())
    }

    /// Z2c-02: Für jede nicht-interaktive Auflösung hängt die Kette genau
    /// einen [`AskResolutionPolicy`] an — mit dem Label `ask:<auflösung>` und
    /// [`ApprovalHandlerKind::Other`].
    #[test]
    fn every_non_interactive_resolution_installs_its_own_handler() {
        let cases = [
            (AskResolution::RejectTurn, "ask:reject-turn"),
            (AskResolution::BlockJob, "ask:block-job"),
            (AskResolution::Fail, "ask:fail"),
        ];
        for (resolution, label) in cases {
            let chain = ApprovalChain::for_root(
                &config_with(&[]),
                resolution,
                ApprovalModeCell::new(ApprovalMode::Delegated),
                None,
                AllowRuleSet::new(),
            );
            assert_eq!(chain.ask_resolution(), Some(resolution));
            assert_eq!(
                chain.snapshot(),
                vec![
                    (DEFAULT_POLICY_LABEL, ApprovalHandlerKind::DefaultPolicy),
                    (label, ApprovalHandlerKind::Other),
                ],
                "{resolution:?}"
            );
            assert_eq!(chain.handlers().len(), 2, "{resolution:?}");
        }
    }

    /// `Interactive` bekommt keinen — dort antwortet eine Person.
    #[test]
    fn the_interactive_resolution_installs_no_ask_handler() {
        let chain = root_chain(&[], ApprovalMode::Delegated);
        assert_eq!(chain.ask_resolution(), None);
        assert_eq!(
            chain.snapshot(),
            vec![(DEFAULT_POLICY_LABEL, ApprovalHandlerKind::DefaultPolicy)]
        );
    }

    /// Der Handler lehnt **genau** die Aufrufe ab, die sonst eine Rückfrage
    /// auslösten — ein pauschales `Deny` würde jeden Werkzeugaufruf treffen.
    #[test]
    fn the_ask_handler_denies_only_what_would_have_been_asked() -> TestResult {
        let chain = ApprovalChain::for_root(
            &config_with(&[]),
            AskResolution::Fail,
            ApprovalModeCell::new(ApprovalMode::Delegated),
            None,
            AllowRuleSet::new(),
        );
        let handlers = chain.handlers();
        let ask = &handlers[1];

        // `fs.read` steht in `AUTO_APPROVED_TOOLS`: die Standardpolitik sagt
        // `Allow`, also darf auch dieser Handler nicht ablehnen.
        assert!(matches!(
            block_on(ask.review(&call("fs.read")))?,
            ApprovalDecision::Allow
        ));
        // `shell.exec` steht nicht darin: die Standardpolitik fragte, und die
        // Frage kann hier niemand beantworten.
        let decision = block_on(ask.review(&call("shell.exec")))?;
        match decision {
            ApprovalDecision::Deny(reason) => {
                assert!(reason.contains("shell.exec"), "{reason}");
                assert!(reason.contains("fails the run"), "{reason}");
            }
            other => {
                return Err(TestError::Unexpected(format!(
                    "erwartet Deny, war {other:?}"
                )));
            }
        }
        Ok(())
    }

    /// `FullAccess` fragt nichts — dann lehnt auch die Auflösung nichts ab.
    #[test]
    fn the_ask_handler_stays_silent_when_nothing_would_be_asked() -> TestResult {
        let chain = ApprovalChain::for_root(
            &config_with(&[]),
            AskResolution::BlockJob,
            ApprovalModeCell::new(ApprovalMode::FullAccess),
            None,
            AllowRuleSet::new(),
        );
        assert!(matches!(
            block_on(chain.handlers()[1].review(&call("shell.exec")))?,
            ApprovalDecision::Allow
        ));
        Ok(())
    }

    /// Die Pflichtliste der Konfiguration zieht denselben Handler nach sich —
    /// sonst bliebe ein `require_approval_for`-Werkzeug in einem Lauf ohne
    /// Antwortfläche unbemerkt erlaubt.
    #[test]
    fn the_ask_handler_also_resolves_the_config_policy_list() -> TestResult {
        let chain = ApprovalChain::for_root(
            &config_with(&["fs.read"]),
            AskResolution::RejectTurn,
            ApprovalModeCell::new(ApprovalMode::FullAccess),
            None,
            AllowRuleSet::new(),
        );
        // Reihenfolge: Config, Default, Ask.
        assert!(matches!(
            block_on(chain.handlers()[2].review(&call("fs.read")))?,
            ApprovalDecision::Deny(_)
        ));
        Ok(())
    }

    /// Das Kind erbt die Auflösung und liest seine **gedeckelte** Folgezelle.
    #[test]
    fn a_child_keeps_the_ask_resolution_over_its_own_cell() -> TestResult {
        let root = ApprovalChain::for_root(
            &config_with(&[]),
            AskResolution::Fail,
            ApprovalModeCell::new(ApprovalMode::FullAccess),
            None,
            AllowRuleSet::new(),
        );
        let child = root.for_child();

        assert_eq!(child.ask_resolution(), Some(AskResolution::Fail));
        // Die Wurzel steht auf `FullAccess`, das Kind auf `Delegated` (G-009):
        // derselbe Aufruf wird deshalb nur im Kind aufgelöst.
        assert!(matches!(
            block_on(root.handlers()[1].review(&call("shell.exec")))?,
            ApprovalDecision::Allow
        ));
        assert!(matches!(
            block_on(child.handlers()[1].review(&call("shell.exec")))?,
            ApprovalDecision::Deny(_)
        ));
        Ok(())
    }

    /// Eine Registry aus den Defaults erhält die regelbewusste
    /// Standardpolitik der Kette statt ihrer regellosen mitgebrachten.
    #[test]
    fn install_over_default_does_not_add_a_second_default_policy() {
        let cell = ApprovalModeCell::new(ApprovalMode::Delegated);
        let chain = ApprovalChain::for_root(
            &config_with(&["fs.write"]),
            AskResolution::Fail,
            cell.clone(),
            None,
            AllowRuleSet::new(),
        );
        // Die Gestalt, die `assemble_registry_for_project` liefert: genau eine
        // `DefaultApprovalPolicy` über derselben Zelle.
        let assembled = ExtensionRegistryBuilder::default()
            .approval_handler(Arc::new(DefaultApprovalPolicy::new(cell)))
            .build();

        let registry = chain.install_over_default(assembled).build();

        // Standardpolitik (aus der Kette) + Config + Ask — keine vierte.
        assert_eq!(registry.approval_handlers().len(), 3);
        assert_eq!(registry.approval_handlers().len(), chain.snapshot().len());
    }

    #[test]
    fn install_over_default_replaces_the_default_with_the_rules_aware_policy() -> TestResult {
        let cell = ApprovalModeCell::new(ApprovalMode::Delegated);
        let rules = AllowRuleSet::new();
        rules.add(ApprovalRule {
            tool: "shell.exec".to_owned(),
            pattern: Some("git status".to_owned()),
            decision: RuleDecision::Allow,
            scope: RuleScope::Project,
        });
        let chain = ApprovalChain::for_root(
            &config_with(&[]),
            AskResolution::Fail,
            cell.clone(),
            None,
            rules,
        );
        let assembled = ExtensionRegistryBuilder::default()
            .approval_handler(Arc::new(DefaultApprovalPolicy::new(cell)))
            .build();

        let registry = chain.install_over_default(assembled).build();

        // Default und Ask-Auflösung müssen beide die gleiche Regelmenge sehen;
        // eine regellose Default-Policy würde hier stattdessen AskUser liefern.
        assert!(matches!(
            block_on(registry.approval_handlers()[0].review(&shell_call("git status --short")))?,
            ApprovalDecision::Allow
        ));
        assert!(matches!(
            block_on(registry.approval_handlers()[1].review(&shell_call("git status --short")))?,
            ApprovalDecision::Allow
        ));
        Ok(())
    }

    /// Eine Registry anderer Gestalt bekommt die Grundlinie trotzdem.
    #[test]
    fn install_over_default_still_adds_the_baseline_to_an_empty_registry() -> TestResult {
        let chain = root_chain(&[], ApprovalMode::Delegated);
        let registry = chain
            .install_over_default(ExtensionRegistryBuilder::default().build())
            .build();

        assert_eq!(registry.approval_handlers().len(), 1);
        assert!(matches!(
            block_on(registry.approval_handlers()[0].review(&call("shell.exec")))?,
            ApprovalDecision::AskUser(_)
        ));
        Ok(())
    }

    // ── Contract §2/§4: Freigaberegeln in der Kette ──────────────────────────

    fn shell_call(command: &str) -> ToolCall {
        let mut call = call("shell.exec");
        call.arguments = serde_json::json!({ "command": command });
        call
    }

    /// Eine passende Erlauben-Regel überspringt die Rückfrage der
    /// Standardpolitik, ohne dass die Kette einen zweiten Handler bräuchte.
    #[test]
    fn review_allow_rule_bypasses_default_policys_ask() -> TestResult {
        let rules = AllowRuleSet::new();
        rules.add(ApprovalRule {
            tool: "shell.exec".to_owned(),
            pattern: Some("git status".to_owned()),
            decision: RuleDecision::Allow,
            scope: RuleScope::Project,
        });
        let chain = ApprovalChain::for_root(
            &config_with(&[]),
            AskResolution::Interactive,
            ApprovalModeCell::new(ApprovalMode::Delegated),
            None,
            rules,
        );

        assert!(matches!(
            block_on(chain.handlers()[0].review(&shell_call("git status --short")))?,
            ApprovalDecision::Allow
        ));
        Ok(())
    }

    /// Contract §2: Deny gewinnt über eine passende Allow-Regel und über den
    /// Modus `full` — beides würde ohne die Deny-Regel automatisch freigeben.
    #[test]
    fn review_deny_rule_beats_allow_rule_and_full_access_mode() -> TestResult {
        let rules = AllowRuleSet::new();
        rules.add(ApprovalRule {
            tool: "shell.exec".to_owned(),
            pattern: Some("git push".to_owned()),
            decision: RuleDecision::Allow,
            scope: RuleScope::Global,
        });
        rules.add(ApprovalRule {
            tool: "shell.exec".to_owned(),
            pattern: Some("git push".to_owned()),
            decision: RuleDecision::Deny,
            scope: RuleScope::Session,
        });
        let chain = ApprovalChain::for_root(
            &config_with(&[]),
            AskResolution::Interactive,
            ApprovalModeCell::new(ApprovalMode::FullAccess),
            None,
            rules,
        );

        assert!(matches!(
            block_on(chain.handlers()[0].review(&shell_call("git push origin main")))?,
            ApprovalDecision::AskUser(_)
        ));
        Ok(())
    }

    /// Ein Kind teilt exakt die Regelmenge der Wurzel (kein `detached()`):
    /// eine über die Wurzel hinzugefügte Regel wirkt sofort auch im Kind.
    #[test]
    fn a_child_shares_exactly_the_parents_rule_set() -> TestResult {
        let root = ApprovalChain::for_root(
            &config_with(&[]),
            AskResolution::Interactive,
            ApprovalModeCell::new(ApprovalMode::Delegated),
            None,
            AllowRuleSet::new(),
        );
        let child = root.for_child();

        root.rules().add(ApprovalRule {
            tool: "shell.exec".to_owned(),
            pattern: Some("git status".to_owned()),
            decision: RuleDecision::Allow,
            scope: RuleScope::Session,
        });

        assert!(
            matches!(
                block_on(child.handlers()[0].review(&shell_call("git status")))?,
                ApprovalDecision::Allow
            ),
            "das Kind muss dieselbe (geteilte) Regelmenge sehen wie die Wurzel"
        );
        assert_eq!(child.rules().snapshot(), root.rules().snapshot());
        Ok(())
    }

    /// Ein Kind bekommt eine Deny-Regel der Wurzel niemals „geschenkt" weg —
    /// sie wirkt dort genauso einschränkend wie in der Wurzel selbst, auch
    /// unter `FullAccess` (analog zu `a_child_never_inherits_full_access`).
    #[test]
    fn a_child_never_loses_a_deny_rule_of_the_parent() -> TestResult {
        let rules = AllowRuleSet::new();
        rules.add(ApprovalRule {
            tool: "fs.write".to_owned(),
            pattern: None,
            decision: RuleDecision::Deny,
            scope: RuleScope::Global,
        });
        let root = ApprovalChain::for_root(
            &config_with(&[]),
            AskResolution::Interactive,
            ApprovalModeCell::new(ApprovalMode::FullAccess),
            None,
            rules,
        );
        let child = root.for_child();

        assert!(matches!(
            block_on(child.handlers()[0].review(&call("fs.write")))?,
            ApprovalDecision::AskUser(_)
        ));
        Ok(())
    }

    /// Die Ask-Vorhersage (`AskResolutionPolicy::would_ask`) muss dieselbe
    /// Regelmenge lesen wie die Standardpolitik — sonst würde ein nicht
    /// interaktiver Einstieg eine per Regel freigegebene Anfrage fälschlich
    /// ablehnen (kein Responder kann sie sonst je beantworten).
    #[test]
    fn ask_resolution_prediction_honors_an_allow_rule() -> TestResult {
        let rules = AllowRuleSet::new();
        rules.add(ApprovalRule {
            tool: "shell.exec".to_owned(),
            pattern: Some("git status".to_owned()),
            decision: RuleDecision::Allow,
            scope: RuleScope::Project,
        });
        let chain = ApprovalChain::for_root(
            &config_with(&[]),
            AskResolution::Fail,
            ApprovalModeCell::new(ApprovalMode::Delegated),
            None,
            rules,
        );
        // Reihenfolge ohne Config-Politik: Default, Ask.
        assert!(matches!(
            block_on(chain.handlers()[1].review(&shell_call("git status --short")))?,
            ApprovalDecision::Allow
        ));
        Ok(())
    }
}
