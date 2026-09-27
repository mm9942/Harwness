//! Umkehrbare Abbildung interner Tool-Namen auf provider-taugliche Wire-Namen.
//!
//! Harw-Tools heißen z. B. `fs.read`, `shell.exec` oder `context.load`. Wir
//! sprechen mehrere Provider mit unterschiedlichen, aber überlappenden
//! Namensregeln für Tool-Namen:
//!
//! - Anthropic Messages API: `^[a-zA-Z0-9_-]{1,128}$` (sonst HTTP 400
//!   `tools.N.custom.name`).
//! - OpenAI Chat Completions: höchstens 64 Zeichen, Zeichen aus
//!   `[a-zA-Z0-9_-]`.
//! - Gemini: höchstens 64 Zeichen, muss mit einem Buchstaben oder `_`
//!   beginnen, danach `[a-zA-Z0-9_-]`.
//!
//! Wir erzeugen Wire-Namen nach der strengsten gemeinsamen Regel aller drei:
//! `^[A-Za-z_][A-Za-z0-9_-]{0,63}$`. Dieser Codec ersetzt unzulässige Zeichen
//! durch `_`, stellt bei einem unzulässigen ersten Zeichen (Ziffer oder `-`)
//! ein `_` voran, kürzt auf 64 Zeichen, löst Kollisionen deterministisch mit
//! einem numerischen Suffix auf und merkt sich die Rückrichtung, damit
//! `tool_use`-Blöcke der Antwort wieder den internen Namen tragen.
//!
//! Für jeden Namen, der schon vor dieser Änderung gültig war (kein führendes
//! `-`/Ziffer, ≤ 64 Zeichen), bleibt der Wire-Name bytegleich — wichtig, weil
//! Prompt-Caches davon abhängen. Nur Namen, die die neue, strengere Grenze
//! verletzen, ändern sich.
//!
//! Die Abbildung hängt nur von der Reihenfolge der Tools und des Verlaufs ab —
//! derselbe Request ergibt bytegleiche Wire-Namen (cache-stabil).

use harw_core::{ModelMessage, ModelRequest};
use std::collections::BTreeMap;

/// Maximale Länge eines Wire-Tool-Namens (strengste gemeinsame Grenze:
/// OpenAI Chat Completions und Gemini erlauben höchstens 64 Zeichen).
const MAX_WIRE_NAME_LEN: usize = 64;

/// Bidirektionale Zuordnung intern ↔ Wire für genau einen Request.
///
/// # Description
/// Hält zwei komplementäre `BTreeMap`s: `to_wire` für die Kodierung ausgehender
/// Tool-Namen, `from_wire` für die Dekodierung der `tool_use`-Namen aus der
/// Antwort. Beide werden einmalig über [`ToolNameCodec::for_request`] befüllt
/// und danach nur noch gelesen — pro Request wird eine neue Instanz gebaut,
/// es gibt keinen geteilten Zustand über Requests hinweg.
///
/// # Concurrency
/// Kein `Send`/`Sync`-Zwang nötig: Eine Instanz lebt nur innerhalb der
/// synchronen Bearbeitung eines einzelnen Requests auf einem Thread.
#[derive(Debug, Default)]
pub(crate) struct ToolNameCodec {
    to_wire: BTreeMap<String, String>,
    from_wire: BTreeMap<String, String>,
}

impl ToolNameCodec {
    /// Baut den Codec aus den angebotenen Tools und allen Tool-Calls im Verlauf.
    ///
    /// # Description
    /// Registriert zunächst jeden in `request.tools` angebotenen Funktionsnamen,
    /// danach jeden `name` aus einem `ModelMessage::ToolCall` im Verlauf
    /// (`request.history`). Die Reihenfolge ist deterministisch (Iterationsreihenfolge
    /// von `request.tools` bzw. `to_model_messages()`), sodass Kollisionssuffixe
    /// für denselben Request stets identisch vergeben werden (cache-stabil).
    ///
    /// # Arguments
    /// - `request` (`&ModelRequest`): der aufzubauende Request; wird nur gelesen.
    ///
    /// # Returns
    /// `Self` — vollständig befüllter Codec für genau diesen Request.
    ///
    /// # Concurrency
    /// Rein lesend gegenüber `request`; keine geteilten Zustände.
    pub(crate) fn for_request(request: &ModelRequest) -> Self {
        let mut codec = Self::for_tools(&request.tools);
        for message in request.history.to_model_messages() {
            if let ModelMessage::ToolCall { name, .. } = message {
                codec.register(&name);
            }
        }
        codec
    }

