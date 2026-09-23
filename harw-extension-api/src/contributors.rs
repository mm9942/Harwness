//! Contributor-Traits — wo sich eine Extension einklinkt (v1 Minimal-Set).
//!
//! # Namensraum/Trust auf `ContextProvider` selbst (Nachzug zu AW3-03)
//!
//! [`ContextProvider::namespace`] und [`ContextProvider::max_trust`] sitzen
//! bewusst auf diesem Trait und nicht an der Stelle, die einen Provider
//! registriert (`harw_extension_api::registry::ExtensionRegistryBuilder`).
//! Der Unterschied ist keine Geschmacksfrage: eine Prüfung am
//! Registrierungsaufruf kann nur etwas prüfen, das ihr der Aufrufer aktiv
//! mitgibt — und ein Aufrufer, der `Arc::new(Provider) as Arc<dyn
//! ContextProvider>` baut und diesen typgelöschten Wert weitergibt (genau der
//! Fall in `harw-cli` und `harw-tui`, die bereits registrierte Provider aus
//! einer bestehenden Registry in eine neue kopieren), hat spätestens ab
//! diesem Punkt keinen Zugriff mehr auf assoziierte Konstanten des
//! ursprünglichen, konkreten Typs. Eine zweite, separate „Deklarations"-Spur
//! (wie das vorherige `DeclaredContextProvider`) hilft nur, solange *jeder*
//! Registrierungsweg sie auch tatsächlich benutzt — und genau das ließ sich
//! nicht erzwingen, weil `ExtensionRegistryBuilder::context_provider(Arc<dyn
//! ContextProvider>)` unverändert weiter existierte und keine Ahnung von
//! einer zweiten Spur hatte. Sitzen `namespace()`/`max_trust()` dagegen als
//! virtuelle Methoden auf `ContextProvider` selbst, trägt sie **jeder** Wert
//! dieses Typs mit sich, typgelöscht oder nicht — eine virtuelle Methode
//! überlebt `Arc<dyn Trait>`, eine assoziierte Konstante nicht. Das macht die
//! Prüfung am Registrierungsaufruf zum ersten Mal tatsächlich unumgehbar,
//! unabhängig davon, über wie viele Zwischenstationen ein Provider dorthin
//! gelangt. Siehe `harw_extension_api::registry`-Moduldoku für die konkrete
//! Durchsetzung.
//!
//! # Die zweite Naht: [`ContextProvider::contribute_v2`] (v1 → v2, Blocker 2)
//!
//! Befund: kein `ContextProvider` erzeugt `harw_context::Fragment` (v2) —
//! `contribute` liefert ausnahmslos das alte [`ContextFragment`], und
//! `harw-core::turn_loop::gather_context` (außerhalb des Schreibbereichs
//! dieses Knotens) ruft ausschließlich `contribute` auf. Alle dreizehn
//! bestehenden `ContextProvider`-Implementierungen sofort auf v2 umzustellen
//! hieße, dreizehn fremde Crates gleichzeitig zu ändern — nicht die Aufgabe
//! dieses Knotens und nicht in seinem Schreibbereich.
//!
//! **Drei mögliche Nähte standen zur Wahl:**
//!
//! 1. **Eine zusätzliche Trait-Methode mit Vorgabe** (gewählt), die
//!    `fragment_from_v1` auf das Ergebnis von `contribute` anwendet. Jeder
//!    Provider liefert damit sofort v2, ohne etwas zu tun; wer echte
//!    Herkunfts-/Vertrauensangaben hat, überschreibt die Methode direkt.
//! 2. **Ein zweiter Trait neben `ContextProvider`** (z. B.
//!    `ContextProviderV2: ContextProvider`), den ein Provider zusätzlich
//!    implementiert. Verworfen: die Registry hält Provider ausschließlich als
//!    `Arc<dyn ContextProvider>` (siehe oben, Abschnitt „Namensraum/Trust auf
//!    `ContextProvider` selbst" — derselbe Grund gilt hier erneut). Ein
//!    zweiter Trait wäre für einen bereits typgelöschten Wert nur über
//!    `Any`-Downcasting erreichbar, oder er bräuchte eine zweite, parallele
//!    Provider-Liste in der Registry — zwei Wege, auf denen sich „hat v2, hat
//!    nicht v2" wieder auseinanderentwickeln kann, genau das Muster, das
//!    `namespace()`/`max_trust()` hier schon einmal beheben mussten.
//! 3. **Eine Umhüllung in der Registry**, die für jeden Provider `contribute`
//!    aufruft und pauschal über `fragment_from_v1` umwandelt. Verworfen: eine
//!    Umhüllung sieht nur, was `contribute` zurückgibt — ein Provider, der
//!    seine wahre Herkunft oder eine höhere Vertrauensklasse kennt, hat
//!    keinen Weg, das der Registry mitzuteilen. Die Registry kann pauschal
//!    konvertieren, aber niemals veredeln; genau das *Veredeln* ist der Punkt
//!    der Aufgabe.
//!
//! Variante 1 ist die einzige der drei, die (a) auf demselben, typgelöschten
//! `Arc<dyn ContextProvider>`-Wert funktioniert, den die Registry bereits
//! hält, (b) einem Provider erlaubt, seine echte Herkunft und
//! Vertrauensklasse zu liefern, statt nur pauschal zu konvertieren, und (c)
//! `fragment_from_v1` als **einzige** Brücke unangetastet lässt: die Vorgabe
//! von [`ContextProvider::contribute_v2`] ruft `fragment_from_v1` auf, statt
//! eine eigene, zweite Abbildung von `ContextFragment` auf `Fragment` zu
//! schreiben — der Fall, den die Moduldoku von `v1_compat.rs` ausdrücklich
//! verhindern will.
//!
//! `#[harw_macros::context_provider]` überschreibt `contribute_v2`
//! **nicht** (Stand dieses Knotens; siehe `harw-macros/src/contributor.rs`,
//! `expand_context_provider` — es erzeugt ausschließlich `contribute`,
//! `namespace()` und `max_trust()`). Ein makro-erzeugter Provider fällt damit
//! auf die Vorgabe zurück, exakt wie ein handgeschriebener Provider ohne
//! eigene Überschreibung — unverändertes Verhalten, keine stille Lücke.

