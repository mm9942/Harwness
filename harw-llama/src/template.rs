//! Renders the chat prompt from the template stored in the GGUF.
//!
//! The template is Jinja. `minijinja` (pure Rust) renders it with the Python-style
//! string methods HF templates use; if the template is missing or fails to render,
//! a ChatML prompt in the Qwen layout is built instead.

use serde_json::{Value, json};

use crate::error::LlamaError;

/// One tool call inside an assistant message.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ChatToolCall {
    pub name: String,
    pub arguments: Value,
}

/// One message of the conversation.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ChatMessage {
    /// `system`, `user`, `assistant` or `tool`.
    pub role: &'static str,
    pub content: String,
    pub tool_calls: Vec<ChatToolCall>,
}

impl ChatMessage {
    pub(crate) fn new(role: &'static str, content: impl Into<String>) -> Self {
        Self {
            role,
            content: content.into(),
            tool_calls: Vec::new(),
        }
    }
}

/// Everything the template sees.
pub(crate) struct RenderInput<'a> {
    pub messages: &'a [ChatMessage],
    /// OpenAI-style `{"type":"function","function":{...}}` entries.
    pub tools: &'a [Value],
    pub enable_thinking: bool,
    pub bos_token: &'a str,
    pub eos_token: &'a str,
}

fn message_json(message: &ChatMessage) -> Value {
    let mut value = json!({ "role": message.role, "content": message.content });
    if !message.tool_calls.is_empty() {
        let calls: Vec<Value> = message
            .tool_calls
            .iter()
            .map(|call| {
                let arguments = match &call.arguments {
                    Value::Object(_) => call.arguments.clone(),
                    Value::String(text) => serde_json::from_str::<Value>(text)
                        .ok()
                        .filter(Value::is_object)
                        .unwrap_or_else(|| json!({})),
                    _ => json!({}),
                };
                json!({"type":"function","function":{"name": call.name, "arguments": arguments}})
            })
            .collect();
        value["tool_calls"] = Value::Array(calls);
    }
    value
}

fn render_jinja(source: &str, input: &RenderInput<'_>) -> Result<String, minijinja::Error> {
    use minijinja::{Environment, Error, ErrorKind, Value as JValue, value::Kwargs};

    let mut env = Environment::new();
    env.set_trim_blocks(true);
    env.set_lstrip_blocks(true);
    env.set_unknown_method_callback(minijinja_contrib::pycompat::unknown_method_callback);
    env.add_function("raise_exception", |message: String| -> Result<String, Error> {
        Err(Error::new(ErrorKind::InvalidOperation, message))
    });
    env.add_function("strftime_now", |format: String| -> String {
        jiff::Zoned::now().strftime(&format).to_string()
    });
    // HF templates pass extra keywords (`ensure_ascii`, `indent`); accept and ignore them.
    env.add_filter("tojson", |value: JValue, _kwargs: Kwargs| -> Result<String, Error> {
        serde_json::to_string(&value)
            .map_err(|e| Error::new(ErrorKind::InvalidOperation, e.to_string()))
    });
    env.add_template_owned("chat", source.to_owned())?;
    let messages: Vec<Value> = input.messages.iter().map(message_json).collect();
    let context = json!({
        "messages": messages,
        "tools": input.tools,
        "add_generation_prompt": true,
        "enable_thinking": input.enable_thinking,
        "bos_token": input.bos_token,
        "eos_token": input.eos_token,
    });
    env.get_template("chat")?.render(JValue::from_serialize(&context))
}

/// Renders the prompt, preferring the GGUF template.
pub(crate) fn render_prompt(
    template: Option<&str>,
    input: &RenderInput<'_>,
) -> Result<String, LlamaError> {
    if let Some(source) = template.filter(|t| !t.trim().is_empty()) {
        match render_jinja(source, input) {
            Ok(prompt) if !prompt.trim().is_empty() => return Ok(prompt),
            Ok(_) => tracing::warn!("chat template rendered an empty prompt; using ChatML"),
            Err(error) => tracing::warn!(%error, "chat template failed; using ChatML"),
        }
    }
    Ok(chatml_prompt(input))
}

fn tool_call_markup(call: &ChatToolCall) -> String {
    let body = json!({ "name": call.name, "arguments": call.arguments });
    format!("<tool_call>\n{body}\n</tool_call>")
}

/// Qwen-style ChatML with tools in the system turn.
pub(crate) fn chatml_prompt(input: &RenderInput<'_>) -> String {
    let mut out = String::new();
    let mut system = String::new();
    let mut rest = input.messages;
    if let Some((first, tail)) = input.messages.split_first() {
        if first.role == "system" {
            system.push_str(&first.content);
            rest = tail;
        }
    }
    if !input.tools.is_empty() {
        if !system.is_empty() {
            system.push_str("\n\n");
        }
        system.push_str(
            "# Tools\n\nYou may call one or more functions to assist with the user query.\n\n\
             You are provided with function signatures within <tools></tools> XML tags:\n<tools>",
        );
        for tool in input.tools {
            system.push('\n');
            system.push_str(&tool.to_string());
        }
        system.push_str(
            "\n</tools>\n\nFor each function call, return a json object with the function name \
             and arguments within <tool_call></tool_call> XML tags:\n<tool_call>\n\
             {\"name\": <function-name>, \"arguments\": <args-json-object>}\n</tool_call>",
        );
    }
    if !system.is_empty() {
        out.push_str("<|im_start|>system\n");
        out.push_str(&system);
        out.push_str("<|im_end|>\n");
    }
    for message in rest {
        match message.role {
            "tool" => {
                out.push_str("<|im_start|>user\n<tool_response>\n");
                out.push_str(&message.content);
                out.push_str("\n</tool_response><|im_end|>\n");
            }
            role => {
                out.push_str("<|im_start|>");
                out.push_str(role);
                out.push('\n');
                out.push_str(&message.content);
                for call in &message.tool_calls {
                    if !message.content.is_empty() {
                        out.push('\n');
                    }
                    out.push_str(&tool_call_markup(call));
                }
                out.push_str("<|im_end|>\n");
            }
        }
    }
    out.push_str("<|im_start|>assistant\n");
    if !input.enable_thinking {
        out.push_str("<think>\n\n</think>\n\n");
    }
    out
}

/// Whether the prompt ends inside an open `<think>` block (thinking mode).
pub(crate) fn prompt_opens_think(prompt: &str) -> bool {
    prompt.trim_end().ends_with("<think>")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input<'a>(messages: &'a [ChatMessage], thinking: bool) -> RenderInput<'a> {
        RenderInput {
            messages,
            tools: &[],
            enable_thinking: thinking,
            bos_token: "",
            eos_token: "<|im_end|>",
        }
    }

    #[test]
    fn jinja_template_renders_with_pycompat_methods() {
        let template = "{% for m in messages %}{{ m.role }}:{{ m.content.strip() }}\n{% endfor %}\
                        {% if add_generation_prompt %}assistant:{% if not enable_thinking %}no{% endif %}{% endif %}";
        let messages = [ChatMessage::new("user", " hi ")];
        let prompt = render_prompt(Some(template), &input(&messages, false)).unwrap_or_default();
        assert_eq!(prompt, "user:hi\nassistant:no");
    }

    #[test]
    fn chatml_fallback_closes_think_when_thinking_is_off() {
        let messages = [ChatMessage::new("user", "hi")];
        let prompt = chatml_prompt(&input(&messages, false));
        assert!(prompt.ends_with("<|im_start|>assistant\n<think>\n\n</think>\n\n"));
        assert!(!prompt_opens_think(&prompt));
    }
}
