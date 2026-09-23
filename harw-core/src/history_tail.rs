//! `history.tail` als `DetailMode::References`-fähige Sektion (Knoten AW5-07).
//!
//! # Verantwortungsbereich
//! Besitzt [`render_history_tail`] und [`HistoryTailRender`]: die Übersetzung
//! einer [`crate::history::ConversationHistory`] in eine Liste von
//! [`harw_context::Fragment`]s für die Sektion `"history.tail"`, bei der ein
//! garantierter, immer vollständig gerenderter Schwanz mit älteren, zu
//! [`harw_context::FragmentReference`]s verkürzten Gruppen kombiniert wird.
//!
//! # Warum die Historie der wichtigste Anwendungsfall für Verweise ist
//! Die Konversationshistorie ist der größte Kontextposten und wächst
//! unbegrenzt über die Lebenszeit einer Sitzung — genau die Eigenschaft, für
//! die ein Verweis (`harw_context::DetailMode::References`,
//! `harw_context::FragmentReference`) gebaut wurde: alte Turns kosten dann
//! nur noch die Größe ihres Verweises, nicht mehr ihren vollen Text, bleiben
//! aber über `context.load` (`harw_tools::context_load`) nachladbar.
//!
//! # Wie viel Ende immer da ist, und warum
//! [`HISTORY_TAIL_GUARANTEED_GROUPS`] Gruppen (siehe dort für die Zählweise)
//! werden **immer** vollständig gerendert, unabhängig vom Budget. Das ist
//! eine bewusste Entscheidung gegen zwei Fehlrichtungen:
//! - **Zu kurz** erzeugt einen Agenten, der sich nach praktisch jeder Antwort
//!   selbst über `context.load` nachladen muss, um den unmittelbaren
//!   Gesprächsfaden wiederherzustellen — teurer (ein zusätzlicher
//!   Werkzeugaufruf plus dessen eigene Latenz) als das, was die Referenz
//!   ursprünglich sparen sollte.
//! - **Zu lang** unterläuft den Sinn dieses Knotens: praktisch die gesamte
//!   Historie bliebe voll gerendert, und Verweise kämen nie zum Tragen.
//!
//! Der gewählte Wert deckt in der weitaus häufigsten Form eines
//! Antwort-Zyklus — Nutzer-Nachricht, ein bis zwei Werkzeug-Aufruf/Ergebnis-
//! Paare, abschließende Assistenten-Antwort — den vollständigen zuletzt
//! abgeschlossenen Austausch ab, ohne die Historie strukturell zu
//! unterscheiden (kein Blick auf Rollen oder Sitzungsgrenzen nötig). Diese
//! Zahl ist bewusst ein einfacher, dokumentierter Kompromiss, kein aus einer
//! harten Anforderung abgeleiteter Wert — ein späterer Knoten darf sie mit
//! Messdaten neu justieren.
//!
//! # Was zu einem Verweis wird, und was nicht
//! Jede atomare Gruppe (siehe
//! [`crate::history::ConversationHistory::atomic_groups`] — ein einzelnes
//! Item, außer ein `ToolResult` folgt unmittelbar auf seinen `ToolCall`, dann
//! beide zusammen) jenseits des garantierten Schwanzes wird zu einem
//! [`harw_context::FragmentReference`] verkürzt: Sektion, Label, Vertrauen,
//! Beständigkeit, Herkunft und volle Kosten bleiben erhalten, der Rumpf
//! entfällt zugunsten der Verweis-Darstellung (`FragmentReference::Display`).
//! `Reasoning`- und `Error`-Items werden — wie schon in
//! [`crate::history::ConversationHistory::to_model_messages`] — grundsätzlich
//! ausgelassen: Surface-Metadaten, kein Modell-Input.
//!
//! **Nur, wenn es tatsächlich billiger ist.** Ein Verweis trägt Fixkosten
//! (Label-UUID, 64-Zeichen-Digest, Zeitstempel — siehe
//! `harw_context::reference`s Moduldoku, Abschnitt „Fixkosten eines
//! Verweises"). Für eine sehr kurze Gruppe (eine Zwei-Wort-Nachricht) kann
//! diese Darstellung teurer sein als ihr eigener Rumpf. `render_history_tail`
//! vergleicht deshalb `estimator.estimate(&verweistext)` gegen
//! `estimator.estimate(&rumpf)` und verkürzt eine Gruppe nur, wenn der
//! Verweis tatsächlich weniger kostet — sonst bliebe die Gruppe zwar
//! „verkürzt", aber teurer als zuvor, und das Budget würde durch die
//! Verkürzung selbst schlechter statt besser ausgenutzt.
//!
//! # Vertrauensklasse: nie höher als bei regulärem Rendern
//! Jedes hier erzeugte Fragment trägt [`harw_context::TrustClass::Evidence`]
//! — nie [`harw_context::TrustClass::Instruction`]. Konversationsinhalt
//! (Nutzer-Text, Assistenten-Text, Werkzeugaufrufe und -ergebnisse) ist per
//! Definition kein Betreiber-Instruktionstext (siehe
//! `harw_core::context_budget`, Abschnitt „AW4-01"). Diese Konstante gilt
//! unabhängig davon, ob ein Fragment vollständig gerendert oder zu einem
//! Verweis verkürzt wurde — ein über `context.load` nachgeladenes Fragment
//! trägt dieselbe Vertrauensklasse wie sein vollständig gerendertes Gegenstück
//! und kann deshalb nie in einem vertrauenswürdigeren Block landen, als es
//! beim regulären Rendern hätte tun können.
//!
//! # Keine Systemuhr in dieser Datei
//! [`render_history_tail`] nimmt den Renderzeitpunkt (`rendered_at`) als
//! Parameter entgegen, statt ihn selbst über `jiff::Timestamp::now()` zu
//! ermitteln — dieselbe Zusicherung wie überall sonst im Bibliothekscode
//! dieses Projekts: dieselbe Historie und derselbe Zeitpunkt ergeben
//! deterministisch dasselbe Ergebnis, beliebig oft wiederholbar (siehe den
//! Test `test_render_history_tail_is_deterministic` weiter unten).
//!
//! # Exportierte Typen
//! [`HistoryTailRender`], [`HISTORY_TAIL_GUARANTEED_GROUPS`],
//! [`render_history_tail`].
//!
//! # Nebenläufigkeit
//! [`render_history_tail`] ist eine reine Funktion ohne innere
//! Veränderlichkeit; `Send + Sync` an jedem Aufrufort, sofern der übergebene
//! `estimator` es ist (siehe `harw_lens_types::CostEstimator`).
//!
//! # Fehler
//! [`harw_context::ContextError`], wenn die Sektionskonstante `"history.tail"`
//! ungültig wäre — strukturell unerreichbar (das Literal enthält weder
//! Steuerzeichen noch ist es leer), aber ehrlich als `Result` statt über
//! `unwrap`/`expect` an der Aufrufstelle behandelt.
//!
//! # Anschluss an die Montage (bewusst offen)
//! Diese Datei liefert `Vec<Fragment>`, keine fertige Prompt-Sektion: der
//! Anschluss an `Assembly::gather` (dieses Modul, `context_budget.rs`) sowie
//! die Frage, wie `loadable` tatsächlich in einen `context.load`-fähigen
//! Store (`harw_tools::context_load::ReferenceStore`) einer laufenden Sitzung
//! einfließt, bleibt ein Folge-Knoten — siehe den Abschlussbericht dieses
//! Knotens. `render_history_tail` selbst ist bereits vollständig durch
//! [`crate::context_budget::Assembly::gather`] konsumierbar, weil sein
//! Rückgabetyp exakt `Vec<harw_context::Fragment>` ist.