use crate::types::*;
use crate::v1_compat::{V1_BRIDGE_TIMESTAMP, V1_UNLABELED, fragment_from_v1};
use harw_context::TrustClass;
use harw_tools::{ToolCall, ToolExecutor, ToolName, ToolSpec};
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

pub type ExtFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Stellt Tools bereit.
pub trait ToolProvider: Send + Sync {
    fn tools(&self) -> Vec<ToolSpec>;
    fn executor(&self, name: &ToolName) -> Option<Arc<dyn ToolExecutor>>;
    /// Whether independent calls to this tool may run concurrently. Defaults
    /// to false: tool side effects are presumed non-commutative unless a
    /// provider explicitly proves otherwise.
    fn parallel_safe(&self, _name: &ToolName) -> bool {
        false
    }
}

/// Reichert Turn-Input mit Kontext an.
pub trait ContextProvider: Send + Sync {
    fn contribute<'a>(&'a self, ctx: &'a TurnInputContext) -> ExtFuture<'a, Vec<ContextFragment>>;

    /// Der Namensraum, unter dem dieser Provider Kontext liefert.
    ///
    /// # Description
    /// Grundlage der Kollisionsprüfung bei der Registrierung (siehe
    /// `harw_extension_api::registry::ExtensionRegistryBuilder::context_provider`):
    /// zwei Provider, die denselben (getrimmten) Namensraum beanspruchen,
    /// dürfen nicht beide registriert werden, sonst überschreibt einer den
    /// anderen still. `#[harw_macros::context_provider(namespace = "...")]`
    /// überschreibt diese Methode mit dem deklarierten Wert; ein Provider,
    /// der sie nicht überschreibt, bekommt hier den vollqualifizierten
    /// Rust-Typnamen (`std::any::type_name::<Self>()`) — nie leer, aber auch
    /// nicht garantiert kollisionsfrei über mehrere Instanzen desselben
    /// konkreten Typs hinweg. Ein Provider, der mehrfach mit
    /// unterschiedlicher Bedeutung registriert werden soll (z. B. je Projekt
    /// eine Instanz), muss deshalb einen eigenen Namensraum deklarieren statt
    /// sich auf diesen Vorgabewert zu verlassen.
    ///
    /// # Returns
    /// Den (noch nicht getrimmten) Namensraum dieses Providers.
    ///
    /// # Examples
    /// ```rust
    /// use harw_extension_api::contributors::{ContextProvider, ExtFuture};
    /// use harw_extension_api::types::{ContextFragment, TurnInputContext};
    ///
    /// struct Undeclared;
    /// impl ContextProvider for Undeclared {
    ///     fn contribute<'a>(&'a self, _ctx: &'a TurnInputContext) -> ExtFuture<'a, Vec<ContextFragment>> {
    ///         Box::pin(async { Vec::new() })
    ///     }
    /// }
    ///
    /// assert!(!Undeclared.namespace().is_empty());
    /// ```
    fn namespace(&self) -> &'static str {
        std::any::type_name::<Self>()
    }

    /// Die höchste Vertrauensklasse, die dieser Provider für seine Fragmente
    /// behauptet.
    ///
    /// # Description
    /// [`TrustClass`] entscheidet, in welchem der beiden Blöcke (AW4-01) ein
    /// Fragment landet. Der Vorgabewert ist ausdrücklich
    /// [`TrustClass::Data`] — die **niedrigste** Klasse, nie die höchste und
    /// nie „unbekannt behandelt wie vertrauenswürdig": ein Provider, der
    /// nichts behauptet, bekommt die Behandlung „bis zum Beweis des
    /// Gegenteils nicht besonders vertrauenswürdig". Eine höhere Klasse muss
    /// aktiv und sichtbar im Quelltext deklariert werden (z. B. über
    /// `#[harw_macros::context_provider(trust = Instruction)]`); sie fällt
    /// einem Provider nie durch bloßes Weglassen zu. Dieselbe Wahl trifft
    /// `harw-extension-api/src/v1_compat.rs` für v1-Fragmente und
    /// `harw-macros/src/contributor.rs` für den generierten `TRUST`-Vorgabewert
    /// aus genau demselben Grund.
    ///
    /// # Returns
    /// [`TrustClass::Data`], sofern nicht überschrieben.
    ///
    /// # Examples
    /// ```rust
    /// use harw_context::TrustClass;
    /// use harw_extension_api::contributors::{ContextProvider, ExtFuture};
    /// use harw_extension_api::types::{ContextFragment, TurnInputContext};
    ///
    /// struct Undeclared;
    /// impl ContextProvider for Undeclared {
    ///     fn contribute<'a>(&'a self, _ctx: &'a TurnInputContext) -> ExtFuture<'a, Vec<ContextFragment>> {
    ///         Box::pin(async { Vec::new() })
    ///     }
    /// }
    ///
    /// assert_eq!(Undeclared.max_trust(), TrustClass::Data);
    /// ```
    fn max_trust(&self) -> TrustClass {
        TrustClass::Data
    }

    /// Liefert dieses Providers Fragmente als v2-[`harw_context::Fragment`].
    ///
    /// # Description
    /// Die zweite Naht zwischen v1 und v2 (siehe Moduldoku, Abschnitt „Die
    /// zweite Naht"). Die Vorgabe-Implementierung ruft [`Self::contribute`]
    /// auf und übergibt jedes zurückgegebene [`ContextFragment`] einzeln an
    /// [`crate::v1_compat::fragment_from_v1`] — die einzige Brücke zwischen
    /// den beiden Fragment-Formen. Sie setzt dabei ausnahmslos
    /// [`TrustClass::Data`] (niedrigste Klasse) und
    /// [`harw_context::Stability::Fresh`], mit
    /// [`crate::v1_compat::V1_BRIDGE_TIMESTAMP`] (fester Ersatzwert, keine
    /// Systemuhr) als `produced_at`. Ein Provider, der nichts überschreibt,
    /// verhält sich damit exakt so, als hätte ihn ein Aufrufer manuell durch
    /// `fragment_from_v1` geschickt — keine zusätzliche, abweichende
    /// Zuordnung entsteht hier.
    ///
    /// Ein Provider, der seine echte Herkunft, Vertrauensklasse oder
    /// Beständigkeit kennt, überschreibt diese Methode direkt und konstruiert
    /// eigene [`harw_context::Fragment`]-Werte, statt sich auf die Vorgabe zu
    /// verlassen — die Vorgabe ist ein sicherer Rückfall, keine Obergrenze
    /// für das, was ein Provider ausdrücken kann.
    ///
    /// Ein `old.label`, das [`harw_context::FragmentLabel::try_new`] ablehnt
    /// (leer oder steuerzeichenhaltig nach Trimmen — bei v1-Providern in der
    /// Praxis nicht zu erwarten, aber von [`ContextFragment`] selbst nicht
    /// ausgeschlossen), wird aus dem Ergebnis stillschweigend ausgelassen
    /// statt die gesamte Konvertierung abzubrechen: ein einzelnes
    /// unbrauchbares Fragment darf die übrigen, gültigen Fragmente desselben
    /// Providers nicht verschlucken. Ein Provider, dem das nicht genügt, muss
    /// diese Methode überschreiben.
    ///
    /// # Arguments
    /// - `ctx` (`&'a TurnInputContext`): derselbe Kontext, den auch
    ///   [`Self::contribute`] erhält.
    ///
    /// # Returns
    /// `Vec<harw_context::Fragment>` — für die Vorgabe-Implementierung genau
    /// die erfolgreich konvertierten Fragmente aus [`Self::contribute`], in
    /// derselben Reihenfolge.
    ///
    /// # Concurrency
    /// Wie [`Self::contribute`]: liefert ein `Future`, das `Send` ist und an
    /// `'a` gebunden bleibt; keine innere Veränderlichkeit.
    ///
    /// # Examples
    /// ```rust,ignore
    /// // `ignore`, weil ein `.await` hier einen async-Executor braucht, den
    /// // diese Crate nicht als Dev-Dependency führt (siehe Tests in diesem
    /// // Modul für ein tatsächlich ausgeführtes Äquivalent).
    /// use harw_extension_api::contributors::{ContextProvider, ExtFuture};
    /// use harw_extension_api::types::{ContextFragment, TurnInputContext};
    /// use harw_context::TrustClass;
    ///
    /// struct Undeclared;
    /// impl ContextProvider for Undeclared {
    ///     fn contribute<'a>(&'a self, _ctx: &'a TurnInputContext) -> ExtFuture<'a, Vec<ContextFragment>> {
    ///         Box::pin(async {
    ///             vec![ContextFragment { label: "plan.current".to_owned(), content: "todo".to_owned() }]
    ///         })
    ///     }
    /// }
    ///
    /// let fragments = Undeclared.contribute_v2(&TurnInputContext::default()).await;
    /// assert_eq!(fragments.len(), 1);
    /// assert_eq!(fragments[0].trust, TrustClass::Data);
    /// ```
    fn contribute_v2<'a>(
        &'a self,
        ctx: &'a TurnInputContext,
    ) -> ExtFuture<'a, Vec<harw_context::Fragment>> {
        let legacy = self.contribute(ctx);
        Box::pin(async move {
            legacy
                .await
                .iter()
                .filter_map(|fragment| {
                    // Ein leeres Label ist in v1 zulässig; `FragmentLabel::try_new`
                    // weist es zurück. Ohne diese Normalisierung verschwände ein
                    // solches Fragment **spurlos** — der alte Sammelpfad in
                    // `harw_core::turn_loop::gather_context` hat es dagegen seit
                    // jeher unter `"unlabeled"` durchgereicht und nur gegen die
                    // Aktivierung geprüft. Eine Brücke darf das Verhalten nicht
                    // stillschweigend verschärfen: ein verworfenes Fragment ist von
                    // einem nie beigesteuerten nicht zu unterscheiden.
                    let normalized = if fragment.label.is_empty() {
                        ContextFragment {
                            label: V1_UNLABELED.to_owned(),
                            content: fragment.content.clone(),
                        }
                    } else {
                        fragment.clone()
                    };
                    fragment_from_v1(&normalized, V1_BRIDGE_TIMESTAMP).ok()
                })
                .collect()
        })
    }
}