    /// Baut den Codec nur aus den angebotenen Tools.
    ///
    /// Tools werden in [`ToolNameCodec::for_request`] zuerst registriert; ihre
    /// Wire-Namen sind daher identisch, egal ob der Verlauf mitregistriert wird.
    pub(crate) fn for_tools(tools: &[harw_tools::ToolSpec]) -> Self {
        let mut codec = Self::default();
        for spec in tools {
            match spec {
                harw_tools::ToolSpec::Function(function) => codec.register(function.name.as_str()),
            }
        }
        codec
    }

    // Vergibt einen eindeutigen Wire-Namen, falls `name` noch unbekannt ist.
    fn register(&mut self, name: &str) {
        if self.to_wire.contains_key(name) {
            return;
        }
        let base = sanitize(name);
        let mut candidate = base.clone();
        let mut suffix = 2_usize;
        while self.from_wire.contains_key(&candidate) {
            let tail = format!("_{suffix}");
            let keep = MAX_WIRE_NAME_LEN.saturating_sub(tail.len()).min(base.len());
            candidate = format!("{}{tail}", &base[..keep]);
            suffix += 1;
        }
        self.to_wire.insert(name.to_owned(), candidate.clone());
        self.from_wire.insert(candidate, name.to_owned());
    }

    /// Liefert den Wire-Namen; unbekannte Namen werden nur bereinigt.
    ///
    /// # Description
    /// Schlägt `name` in `to_wire` nach. Ein unbekannter Name (nicht über
    /// [`ToolNameCodec::for_request`] registriert) wird nicht als Fehler
    /// behandelt, sondern nur über [`sanitize`] bereinigt und ohne
    /// Kollisionsauflösung zurückgegeben — das kann in seltenen Fällen mit
    /// einem registrierten Wire-Namen kollidieren, betrifft aber nur Namen,
    /// die weder als Tool angeboten noch im Verlauf aufgerufen wurden.
    ///
    /// # Arguments
    /// - `name` (`&str`): interner Tool-Name, geliehen.
    ///
    /// # Returns
    /// `String` — der Wire-taugliche Name
    /// (`^[A-Za-z_][A-Za-z0-9_-]{0,63}$`).
    ///
    /// # Concurrency
    /// Rein lesend; sicher aus mehreren Threads parallel auf derselben
    /// gemeinsam referenzierten Instanz aufrufbar.
    pub(crate) fn encode(&self, name: &str) -> String {
        self.to_wire
            .get(name)
            .cloned()
            .unwrap_or_else(|| sanitize(name))
    }

    /// Liefert den internen Namen; unbekannte Wire-Namen bleiben unverändert.
    ///
    /// # Description
    /// Schlägt `wire` in `from_wire` nach — die Umkehrung von
    /// [`ToolNameCodec::encode`]. Ein unbekannter Wire-Name (z. B. ein von
    /// diesem Prozess nicht registriertes `tool_use.name` aus der Antwort)
    /// wird unverändert zurückgegeben, statt einen Fehler auszulösen.
    ///
    /// # Arguments
    /// - `wire` (`&str`): der von der Provider-API gelieferte Tool-Name, geliehen.
    ///
    /// # Returns
    /// `String` — der interne Tool-Name (z. B. `fs.read`), sofern registriert;
    /// sonst `wire` unverändert als `String`.
    ///
    /// # Concurrency
    /// Rein lesend; sicher aus mehreren Threads parallel auf derselben
    /// gemeinsam referenzierten Instanz aufrufbar.
    pub(crate) fn decode(&self, wire: &str) -> String {
        self.from_wire
            .get(wire)
            .cloned()
            .unwrap_or_else(|| wire.to_owned())
    }
}