use crate::history::ConversationHistory;
use harw_context::{
    Fragment, FragmentLabel, FragmentOrigin, FragmentReference, SectionName, Stability, TrustClass,
};
use harw_lens_types::CostEstimator;
use harw_protocol::items::TurnItem;
use harw_types::ItemId;

/// Sektionsname für die Konversationshistorie, unter dem alle hier erzeugten
/// Fragmente stehen.
pub const HISTORY_TAIL_SECTION: &str = "history.tail";

/// Anzahl der jüngsten atomaren Gruppen, die **immer** vollständig gerendert
/// werden — unabhängig vom Budget. Siehe die Moduldoku, Abschnitt „Wie viel
/// Ende immer da ist, und warum", für die vollständige Begründung dieser
/// Zahl.
pub const HISTORY_TAIL_GUARANTEED_GROUPS: usize = 4;

/// Herkunfts-Provider-Name für alle von dieser Datei erzeugten Fragmente.
const HISTORY_ORIGIN_PROVIDER: &str = "harw-core::history";

/// Herkunfts-Namespace für alle von dieser Datei erzeugten Fragmente.
const HISTORY_ORIGIN_NAMESPACE: &str = "conversation";

/// Ergebnis von [`render_history_tail`]: was in die Montage einfließt, plus
/// was ein `context.load`-fähiger Store aus dieser Historie kennen müsste.
///
/// # Description
/// `fragments` ist die vollständige Sektion `"history.tail"` — ein Mix aus
/// vollständig gerenderten (garantierter Schwanz) und zu
/// [`harw_context::FragmentReference`]s verkürzten (ältere Gruppen)
/// Fragmenten, direkt konsumierbar durch
/// [`crate::context_budget::Assembly::gather`]. `loadable` trägt für jede
/// verkürzte Gruppe das **vollständige** Fragment (Originalrumpf, volle
/// Kosten, identische Sektion/Label) — der Rohstoff, aus dem ein
/// `harw_tools::context_load::ReferenceStore` gebaut würde, um dieselben
/// Sektions-/Label-Paare später aufzulösen.
#[derive(Debug, Clone, PartialEq)]
pub struct HistoryTailRender {
    /// Die Sektion `"history.tail"`, bereit für die Montage.
    pub fragments: Vec<Fragment>,
    /// Die vollständigen Fragmente hinter jeder verkürzten Gruppe in
    /// `fragments`, adressiert über dasselbe `(section, label)`-Paar.
    pub loadable: Vec<Fragment>,
}