/// Lädt System-Instructions.
pub trait InstructionsProvider: Send + Sync {
    fn load<'a>(&'a self) -> ExtFuture<'a, LoadedInstructions>;
}

/// Grobe Einordnung, welche Art von Freigabe-Entscheider ein
/// [`ApprovalHandler`] ist — reine Introspektion (Diagnose, Logging,
/// Auswahl unter mehreren registrierten Handlern), keine Verhaltenssteuerung.
///
/// # Beschreibung
/// Ein Wert hier behauptet nichts über die *Qualität* der Entscheidung, nur
/// über ihre *Herkunftsart*. Vorgabe (siehe [`ApprovalHandler::kind`]) ist
/// [`Self::Other`] — ein Handler muss eine spezifischere Einordnung aktiv
/// wählen, sie fällt ihm nie zu.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ApprovalHandlerKind {
    /// Entscheidet anhand einer statischen, aus Konfiguration geladenen Politik.
    ConfigPolicy,
    /// Die eingebaute Standardpolitik ohne externe Konfiguration.
    DefaultPolicy,
    /// Fragt eine anwesende Person direkt (z. B. über ein TUI-Prompt).
    Interactive,
    /// Fragt über einen entfernten Kanal (z. B. Chat, Webhook) nach.
    Channel,
    /// Persistiert die Entscheidung dauerhaft (z. B. für spätere Wiedergabe/Audit).
    Durable,
    /// Keine der obigen Kategorien, oder nicht deklariert.
    Other,
}

