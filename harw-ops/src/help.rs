//! `/help`-Operation für das Harwness-System.
//!
//! # Verantwortungsbereich
//! Dieses Modul implementiert den `/help`-Slash-Command (`HelpOperation`).
//! Es listet alle in der [`harw_operations::registry::OperationRegistry`]
//! registrierten Operationen nach [`harw_operations::OperationCategory`]
//! gruppiert auf — mit Name, Aliassen und Kurzbeschreibung je Kategorie.
//! Ohne Filter werden alle Command-exponierten Operationen angezeigt;
//! mit optionalem [`HelpArgs::filter`] werden nur passende Namen gezeigt.
//!
//! # Render-Kontrakt
//! Ausgabe-Struktur:
//! ```text
//! Model:
//!   /provider — ...
//!   /model    — ...
//!
//! Agent:
//!   /agent — ...
//! ```
//! Kategoriereihenfolge: Model → Agent → Session → System → Knowledge → Misc.
//! Innerhalb jeder Kategorie sind Operationen alphabetisch nach Name sortiert.
//!
//! # Schlüsseltypen
//! - [`HelpArgs`] — optionaler Filter-Parameter (Substring-Suche, case-insensitiv).
//! - [`HelpOperation`] — generierter Unit-Struct (via `#[operation]`-Makro).
//!
//! # Nebenläufigkeit
//! `HelpOperation` ist `Send + Sync` (Unit-Struct ohne inneren Zustand).
//! Die Registry wird per `ctx.service::<OperationRegistry>()` als shared reference
//! abgerufen — kein Lock erforderlich.
//!
//! # Fehlertypen
//! - [`harw_operations::OpError::NotAvailable`]: Registry nicht im `ServiceMap` registriert.
//!
//! # Beispiel
//! ```rust,no_run
//! // Die Operation wird vom Adapter via `Operation::run` aufgerufen.
//! // Direkte Nutzung über die generierte `HelpOperation`-Struct:
//! // let op = HelpOperation;
//! // let meta = op.meta();
//! // assert_eq!(meta.name, "help");
//! ```

use std::collections::HashMap;

use harw_macros::operation;
use harw_operations::{OpContext, OpError, OpOutput, OperationCategory, Surface};

// ── Args ──────────────────────────────────────────────────────────────────────

/// Argumente für die `/help`-Operation.
///
/// # Beschreibung
/// Alle Felder sind optional; ohne Argumente werden alle registrierten Command-
/// Operationen nach Kategorie gruppiert aufgelistet. Mit [`HelpArgs::filter`]
/// kann die Ausgabe auf Operationen eingeschränkt werden, deren Name den
/// angegebenen Substring enthält (Vergleich: case-insensitiv).
///
/// # Deserialiserung
/// Das `#[operation]`-Makro deserialisiert `input.json_args` via `serde_json` in
/// diesen Typ. Bei `json_args == null` wird [`Default::default`] verwendet, was
/// `filter = None` ergibt (alle Ops werden angezeigt).
///
/// # Beispiel
/// ```rust
/// use harw_ops::help::HelpArgs;
///
/// let args: HelpArgs = serde_json::from_str(r#"{"filter": "stat"}"#).unwrap();
/// assert_eq!(args.filter.as_deref(), Some("stat"));
///
/// let default_args = HelpArgs::default();
/// assert!(default_args.filter.is_none());
/// ```
#[derive(Default, serde::Deserialize, harw_macros::FromRawArgs)]
pub struct HelpArgs {
    /// Optionaler Filter: nur Ops zeigen, deren Name diesen Substring enthält.
    ///
    /// Der Vergleich erfolgt case-insensitiv (beide Seiten werden vor dem Vergleich
    /// in Kleinbuchstaben konvertiert via [`str::to_lowercase`]).
    #[serde(default)]
    #[raw(first)]
    pub filter: Option<String>,
}

// ── Hilfsfunktionen ───────────────────────────────────────────────────────────