/// Rendert `history` als `"history.tail"`-Sektion mit garantiertem Schwanz
/// und nachladbaren Verweisen für ältere Gruppen.
///
/// # Description
/// Zerlegt `history` über
/// [`crate::history::ConversationHistory::atomic_groups`] in atomare
/// Gruppen (siehe Moduldoku), lässt `Reasoning`- und `Error`-Gruppen aus,
/// und rendert die jüngsten [`HISTORY_TAIL_GUARANTEED_GROUPS`] Gruppen
/// vollständig sowie jede ältere Gruppe als [`harw_context::FragmentReference`]
/// (Rumpf ersetzt durch dessen `Display`-Darstellung, Kosten neu über
/// `estimator` gegen diesen kürzeren Text geschätzt). Jedes erzeugte
/// Fragment trägt [`harw_context::TrustClass::Evidence`] (siehe Moduldoku,
/// Abschnitt „Vertrauensklasse").
///
/// # Arguments
/// - `history` (`&ConversationHistory`): die zu rendernde Historie.
/// - `rendered_at` (`jiff::Timestamp`): der Zeitpunkt, der als
///   `FragmentOrigin::produced_at` für jedes erzeugte Fragment verwendet
///   wird. Vom Aufrufer zu liefern (siehe Moduldoku, „Keine Systemuhr").
/// - `estimator` (`&dyn CostEstimator`): schätzt die Kosten sowohl des
///   vollen Rumpfes als auch der (kürzeren) Verweis-Darstellung.
///
/// # Returns
/// `Ok(HistoryTailRender)` mit der gerenderten Sektion und den
/// vollständigen Fragmenten hinter jedem Verweis.
///
/// # Errors
/// [`harw_context::ContextError`], falls die interne Sektionskonstante
/// `"history.tail"` einmal ungültig würde — strukturell unerreichbar, aber
/// ehrlich propagiert statt über `unwrap`/`expect` verschwiegen.
///
/// # Examples
/// ```rust
/// use harw_core::history::ConversationHistory;
/// use harw_core::history_tail::render_history_tail;
/// use harw_lens_types::BytesOverFour;
///
/// let mut history = ConversationHistory::new();
/// history.push_user_text("hello");
/// history.push_assistant_text("hi there", None);
///
/// let rendered = render_history_tail(&history, jiff::Timestamp::UNIX_EPOCH, &BytesOverFour)
///     .expect("the literal section name always validates");
/// assert_eq!(rendered.fragments.len(), 2);
/// assert!(rendered.loadable.is_empty(), "a two-group history fits entirely in the guaranteed tail");
/// ```
pub fn render_history_tail(
    history: &ConversationHistory,
    rendered_at: jiff::Timestamp,
    estimator: &dyn CostEstimator,
) -> Result<HistoryTailRender, harw_context::ContextError> {
    let section = SectionName::try_new(HISTORY_TAIL_SECTION)?;

    let renderable: Vec<(FragmentLabel, String)> = history
        .atomic_groups()
        .into_iter()
        .filter_map(|group| {
            let label = group_label(&group)?;
            let body = render_group_body(&group)?;
            Some((label, body))
        })
        .collect();

    let total = renderable.len();
    let guaranteed_from = total.saturating_sub(HISTORY_TAIL_GUARANTEED_GROUPS);

    let mut fragments = Vec::with_capacity(total);
    let mut loadable = Vec::new();

    for (index, (label, body)) in renderable.into_iter().enumerate() {
        let is_guaranteed = index >= guaranteed_from;
        let full_cost = estimator.estimate(&body);
        let digest = harw_types::ContentDigest::of(body.as_bytes());
        // Der garantierte Schwanz ist `Fresh` (jüngst produziert, noch nicht
        // abgelegt); eine zum Verweis verkürzte Gruppe ist `Stable`
        // (abgeschlossene, unveränderliche Historie) — dieselbe Unterscheidung
        // in der Größenordnung, die `Assembly::budget`s Stabilitäts-Rang beim
        // Sortieren trifft.
        let stability = if is_guaranteed {
            Stability::Fresh
        } else {
            Stability::Stable
        };
        let full = Fragment {
            label,
            section: section.clone(),
            trust: TrustClass::Evidence,
            stability,
            origin: FragmentOrigin {
                provider: HISTORY_ORIGIN_PROVIDER.to_owned(),
                namespace: HISTORY_ORIGIN_NAMESPACE.to_owned(),
                produced_at: rendered_at,
            },
            cost: full_cost,
            digest,
            body,
        };

        if is_guaranteed {
            fragments.push(full);
        } else {
            let reference = FragmentReference::from_fragment(&full);
            let reference_body = reference.to_string();
            let reference_cost = estimator.estimate(&reference_body);

            // Ein Verweis trägt Fixkosten (Label-UUID, 64-Zeichen-Digest,
            // Zeitstempel — siehe `harw_context::reference`s Moduldoku,
            // Abschnitt „Fixkosten eines Verweises"). Für eine sehr kurze
            // Gruppe kann er dadurch teurer sein als ihr eigener Rumpf; in
            // dem Fall bleibt die Gruppe vollständig, statt eine
            // Verkürzung vorzutäuschen, die keine ist.
            if reference_cost.0 < full.cost.0 {
                let mut reference_fragment = full.clone();
                reference_fragment.cost = reference_cost;
                reference_fragment.body = reference_body;

                fragments.push(reference_fragment);
                loadable.push(full);
            } else {
                fragments.push(full);
            }
        }
    }

    Ok(HistoryTailRender {
        fragments,
        loadable,
    })
}

