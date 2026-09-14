//! Umkehrbare Abbildung interner Tool-Namen auf provider-taugliche Wire-Namen.
//!
//! Harw-Tools heißen z. B. `fs.read`, `shell.exec` oder `context.load`. Die
//! Anthropic-Messages-API akzeptiert als Tool-Namen aber nur
//! `^[a-zA-Z0-9_-]{1,128}$` (sonst HTTP 400 `tools.N.custom.name`). Dieser
//! Codec ersetzt unzulässige Zeichen durch `_`, kürzt auf 128 Zeichen,
//! löst Kollisionen deterministisch mit einem numerischen Suffix auf und merkt
//! sich die Rückrichtung, damit `tool_use`-Blöcke der Antwort wieder den
//! internen Namen tragen.
//!
//! Die Abbildung hängt nur von der Reihenfolge der Tools und des Verlaufs ab —
//! derselbe Request ergibt bytegleiche Wire-Namen (cache-stabil).

use harw_core::{ModelMessage, ModelRequest};
use std::collections::BTreeMap;

/// Maximale Länge eines Wire-Tool-Namens (Anthropic- und OpenAI-Grenze).
const MAX_WIRE_NAME_LEN: usize = 128;

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
    /// `String` — der Wire-taugliche Name (`^[a-zA-Z0-9_-]{1,128}$`).
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

// Ersetzt alles außerhalb von `[A-Za-z0-9_-]` durch `_` und kürzt auf 128 Bytes
// (nach der Ersetzung ist der String reines ASCII, Byte-Slicing also sicher).
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
    out.truncate(MAX_WIRE_NAME_LEN);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use harw_core::ModelRequest;
    use harw_tools::{FunctionToolSpec, JsonSchema, JsonSchemaType, ToolName, ToolSpec};
    use harw_types::ToolCallId;

    // Baut einen `ModelRequest` mit den übergebenen Tools und einer leeren
    // Historie, angelehnt an `request_with_ids` in `anthropic.rs`.
    fn request_with_tools(tools: Vec<ToolSpec>) -> ModelRequest {
        ModelRequest {
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
    fn test_sanitize_truncates_to_128_chars() {
        let long_name = "a".repeat(200);
        let sanitized = sanitize(&long_name);
        assert_eq!(sanitized.len(), MAX_WIRE_NAME_LEN);
        assert!(sanitized.chars().all(|c| c == 'a'));
    }

    #[test]
    fn test_for_request_registers_tool_spec_names() {
        let request = request_with_tools(vec![function_tool("fs.read")]);
        let codec = ToolNameCodec::for_request(&request);

        assert_eq!(codec.encode("fs.read"), "fs_read");
        assert_eq!(codec.decode("fs_read"), "fs.read");
    }

    #[test]
    fn test_for_request_registers_history_tool_call_names() {
        let mut history = harw_core::ConversationHistory::new();
        history.push_tool_call(
            ToolCallId::try_from_str("call-1").expect("valid call id"),
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