/// Gibt die kanonische Render-Reihenfolge der Kategorien zurück.
///
/// # Beschreibung
/// Deterministisch: Model → Agent → Session → System → Knowledge → Misc.
/// Entspricht dem `/help`-Render-Kontrakt (Spec § Neuer Render-Kontrakt).
///
/// # Rückgabe
/// Ein statisches Slice aller [`OperationCategory`]-Varianten in Anzeigereihenfolge.
fn category_order() -> &'static [OperationCategory] {
    &[
        OperationCategory::Model,
        OperationCategory::Agent,
        OperationCategory::Session,
        OperationCategory::System,
        OperationCategory::Knowledge,
        OperationCategory::Misc,
    ]
}

/// Gibt die kapitalisierte Form eines Strings zurück (erstes Zeichen groß).
///
/// # Argumente
/// - `s` (`&str`): Eingabe-String.
///
/// # Rückgabe
/// Neuer `String` mit erstem Zeichen in Großbuchstaben, Rest unverändert.
fn capitalize(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) => c.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// Baut den gruppierten Hilfetext aus einem Iterator über Operationen.
///
/// # Beschreibung
/// Filtert auf Command-exponierte Operationen, wendet den optionalen Namens-
/// filter an, gruppiert nach [`OperationCategory`] und sortiert innerhalb
/// jeder Kategorie alphabetisch. Gibt einen fertigen, mehrzeiligen String zurück.
///
/// # Argumente
/// - `ops` — Iterator über Operationen; jedes Element implementiert
///   [`harw_operations::Operation`] via `Arc<dyn Operation>`.
/// - `filter_lower` — Optional lowercase Substring-Filter.
///
/// # Rückgabe
/// Fertiger Hilfetext mit Kategorieüberschriften und Einrückung.
/// One entry per visible command: (name, aliases, summary).
type CommandEntry = (&'static str, &'static [&'static str], &'static str);

fn build_grouped_help<'a>(
    ops: impl Iterator<Item = &'a std::sync::Arc<dyn harw_operations::Operation>>,
    filter_lower: Option<&str>,
) -> String {
    let mut buckets: HashMap<OperationCategory, Vec<CommandEntry>> = HashMap::new();

    for op in ops {
        let meta = op.meta();

        // Nur Command-exponierte Operationen anzeigen.
        let has_command_surface = meta
            .surfaces
            .iter()
            .any(|s| matches!(s, Surface::Command { .. }));
        if !has_command_surface {
            continue;
        }

        // Optionaler Name-Filter (case-insensitiv).
        if let Some(f) = filter_lower {
            if !meta.name.to_lowercase().contains(f) {
                continue;
            }
        }

        buckets
            .entry(meta.category)
            .or_default()
            .push((meta.name, meta.aliases, meta.summary));
    }

    let mut lines: Vec<String> = Vec::new();

    for &category in category_order() {
        let Some(entries) = buckets.get(&category) else {
            continue;
        };
        if entries.is_empty() {
            continue;
        }

        lines.push(format!("{}:", capitalize(category.as_str())));

        let mut sorted = entries.clone();
        sorted.sort_by_key(|(name, _, _)| *name);

        for (name, aliases, summary) in sorted {
            let alias_suffix = if aliases.is_empty() {
                String::new()
            } else {
                format!(" (aliases: {})", aliases.join(", "))
            };
            lines.push(format!("  /{name}{alias_suffix} — {summary}"));
        }

        lines.push(String::new());
    }

    // Abschließende Leerzeile entfernen.
    if lines.last().is_some_and(|s| s.is_empty()) {
        lines.pop();
    }

    lines.join("\n")
}

// ── Operation ─────────────────────────────────────────────────────────────────