/// Extrahiert die `ItemId` eines beliebigen `TurnItem`, unabhängig von der
/// Variante — jede Variante trägt ein `id`-Feld.
fn item_id(item: &TurnItem) -> &ItemId {
    match item {
        TurnItem::UserMessage(m) => &m.id,
        TurnItem::AssistantMessage(m) => &m.id,
        TurnItem::ToolCall(c) => &c.id,
        TurnItem::ToolResult(r) => &r.id,
        TurnItem::Reasoning(r) => &r.id,
        TurnItem::Error(e) => &e.id,
    }
}

/// Leitet das Fragment-Label einer Gruppe aus der `ItemId` ihres ersten
/// Items ab.
///
/// # Returns
/// `None`, wenn `group` leer ist (strukturell unerreichbar — jede von
/// [`crate::history::ConversationHistory::atomic_groups`] gelieferte Gruppe
/// enthält mindestens ein Item) oder wenn die `ItemId` wider Erwarten keinen
/// gültigen [`FragmentLabel`] ergibt. `ItemId`-Werte sind stets nicht-leere,
/// steuerzeichenfreie UUID-Strings (`harw_types::ids`), daher ist der
/// zweite Fall in der Praxis ebenfalls unerreichbar — diese Funktion
/// entscheidet sich trotzdem für `None` statt `unwrap`/`expect`, damit ein
/// künftiger, tatsächlich abweichender `ItemId`-Konstruktor nicht zum Panic
/// wird.
fn group_label(group: &[&TurnItem]) -> Option<FragmentLabel> {
    let anchor = group.first()?;
    FragmentLabel::try_new(item_id(anchor).as_str()).ok()
}