/// Guardrail: prüft vor Tool-Ausführung.
///
/// # Vertrag
/// [`Self::review`] ist seiteneffektfrei: es öffnet keine Prompts, schreibt
/// nichts und darf beliebig oft aufgerufen werden, ohne dass sich das
/// Ergebnis für denselben `call` je nach Aufrufreihenfolge ändert. Ein
/// Handler, der eine Person tatsächlich fragen muss (z. B. [`ApprovalDecision::AskUser`]),
/// tut das nicht in `review` selbst, sondern verweist über die
/// zurückgegebene `ItemId` auf eine spätere, separate Interaktion.
pub trait ApprovalHandler: Send + Sync {
    fn review<'a>(&'a self, call: &'a ToolCall) -> ExtFuture<'a, ApprovalDecision>;

    /// Grobe Einordnung dieses Handlers. Vorgabe: [`ApprovalHandlerKind::Other`].
    fn kind(&self) -> ApprovalHandlerKind {
        ApprovalHandlerKind::Other
    }

    /// Menschenlesbarer, stabiler Kurzname für Diagnose/Introspektion.
    /// Vorgabe: `"unnamed"`.
    fn label(&self) -> &'static str {
        "unnamed"
    }
}

#[derive(Debug, Clone)]
pub enum ApprovalDecision {
    Allow,
    Deny(String),
    AskUser(harw_types::ItemId),
}