/// Listet alle registrierten `/`-Befehle nach Kategorie gruppiert auf.
///
/// # Beschreibung
/// Holt die [`harw_operations::registry::OperationRegistry`] aus dem
/// `ServiceMap` des Kontexts und iteriert über alle registrierten Ops.
/// Nur Ops mit mindestens einer `Surface::Command`-Deklaration erscheinen
/// in der Ausgabe. Die Gruppierung erfolgt nach [`OperationCategory`] in
/// der kanonischen Reihenfolge: Model → Agent → Session → System → Knowledge → Misc.
/// Innerhalb jeder Kategorie sind Operationen alphabetisch nach Name sortiert.
/// Name, Aliasse und Summary werden je Zeile ausgegeben.
///
/// Falls [`HelpArgs::filter`] gesetzt ist, werden nur Ops eingeschlossen,
/// deren Name den Filter-Substring enthält (case-insensitiver Vergleich).
///
/// # Argumente
/// - `ctx` (`&OpContext`): Ausführungskontext; muss eine
///   [`harw_operations::registry::OperationRegistry`] in der `ServiceMap` enthalten.
/// - `args` (`HelpArgs`): Optionaler Filter-Substring.
///
/// # Rückgabe
/// - `Ok(OpOutput)`: Gruppierte Auflistung aller (gefilterten) Command-Ops.
///   Leer wenn keine Ops registriert sind oder kein Filter-Match existiert.
/// - `Err(OpError::NotAvailable)`: Registry nicht im `ServiceMap` vorhanden.
///
/// # Fehler
/// - [`OpError::NotAvailable`]: wenn `ctx.service::<OperationRegistry>()` `None` zurückgibt.
///
/// # Nebenläufigkeit
/// Liest die Registry nur via shared reference; keine Locks erforderlich.
///
/// # Beispiel
/// ```rust,no_run
/// // Wird intern vom Operation-Adapter via `HelpOperation::run(ctx, input)` aufgerufen.
/// ```
#[operation(
    name = "help",
    summary = "Listet alle verfügbaren /-Befehle mit Zusammenfassung.",
    domain = "misc",
    permission = "observer",
    command(path = "/help", visibility = "channel_parity"),
    // Web-Fläche: reine Auflistung der registrierten Commands, kein
    // Seiteneffekt möglich (die Operation liest nur `ctx.registry()` und
    // formatiert Text) — deshalb `method = "get"`. `approval = "none"`, weil
    // ein rein lesender Aufruf keine Bestätigung braucht.
    web(path = "/api/help", method = "get", approval = "none")
)]
async fn help(ctx: &OpContext, args: HelpArgs) -> Result<OpOutput, OpError> {
    let registry = ctx
        .service::<harw_operations::registry::OperationRegistry>()
        .ok_or_else(|| OpError::NotAvailable("Registry nicht verfügbar".to_owned()))?;

    let filter_lower = args.filter.as_deref().map(str::to_lowercase);

    let text = build_grouped_help(registry.iter(), filter_lower.as_deref());

    Ok(OpOutput::from(text))
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use crate::testutil::toks;
    use harw_operations::{
        ApprovalPolicy, CommandVisibility, FromRawArgs, OpContext, OpFuture, OpInput, OpOutput,
        Operation, OperationCategory, OperationDomain, OperationMeta, PermissionTier, Surface,
    };

    use super::{HelpArgs, build_grouped_help};

    // ── Test-Fixtures ─────────────────────────────────────────────────────────

    /// Erstellt eine minimale `Arc<dyn Operation>` mit gegebenen Feldern.
    fn make_op(
        name: &'static str,
        summary: &'static str,
        category: OperationCategory,
        aliases: &'static [&'static str],
        surfaces: Vec<Surface>,
    ) -> Arc<dyn Operation> {
        struct StubOp {
            name: &'static str,
            summary: &'static str,
            category: OperationCategory,
            aliases: &'static [&'static str],
            surfaces: Vec<Surface>,
        }

        impl Operation for StubOp {
            fn meta(&self) -> &OperationMeta {
                // Box::leak ist in Tests akzeptabel — jede StubOp-Instanz
                // ist ohnehin für die Lebensdauer des Tests alloziert.
                Box::leak(Box::new(OperationMeta {
                    name: self.name,
                    summary: self.summary,
                    domain: OperationDomain::Misc,
                    permission: PermissionTier::Observer,
                    surfaces: self.surfaces.clone(),
                    aliases: self.aliases,
                    category: self.category,
                    args_schema: None,
                    output_schema: None,
                }))
            }

            fn run<'a>(&'a self, _ctx: &'a OpContext, _input: OpInput) -> OpFuture<'a> {
                Box::pin(async { Ok(OpOutput::from(String::new())) })
            }
        }

        Arc::new(StubOp {
            name,
            summary,
            category,
            aliases,
            surfaces,
        })
    }

    /// Erstellt eine Op mit einem `Surface::Command`-Pfad.
    fn command_surface(path: &'static str) -> Surface {
        Surface::Command {
            path,
            visibility: CommandVisibility::ChannelParity,
        }
    }

    /// Erstellt eine Op mit nur ModelTool-Surface (kein Command).
    fn model_tool_surface() -> Surface {
        Surface::ModelTool {
            readonly: true,
            approval: ApprovalPolicy::None,
        }
    }

    // ── HelpArgs ──────────────────────────────────────────────────────────────

    #[test]
    fn test_help_args_from_raw_args_sets_filter() {
        let args = HelpArgs::from_raw_args(&toks(&["stat"]));
        match args {
            Ok(a) => assert_eq!(a.filter.as_deref(), Some("stat")),
            Err(e) => panic!("Unerwarteter Fehler: {e}"),
        }
    }

    #[test]
    fn test_help_args_from_raw_args_empty_tokens_sets_filter_none() {
        let args = HelpArgs::from_raw_args(&toks(&[]));
        match args {
            Ok(a) => assert!(a.filter.is_none()),
            Err(e) => panic!("Unerwarteter Fehler: {e}"),
        }
    }

    // ── build_grouped_help ────────────────────────────────────────────────────

    /// `test_help_output_contains_model_category_when_model_op_registered`
    ///
    /// Registriert eine Model-Op mit Command-Surface und prüft, dass die
    /// Ausgabe den Abschnitt "Model:" enthält.
    #[test]
    fn test_help_output_contains_model_category_when_model_op_registered() {
        let op = make_op(
            "model",
            "Zeigt oder wechselt das aktive Modell.",
            OperationCategory::Model,
            &[],
            vec![command_surface("/model")],
        );
        let registry: Vec<Arc<dyn Operation>> = vec![op];
        let text = build_grouped_help(registry.iter(), None);
        assert!(
            text.contains("Model:"),
            "Ausgabe muss 'Model:' enthalten, war:\n{text}"
        );
        assert!(
            text.contains("/model"),
            "Ausgabe muss '/model' enthalten, war:\n{text}"
        );
    }

    /// `test_help_output_orders_categories_deterministically`
    ///
    /// Prüft, dass Model-Kategorie vor System-Kategorie erscheint, wenn beide
    /// vertreten sind — unabhängig von der Registrierungsreihenfolge.
    #[test]
    fn test_help_output_orders_categories_deterministically() {
        let sys_op = make_op(
            "status",
            "System-Status.",
            OperationCategory::System,
            &[],
            vec![command_surface("/status")],
        );
        let model_op = make_op(
            "model",
            "Modell-Wechsel.",
            OperationCategory::Model,
            &[],
            vec![command_surface("/model")],
        );
        // System zuerst registriert — Model muss trotzdem früher erscheinen.
        let ops: Vec<Arc<dyn Operation>> = vec![sys_op, model_op];
        let text = build_grouped_help(ops.iter(), None);

        let model_pos = text
            .find("Model:")
            .expect("'Model:' muss in Ausgabe vorhanden sein");
        let system_pos = text
            .find("System:")
            .expect("'System:' muss in Ausgabe vorhanden sein");

        assert!(
            model_pos < system_pos,
            "Model: muss vor System: erscheinen; war:\n{text}"
        );
    }

    /// `test_help_output_shows_aliases_when_present`
    ///
    /// Prüft, dass Aliasse im Format "(aliases: r, reasoning)" erscheinen.
    #[test]
    fn test_help_output_shows_aliases_when_present() {
        let op = make_op(
            "effort",
            "Setzt den Reasoning-Effort.",
            OperationCategory::Model,
            &["r", "reasoning"],
            vec![command_surface("/effort")],
        );
        let ops: Vec<Arc<dyn Operation>> = vec![op];
        let text = build_grouped_help(ops.iter(), None);

        assert!(
            text.contains("(aliases: r, reasoning)"),
            "Ausgabe muss Aliasse enthalten, war:\n{text}"
        );
    }

    /// `test_help_output_skips_ops_without_command_surface`
    ///
    /// Prüft, dass eine Op mit nur ModelTool-Surface nicht in der Ausgabe erscheint.
    #[test]
    fn test_help_output_skips_ops_without_command_surface() {
        let tool_only = make_op(
            "internal-tool",
            "Internes Tool ohne Command.",
            OperationCategory::System,
            &[],
            vec![model_tool_surface()],
        );
        let cmd_op = make_op(
            "status",
            "System-Status.",
            OperationCategory::System,
            &[],
            vec![command_surface("/status")],
        );
        let ops: Vec<Arc<dyn Operation>> = vec![tool_only, cmd_op];
        let text = build_grouped_help(ops.iter(), None);

        assert!(
            !text.contains("internal-tool"),
            "ModelTool-only-Op darf nicht in Hilfe erscheinen, war:\n{text}"
        );
        assert!(
            text.contains("/status"),
            "Command-Op muss in Hilfe erscheinen, war:\n{text}"
        );
    }

    /// `test_help_output_sorts_commands_alphabetically_within_category`
    ///
    /// Prüft, dass Operationen innerhalb derselben Kategorie alphabetisch
    /// nach Name sortiert sind.
    #[test]
    fn test_help_output_sorts_commands_alphabetically_within_category() {
        let ops: Vec<Arc<dyn Operation>> = vec![
            make_op(
                "stop",
                "Stoppt den Agenten.",
                OperationCategory::System,
                &[],
                vec![command_surface("/stop")],
            ),
            make_op(
                "attach",
                "Hängt sich an eine Session.",
                OperationCategory::System,
                &[],
                vec![command_surface("/attach")],
            ),
            make_op(
                "ps",
                "Listet Prozesse auf.",
                OperationCategory::System,
                &[],
                vec![command_surface("/ps")],
            ),
        ];

        let text = build_grouped_help(ops.iter(), None);

        let pos_attach = text.find("/attach").expect("'/attach' fehlt");
        let pos_ps = text.find("/ps").expect("'/ps' fehlt");
        let pos_stop = text.find("/stop").expect("'/stop' fehlt");

        assert!(
            pos_attach < pos_ps,
            "/attach muss vor /ps stehen; war:\n{text}"
        );
        assert!(pos_ps < pos_stop, "/ps muss vor /stop stehen; war:\n{text}");
    }

    // ── Zusatz: leere Aliasse erzeugen kein "(aliases:)" ─────────────────────

    #[test]
    fn test_help_output_no_alias_suffix_when_aliases_empty() {
        let op = make_op(
            "quit",
            "Beendet Harwness.",
            OperationCategory::System,
            &[],
            vec![command_surface("/quit")],
        );
        let ops: Vec<Arc<dyn Operation>> = vec![op];
        let text = build_grouped_help(ops.iter(), None);

        assert!(
            !text.contains("(aliases:"),
            "Leere Aliasse dürfen keinen Suffix erzeugen, war:\n{text}"
        );
    }
}