/// Rendert eine atomare Gruppe zu ihrem Fließtext-Rumpf.
///
/// # Returns
/// `Some(text)` für jede Gruppe mit Modell-relevantem Inhalt; `None` für eine
/// reine `Reasoning`- oder `Error`-Gruppe (siehe Moduldoku, Abschnitt „Was
/// zu einem Verweis wird, und was nicht" — dieselbe Auslassung wie in
/// [`crate::history::ConversationHistory::to_model_messages`]).
fn render_group_body(group: &[&TurnItem]) -> Option<String> {
    let mut lines = Vec::with_capacity(group.len());
    for item in group {
        let line = match item {
            TurnItem::UserMessage(m) => format!("user: {}", flatten(&m.content)),
            TurnItem::AssistantMessage(m) => format!("assistant: {}", flatten(&m.content)),
            TurnItem::ToolCall(c) => format!("tool_call {}({})", c.tool_name, c.arguments),
            TurnItem::ToolResult(r) => format!(
                "tool_result[{}]: {}",
                r.call_id,
                serde_json::to_string(&r.result)
                    .unwrap_or_else(|_| "<unserializable tool result>".to_owned()),
            ),
            TurnItem::Reasoning(_) | TurnItem::Error(_) => return None,
        };
        lines.push(line);
    }
    Some(lines.join("\n"))
}

/// Reduziert `ContentPart`s auf reinen Text — dieselbe Regel wie
/// [`crate::history::ConversationHistory::to_model_messages`] (Bilder werden
/// zu einem `[image]`-Platzhalter).
fn flatten(parts: &[harw_protocol::items::ContentPart]) -> String {
    use harw_protocol::items::ContentPart;
    let mut buf = String::new();
    for part in parts {
        match part {
            ContentPart::Text { text } => buf.push_str(text),
            ContentPart::ImageUrl { .. } => buf.push_str("[image]"),
        }
    }
    buf
}

#[cfg(test)]
mod tests {
    use super::{HISTORY_TAIL_GUARANTEED_GROUPS, render_history_tail};
    use crate::history::ConversationHistory;
    use crate::test_support::{TestError, TestResult, ctx};
    use harw_context::TrustClass;
    use harw_lens_types::BytesOverFour;
    use harw_protocol::ToolCallResult;
    use harw_types::ToolCallId;