/// Turn-Lifecycle Observer (Tracing, Metriken).
pub trait TurnObserver: Send + Sync {
    fn on_turn_start(&self, _input: &TurnStartInput) {}
    fn on_turn_stop(&self, _input: &TurnStopInput) {}
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TestResult;
    use harw_context::Stability;
    use std::task::{Context, Poll, Waker};

    /// Treibt eine `ExtFuture` synchron zu Ende, ohne einen externen
    /// Async-Executor als Dev-Dependency zu brauchen. `Waker::noop()` ist
    /// seit Rust 1.85 stabil — genau die `rust-version` dieses Workspace —
    /// und macht dafür kein `unsafe` nötig (diese Crate trägt
    /// `#![forbid(unsafe_code)]`). Jede Zukunft in diesem Modul ist nach dem
    /// ersten `Box::pin(async { .. })`-Schritt ohne inneres `.await`
    /// aufgebaut und wird deshalb beim ersten Poll fertig; die Schleife ist
    /// trotzdem allgemein, falls sich das künftig ändert.
    fn block_on<T>(mut fut: Pin<Box<dyn Future<Output = T> + Send + '_>>) -> T {
        let waker = Waker::noop();
        let mut cx = Context::from_waker(waker);
        loop {
            if let Poll::Ready(value) = fut.as_mut().poll(&mut cx) {
                return value;
            }
        }
    }