// Ersetzt alles außerhalb von `[A-Za-z0-9_-]` durch `_`, stellt bei einem
// unzulässigen ersten Zeichen (Ziffer oder `-`, von Gemini verboten) ein `_`
// voran und kürzt auf MAX_WIRE_NAME_LEN Bytes (nach der Ersetzung ist der
// String reines ASCII, Byte-Slicing also sicher).
fn sanitize(name: &str) -> String {
    let mut out: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    if out.is_empty() {
        out.push('_');
    }
    if out.starts_with(|c: char| c.is_ascii_digit() || c == '-') {
        out.insert(0, '_');
    }
    out.truncate(MAX_WIRE_NAME_LEN);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestResult, ctx};
    use harw_core::ModelRequest;
    use harw_tools::{FunctionToolSpec, JsonSchema, JsonSchemaType, ToolName, ToolSpec};
    use harw_types::ToolCallId;

    // Baut einen `ModelRequest` mit den übergebenen Tools und einer leeren
    // Historie, angelehnt an `request_with_ids` in `anthropic.rs`.
    fn request_with_tools(tools: Vec<ToolSpec>) -> ModelRequest {
        ModelRequest {
            stream: None,
            system_prompt: String::new(),
            instruction_fragments: Vec::new(),
            context: Vec::new(),
            history: harw_core::ConversationHistory::new(),
            tools,
            context_assembly: Default::default(),
            reasoning_effort: None,
            model_id: None,
            provider_id: None,
            data_block: None,
            max_output_tokens: None,
            tool_result_max_bytes: None,
            cancel: None,
            identity: None,
        }
    }

    fn function_tool(name: &str) -> ToolSpec {
        ToolSpec::Function(FunctionToolSpec {
            name: ToolName::new(name),
            description: "test tool".to_owned(),
            parameters: JsonSchema {
                schema_type: Some(JsonSchemaType::Object),
                ..Default::default()
            },
            strict: false,
        })
    }

    #[test]
    fn test_sanitize_dotted_name_replaces_dots_with_underscore() {
        assert_eq!(sanitize("fs.read"), "fs_read");
    }

    #[test]
    fn test_sanitize_valid_name_is_unchanged() {
        assert_eq!(sanitize("shell_exec-1"), "shell_exec-1");
    }

    #[test]
    fn test_sanitize_empty_name_becomes_underscore() {
        assert_eq!(sanitize(""), "_");
    }

    #[test]
    fn test_sanitize_truncates_to_64_chars() {
        let long_name = "a".repeat(200);
        let sanitized = sanitize(&long_name);
        assert_eq!(sanitized.len(), MAX_WIRE_NAME_LEN);
        assert!(sanitized.chars().all(|c| c == 'a'));
    }

    #[test]
    fn test_sanitize_leading_digit_gets_underscore_prefix() {
        assert_eq!(sanitize("1tool"), "_1tool");
    }

    #[test]
    fn test_sanitize_leading_hyphen_gets_underscore_prefix() {
        assert_eq!(sanitize("-tool"), "_-tool");
    }

    #[test]
    fn test_sanitize_100_char_name_becomes_at_most_64_and_round_trips() -> TestResult {
        let long_name = "x".repeat(100);
        let request = request_with_tools(vec![function_tool(&long_name)]);
        let codec = ToolNameCodec::for_request(&request);

        let wire = codec.encode(&long_name);
        assert!(wire.len() <= MAX_WIRE_NAME_LEN);
        assert_eq!(codec.decode(&wire), long_name);
        Ok(())
    }

    #[test]
    fn test_for_request_leading_digit_name_round_trips() -> TestResult {
        let request = request_with_tools(vec![function_tool("123tool")]);
        let codec = ToolNameCodec::for_request(&request);

        let wire = codec.encode("123tool");
        assert_eq!(wire, "_123tool");
        assert_eq!(codec.decode(&wire), "123tool");
        Ok(())
    }

    #[test]
    fn test_work_driver_enqueue_sanitizes_to_work_driver_enqueue() {
        assert_eq!(sanitize("work_driver.enqueue"), "work_driver_enqueue");
    }

    #[test]
    fn test_collisions_after_truncation_stay_distinct() -> TestResult {
        // Zwei Namen, die erst nach dem Kürzen auf MAX_WIRE_NAME_LEN
        // identisch würden, müssen trotzdem unterscheidbare Wire-Namen
        // bekommen und sich beide zurückdekodieren lassen.
        let prefix = "x".repeat(MAX_WIRE_NAME_LEN);
        let name_a = format!("{prefix}.a");
        let name_b = format!("{prefix}.b");
        let request = request_with_tools(vec![function_tool(&name_a), function_tool(&name_b)]);
        let codec = ToolNameCodec::for_request(&request);

        let wire_a = codec.encode(&name_a);
        let wire_b = codec.encode(&name_b);

        assert!(wire_a.len() <= MAX_WIRE_NAME_LEN);
        assert!(wire_b.len() <= MAX_WIRE_NAME_LEN);
        assert_ne!(wire_a, wire_b);
        assert_eq!(codec.decode(&wire_a), name_a);
        assert_eq!(codec.decode(&wire_b), name_b);
        Ok(())
    }

    #[test]
    fn test_for_request_registers_tool_spec_names() {
        let request = request_with_tools(vec![function_tool("fs.read")]);
        let codec = ToolNameCodec::for_request(&request);

        assert_eq!(codec.encode("fs.read"), "fs_read");
        assert_eq!(codec.decode("fs_read"), "fs.read");
    }

    #[test]
    fn test_for_request_registers_history_tool_call_names() -> TestResult {
        let mut history = harw_core::ConversationHistory::new();
        history.push_tool_call(
            ToolCallId::try_from_str("call-1").map_err(ctx("valid call id"))?,
            "context.load",
            serde_json::json!({}),
        );
        let request = ModelRequest {
            history,
            ..request_with_tools(Vec::new())
        };
        let codec = ToolNameCodec::for_request(&request);

        assert_eq!(codec.encode("context.load"), "context_load");
        assert_eq!(codec.decode("context_load"), "context.load");
        Ok(())
    }

    #[test]
    fn test_for_request_collision_between_dotted_and_underscored_names_gets_distinct_wire_names() {
        // `a.b` und `a_b` sanitizen beide zu `a_b`; der zweite muss einen
        // Suffix bekommen, und beide müssen sich wieder zurückdekodieren lassen.
        let request = request_with_tools(vec![function_tool("a.b"), function_tool("a_b")]);
        let codec = ToolNameCodec::for_request(&request);

        let wire_a_dot_b = codec.encode("a.b");
        let wire_a_underscore_b = codec.encode("a_b");

        assert_eq!(wire_a_dot_b, "a_b");
        assert_eq!(wire_a_underscore_b, "a_b_2");
        assert_ne!(wire_a_dot_b, wire_a_underscore_b);
        assert_eq!(codec.decode(&wire_a_dot_b), "a.b");
        assert_eq!(codec.decode(&wire_a_underscore_b), "a_b");
    }

    #[test]
    fn test_decode_unknown_wire_name_returns_input_unchanged() {
        let codec = ToolNameCodec::for_request(&request_with_tools(Vec::new()));

        assert_eq!(codec.decode("never_registered"), "never_registered");
    }

    #[test]
    fn test_encode_unknown_name_returns_sanitized_input() {
        let codec = ToolNameCodec::for_request(&request_with_tools(Vec::new()));

        assert_eq!(codec.encode("unregistered.tool"), "unregistered_tool");
    }
}