    /// Füllsel, lang genug, dass eine Verweis-Darstellung
    /// (`FragmentReference::Display`, Fixkosten aus UUID-Label, 64-Zeichen-
    /// Digest und Zeitstempel — siehe `harw_context::reference`s Moduldoku)
    /// nachweislich billiger bleibt als der volle Rumpf. Ohne dieses Füllsel
    /// wären die kurzen Testnachrichten selbst schon billiger als jeder
    /// Verweis auf sie — dann bliebe (korrekterweise) alles vollständig, und
    /// die Verweis-Tests dieser Datei hätten nichts zu prüfen.
    const LONG_FILLER: &str = "lorem ipsum dolor sit amet consectetur adipiscing elit sed do eiusmod \
         tempor incididunt ut labore et dolore magna aliqua ";

    /// Baut eine Historie aus `n` User/Assistant-Austauschen
    /// (2 Gruppen je Austausch: eine User-Nachricht, eine Assistenten-Antwort),
    /// mit genug Text je Nachricht, dass eine Verkürzung zum Verweis
    /// tatsächlich lohnt (siehe [`LONG_FILLER`]).
    fn history_with_exchanges(n: usize) -> ConversationHistory {
        let mut history = ConversationHistory::new();
        let filler = LONG_FILLER.repeat(3);
        for i in 0..n {
            history.push_user_text(format!("question {i} {filler}"));
            history.push_assistant_text(format!("answer {i} {filler}"), None);
        }
        history
    }

    #[test]
    fn test_render_history_tail_short_history_has_no_references() -> TestResult {
        // 2 Austausche = 4 Gruppen = genau der garantierte Schwanz.
        let history = history_with_exchanges(2);
        let rendered = render_history_tail(&history, jiff::Timestamp::UNIX_EPOCH, &BytesOverFour)
            .map_err(ctx("literal section name always validates"))?;

        assert_eq!(rendered.fragments.len(), 4);
        assert!(rendered.loadable.is_empty());
        Ok(())
    }

    #[test]
    fn test_render_history_tail_guarantees_the_last_n_groups_verbatim() -> TestResult {
        // 6 Austausche = 12 Gruppen; die letzten HISTORY_TAIL_GUARANTEED_GROUPS
        // müssen wörtlich den vollen Text tragen.
        let history = history_with_exchanges(6);
        let rendered = render_history_tail(&history, jiff::Timestamp::UNIX_EPOCH, &BytesOverFour)
            .map_err(ctx("literal section name always validates"))?;

        let guaranteed =
            &rendered.fragments[rendered.fragments.len() - HISTORY_TAIL_GUARANTEED_GROUPS..];
        assert!(
            guaranteed.iter().any(|f| f.body.contains("answer 5")),
            "the most recent answer must be verbatim in the guaranteed tail"
        );
        for fragment in guaranteed {
            assert!(
                !fragment.body.starts_with("[ref]"),
                "a guaranteed-tail fragment must never be a reference: {}",
                fragment.body
            );
        }
        Ok(())
    }

    #[test]
    fn test_render_history_tail_older_groups_become_cheaper_references() -> TestResult {
        let history = history_with_exchanges(6);
        let rendered = render_history_tail(&history, jiff::Timestamp::UNIX_EPOCH, &BytesOverFour)
            .map_err(ctx("literal section name always validates"))?;

        assert_eq!(
            rendered.loadable.len(),
            rendered.fragments.len() - HISTORY_TAIL_GUARANTEED_GROUPS
        );

        let oldest_reference = &rendered.fragments[0];
        let oldest_full = &rendered.loadable[0];
        assert!(oldest_reference.body.starts_with("[ref]"));
        assert_eq!(oldest_reference.label, oldest_full.label);
        assert_eq!(oldest_reference.section, oldest_full.section);
        assert_eq!(oldest_reference.digest, oldest_full.digest);
        assert!(
            oldest_reference.cost.0 < oldest_full.cost.0,
            "reference cost ({:?}) must be cheaper than the full cost ({:?})",
            oldest_reference.cost,
            oldest_full.cost
        );
        Ok(())
    }