    fn ctx() -> TurnInputContext {
        TurnInputContext::default()
    }

    /// Ein Provider, der `contribute_v2` nicht überschreibt.
    struct PlainProvider;

    impl ContextProvider for PlainProvider {
        fn contribute<'a>(
            &'a self,
            _ctx: &'a TurnInputContext,
        ) -> ExtFuture<'a, Vec<ContextFragment>> {
            Box::pin(async {
                vec![ContextFragment {
                    label: "project.root".to_owned(),
                    content: "project_root=/srv/app".to_owned(),
                }]
            })
        }
    }

    /// Ein Provider, der `contribute_v2` direkt überschreibt und eigene,
    /// „echte" Herkunfts-/Vertrauensangaben liefert.
    struct EnrichedProvider;

    impl ContextProvider for EnrichedProvider {
        fn contribute<'a>(
            &'a self,
            _ctx: &'a TurnInputContext,
        ) -> ExtFuture<'a, Vec<ContextFragment>> {
            // Wird von diesem Test nicht erwartet aufgerufen zu werden --
            // `contribute_v2` ist direkt überschrieben und darf nicht auf
            // die Vorgabe (die `contribute` aufruft) zurückfallen.
            Box::pin(async { Vec::new() })
        }

        fn contribute_v2<'a>(
            &'a self,
            _ctx: &'a TurnInputContext,
        ) -> ExtFuture<'a, Vec<harw_context::Fragment>> {
            Box::pin(async {
                let body = "verified plan snapshot".to_owned();
                // Feste, gültige Test-Literale ("plan.current"/"plan") -- dieser
                // Zweig ist unerreichbar, aber `contribute_v2` liefert per Trait
                // `Vec<Fragment>` ohne `Result`, daher kein `?`/Panik: ein
                // Fehlschlag würde ein leeres Ergebnis liefern, das die
                // Test-Assertion (`fragments.len()`) sichtbar macht.
                let (Ok(label), Ok(section)) = (
                    harw_context::FragmentLabel::try_new("plan.current"),
                    harw_context::SectionName::try_new("plan"),
                ) else {
                    return Vec::new();
                };
                vec![harw_context::Fragment {
                    label,
                    section,
                    trust: TrustClass::Evidence,
                    stability: Stability::Pinned,
                    origin: harw_context::FragmentOrigin {
                        provider: "EnrichedProvider".to_owned(),
                        namespace: "plan".to_owned(),
                        produced_at: jiff::Timestamp::UNIX_EPOCH,
                    },
                    cost: harw_lens_types::CostEstimate(4),
                    digest: harw_types::ContentDigest::of(body.as_bytes()),
                    body,
                }]
            })
        }
    }

    /// Der wichtigste Test dieses Knotens: ein Provider, der `contribute_v2`
    /// nicht überschreibt, verhält sich exakt so, als würde man sein
    /// `contribute`-Ergebnis manuell durch `fragment_from_v1` schicken --
    /// keine abweichende, zweite Abbildung entsteht durch die Vorgabe.
    #[test]
    fn contribute_v2_default_matches_fragment_from_v1_of_contribute_result() -> TestResult {
        let provider = PlainProvider;

        let via_default = block_on(provider.contribute_v2(&ctx()));
        let legacy = block_on(provider.contribute(&ctx()));
        let via_manual_bridge: Vec<harw_context::Fragment> = legacy
            .iter()
            .map(|f| fragment_from_v1(f, V1_BRIDGE_TIMESTAMP))
            .collect::<Result<Vec<_>, _>>()
            .map_err(crate::test_support::ctx("valid v1 label converts"))?;

        assert_eq!(via_default, via_manual_bridge);
        Ok(())
    }

    /// Die Vorgabe setzt die niedrigste Vertrauensklasse und `Fresh` --
    /// nachvollzogen direkt am Ergebnis, nicht nur an `fragment_from_v1`
    /// selbst.
    #[test]
    fn contribute_v2_default_trust_is_lowest_class_and_stability_is_fresh() {
        let fragments = block_on(PlainProvider.contribute_v2(&ctx()));

        assert_eq!(fragments.len(), 1);
        assert_eq!(fragments[0].trust, TrustClass::Data);
        assert_eq!(fragments[0].stability, Stability::Fresh);
        assert!(
            TrustClass::Data.trust_rank() < TrustClass::Evidence.trust_rank()
                && TrustClass::Data.trust_rank() < TrustClass::Instruction.trust_rank(),
            "the default must be the lowest trust class, not merely *a* class"
        );
    }

    /// Ein Provider, der `contribute_v2` direkt überschreibt, kommt mit
    /// seinen eigenen Werten durch -- nicht mit den Vorgaben der Brücke.
    #[test]
    fn contribute_v2_override_bypasses_default_bridge_values() {
        let fragments = block_on(EnrichedProvider.contribute_v2(&ctx()));

        assert_eq!(fragments.len(), 1);
        assert_eq!(fragments[0].trust, TrustClass::Evidence);
        assert_eq!(fragments[0].stability, Stability::Pinned);
        assert_eq!(fragments[0].origin.provider, "EnrichedProvider");
        assert_eq!(fragments[0].origin.namespace, "plan");
    }

    /// `fragment_from_v1` bleibt für seine bestehenden Aufrufer unverändert
    /// aufrufbar -- exakt dieselbe Signatur, die dieser Knoten nicht anfasst.
    #[test]
    fn fragment_from_v1_signature_is_unchanged_for_existing_callers() {
        let old = ContextFragment {
            label: "history.tail".to_owned(),
            content: "hello".to_owned(),
        };

        let fragment: Result<harw_context::Fragment, harw_context::ContextError> =
            fragment_from_v1(&old, jiff::Timestamp::UNIX_EPOCH);

        assert!(fragment.is_ok());
    }

    /// Ein Provider, dessen v1-Fragment ein leeres Label trägt.
    struct EmptyLabelProvider;

    impl ContextProvider for EmptyLabelProvider {
        fn contribute<'a>(
            &'a self,
            _ctx: &'a TurnInputContext,
        ) -> ExtFuture<'a, Vec<ContextFragment>> {
            Box::pin(async {
                vec![ContextFragment {
                    label: String::new(),
                    content: "unnamed fragment body".to_owned(),
                }]
            })
        }
    }

    /// Der wichtigste Test dieser Korrektur: vor ihr verwarf `.ok()` das
    /// Fragment spurlos, weil `FragmentLabel::try_new("")` fehlschlägt. Diese
    /// Behauptung wäre vor der Korrektur rot gewesen -- `fragments` wäre leer
    /// statt ein Element mit Label `"unlabeled"` zu tragen.
    #[test]
    fn contribute_v2_default_normalizes_empty_label_to_unlabeled() {
        let fragments = block_on(EmptyLabelProvider.contribute_v2(&ctx()));

        assert_eq!(
            fragments.len(),
            1,
            "a fragment with an empty v1 label must not be silently dropped"
        );
        assert_eq!(fragments[0].label.as_str(), crate::v1_compat::V1_UNLABELED);
    }

    /// Ein nicht-leeres Label bleibt durch die Normalisierung unverändert --
    /// die Vorgabe fasst nur ein leeres Label an.
    #[test]
    fn contribute_v2_default_preserves_nonempty_label() {
        let fragments = block_on(PlainProvider.contribute_v2(&ctx()));

        assert_eq!(fragments.len(), 1);
        assert_eq!(fragments[0].label.as_str(), "project.root");
    }

    /// Der Inhalt (`body`) überlebt die Normalisierung unverändert -- sie
    /// fasst ausschließlich das Label an, nie `content`/`body`.
    #[test]
    fn contribute_v2_default_preserves_body_through_label_normalization() {
        let fragments = block_on(EmptyLabelProvider.contribute_v2(&ctx()));

        assert_eq!(fragments.len(), 1);
        assert_eq!(fragments[0].body, "unnamed fragment body");
    }

    /// Ein Provider, der `contribute_v2` selbst überschreibt, geht nicht
    /// durch die Brücke -- selbst wenn sein `contribute()` ein leeres Label
    /// liefern würde, berührt die Normalisierung sein tatsächliches Ergebnis
    /// nie, weil die Vorgabe-Implementierung (und damit ihre Normalisierung)
    /// überhaupt nicht aufgerufen wird.
    struct EmptyLabelOverrideProvider;

    impl ContextProvider for EmptyLabelOverrideProvider {
        fn contribute<'a>(
            &'a self,
            _ctx: &'a TurnInputContext,
        ) -> ExtFuture<'a, Vec<ContextFragment>> {
            // Absichtlich ein leeres Label -- würde dieser Pfad je über die
            // Vorgabe-Brücke laufen, würde er normalisiert. Er soll aber nie
            // aufgerufen werden, weil `contribute_v2` unten direkt überschrieben ist.
            Box::pin(async {
                vec![ContextFragment {
                    label: String::new(),
                    content: "should never be bridged".to_owned(),
                }]
            })
        }

        fn contribute_v2<'a>(
            &'a self,
            _ctx: &'a TurnInputContext,
        ) -> ExtFuture<'a, Vec<harw_context::Fragment>> {
            Box::pin(async {
                let body = "own fragment, own label".to_owned();
                // Feste, gültige Test-Literale ("own.label"/"own") -- siehe
                // Begründung bei `EnrichedProvider::contribute_v2` oben: kein
                // `Result` im Trait-Rückgabetyp, daher leeres Ergebnis statt
                // Panik im (unerreichbaren) Fehlerzweig.
                let (Ok(label), Ok(section)) = (
                    harw_context::FragmentLabel::try_new("own.label"),
                    harw_context::SectionName::try_new("own"),
                ) else {
                    return Vec::new();
                };
                vec![harw_context::Fragment {
                    label,
                    section,
                    trust: TrustClass::Evidence,
                    stability: Stability::Pinned,
                    origin: harw_context::FragmentOrigin {
                        provider: "EmptyLabelOverrideProvider".to_owned(),
                        namespace: "own".to_owned(),
                        produced_at: jiff::Timestamp::UNIX_EPOCH,
                    },
                    cost: harw_lens_types::CostEstimate(4),
                    digest: harw_types::ContentDigest::of(body.as_bytes()),
                    body,
                }]
            })
        }
    }

    #[test]
    fn contribute_v2_override_is_not_touched_by_empty_label_normalization() {
        let fragments = block_on(EmptyLabelOverrideProvider.contribute_v2(&ctx()));

        assert_eq!(fragments.len(), 1);
        assert_eq!(
            fragments[0].label.as_str(),
            "own.label",
            "an overriding provider's own label must survive untouched, never becoming 'unlabeled'"
        );
    }

    /// Ein `ApprovalHandler`, der ausschließlich `review` implementiert --
    /// steht für jeden der fünf bestehenden Handler im Workspace, die
    /// `kind`/`label` noch nicht kennen (siehe Ledger).
    struct BareApprovalHandler;

    impl ApprovalHandler for BareApprovalHandler {
        fn review<'a>(&'a self, _call: &'a ToolCall) -> ExtFuture<'a, ApprovalDecision> {
            Box::pin(async { ApprovalDecision::Allow })
        }
    }

    #[test]
    fn approval_handler_kind_defaults_to_other() {
        assert_eq!(BareApprovalHandler.kind(), ApprovalHandlerKind::Other);
    }

    #[test]
    fn approval_handler_label_defaults_to_unnamed() {
        assert_eq!(BareApprovalHandler.label(), "unnamed");
    }

    /// Ein Handler, der beide Vorgaben überschreibt -- belegt, dass die
    /// Vorgaben echte Defaults sind, keine erzwungenen Endwerte.
    struct NamedApprovalHandler;

    impl ApprovalHandler for NamedApprovalHandler {
        fn review<'a>(&'a self, _call: &'a ToolCall) -> ExtFuture<'a, ApprovalDecision> {
            Box::pin(async { ApprovalDecision::Allow })
        }

        fn kind(&self) -> ApprovalHandlerKind {
            ApprovalHandlerKind::Interactive
        }

        fn label(&self) -> &'static str {
            "named-handler"
        }
    }

    #[test]
    fn approval_handler_kind_and_label_can_be_overridden() {
        assert_eq!(
            NamedApprovalHandler.kind(),
            ApprovalHandlerKind::Interactive
        );
        assert_eq!(NamedApprovalHandler.label(), "named-handler");
    }
}