    #[test]
    fn test_render_history_tail_tool_call_result_pair_stays_one_fragment() -> TestResult {
        let mut history = ConversationHistory::new();
        history.push_user_text("run the tests");
        let call_id = ToolCallId::new();
        history.push_tool_call(call_id.clone(), "cargo.test", serde_json::json!({}));
        history.push_tool_result(
            call_id,
            ToolCallResult::success(serde_json::json!({"ok": true})),
            5,
        );
        history.push_assistant_text("tests pass", None);

        let rendered = render_history_tail(&history, jiff::Timestamp::UNIX_EPOCH, &BytesOverFour)
            .map_err(ctx("literal section name always validates"))?;

        // 3 Gruppen: user, (call+result), assistant.
        assert_eq!(rendered.fragments.len(), 3);
        let pair = rendered
            .fragments
            .iter()
            .find(|f| f.body.contains("tool_call") && f.body.contains("tool_result"))
            .ok_or(TestError::Missing(
                "the call/result pair must be rendered as a single fragment",
            ))?;
        assert!(pair.body.contains("cargo.test"));
        Ok(())
    }

    #[test]
    fn test_render_history_tail_all_fragments_are_evidence_trust() -> TestResult {
        let history = history_with_exchanges(6);
        let rendered = render_history_tail(&history, jiff::Timestamp::UNIX_EPOCH, &BytesOverFour)
            .map_err(ctx("literal section name always validates"))?;

        for fragment in rendered.fragments.iter().chain(rendered.loadable.iter()) {
            assert_eq!(
                fragment.trust,
                TrustClass::Evidence,
                "a history fragment must never claim Instruction trust, referenced or not"
            );
        }
        Ok(())
    }

    #[test]
    fn test_render_history_tail_is_deterministic() -> TestResult {
        let history = history_with_exchanges(6);
        let a = render_history_tail(&history, jiff::Timestamp::UNIX_EPOCH, &BytesOverFour)?;
        let b = render_history_tail(&history, jiff::Timestamp::UNIX_EPOCH, &BytesOverFour)?;

        assert_eq!(a, b);
        Ok(())
    }

    /// Determinismus gilt auch für die interne Reihenfolge: zwei Aufrufe mit
    /// identischer Eingabe liefern nicht nur gleiche Fragmente, sondern in
    /// gleicher Reihenfolge — wichtig, weil `Assembly::budget` diese
    /// Reihenfolge zwar neu sortiert, aber Tests darauf verlassen können
    /// müssen, dass hier keine zufällige Variation hereinkommt.
    #[test]
    fn test_render_history_tail_fragment_order_is_stable_across_calls() -> TestResult {
        let history = history_with_exchanges(4);
        let a = render_history_tail(&history, jiff::Timestamp::UNIX_EPOCH, &BytesOverFour)?;
        let b = render_history_tail(&history, jiff::Timestamp::UNIX_EPOCH, &BytesOverFour)?;

        let labels_a: Vec<_> = a.fragments.iter().map(|f| f.label.clone()).collect();
        let labels_b: Vec<_> = b.fragments.iter().map(|f| f.label.clone()).collect();
        assert_eq!(labels_a, labels_b);
        Ok(())
    }

    #[test]
    fn test_render_history_tail_empty_history_yields_empty_render() -> TestResult {
        let history = ConversationHistory::new();
        let rendered = render_history_tail(&history, jiff::Timestamp::UNIX_EPOCH, &BytesOverFour)
            .map_err(ctx("literal section name always validates"))?;

        assert!(rendered.fragments.is_empty());
        assert!(rendered.loadable.is_empty());
        Ok(())
    }
}
