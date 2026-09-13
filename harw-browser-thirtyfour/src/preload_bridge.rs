//! Installation of bounded, connector-owned BiDi preload bridges.
//!
//! This module deliberately keeps the WebDriver BiDi wire types at the adapter
//! boundary.  The core browser contract only receives an installation request
//! and receipt; the channel value and the generated function declaration never
//! cross that boundary.

use crate::error::AdapterError;
use crate::runtime::FirefoxRuntime;
use harw_browser::capability::CapabilityStatus;
use harw_browser::page_bridge::{
    PageBridgeInstallRequest, PageBridgeInstallationId, PageBridgeInstallationReceipt,
};
use sha2::{Digest, Sha256};
use thirtyfour::bidi::modules::script::AddPreloadScript;

const PRELOAD_CAPABILITY: &str = "webdriver-bidi-script-add-preload-script";
const CHANNEL_PREFIX: &str = "harwness-page-bridge-";

impl FirefoxRuntime {
    pub(crate) async fn install_preload_bridge(
        &self,
        request: PageBridgeInstallRequest,
    ) -> harw_browser::Result<PageBridgeInstallationReceipt> {
        if self.bidi_status() != CapabilityStatus::Native {
            return Err(unavailable(
                "the session has no native WebDriver BiDi connection",
            ));
        }

        let bidi = {
            let driver = self.driver().await;
            driver.webdriver().bidi().await.map_err(|error| {
                unavailable(format!(
                    "could not obtain the connected BiDi handle: {error}"
                ))
            })?
        };
        let bidi_context = bidi.browsing_context().top_level().await.map_err(|error| {
            unavailable(format!(
                "could not resolve the primary BiDi browsing context: {error}"
            ))
        })?;
        let mapped_context = self
            .resolve_bidi_context(bidi_context.as_str())
            .await
            .map_err(|error| {
                unavailable(format!(
                    "primary BiDi browsing context has no trustworthy Harwness mapping: {error}"
                ))
            })?;
        if mapped_context != request.context_id() {
            return Err(unavailable(
                "the requested Harwness context is not the mapped primary BiDi context",
            ));
        }

        let installation_id = PageBridgeInstallationId::new();
        let channel_id = format!("{CHANNEL_PREFIX}{installation_id}");
        let function_declaration = bounded_function_declaration(&request);
        let arguments = vec![channel_value(&channel_id)];
        let script_sha256 = sha256_hex(request.script().as_bytes());

        bidi.send(AddPreloadScript {
            function_declaration,
            arguments,
            sandbox: None,
            contexts: vec![bidi_context],
        })
        .await
        .map_err(|error| unavailable(format!("script.addPreloadScript failed: {error}")))?;

        Ok(PageBridgeInstallationReceipt::new(
            installation_id,
            request.context_id(),
            request.bridge_id(),
            request.version(),
            script_sha256,
        ))
    }
}

fn unavailable(detail: impl Into<String>) -> harw_browser::Error {
    AdapterError::CapabilityUnavailable {
        capability: PRELOAD_CAPABILITY,
        detail: detail.into(),
    }
    .into()
}

/// Builds the exact `script.ChannelValue` wire shape prescribed by WebDriver
/// BiDi.  Thirtyfour 0.37.2 documents this value but has not promoted it to a
/// Rust newtype yet, so it remains a private serialized implementation detail.
fn channel_value(channel_id: &str) -> serde_json::Value {
    serde_json::json!({
        "type": "channel",
        "value": {
            "channel": channel_id,
            "ownership": "root",
        },
    })
}

/// Wrap configured bridge source in the only page-side capability it needs:
/// `emit(value)`.  The configured source cannot access Harwness credentials;
/// it can only submit a bounded JSON payload over its assigned BiDi channel.
fn bounded_function_declaration(request: &PageBridgeInstallRequest) -> String {
    let policy = request.policy();
    let max_payload_bytes = policy.max_payload_bytes();
    let max_messages = policy.max_messages();
    let window_millis = policy.window().as_millis();

    format!(
        r#"(channel) => {{
    "use strict";
    const policy = Object.freeze({{
        maxPayloadBytes: {max_payload_bytes},
        maxMessages: {max_messages},
        windowMs: {window_millis},
    }});
    let windowStartedAt = Date.now();
    let sentInWindow = 0;
    const emit = (value) => {{
        let serialized;
        try {{
            serialized = JSON.stringify(value);
        }} catch (_error) {{
            return false;
        }}
        if (typeof serialized !== "string") {{
            return false;
        }}
        let byteLength;
        try {{
            byteLength = new TextEncoder().encode(serialized).byteLength;
        }} catch (_error) {{
            return false;
        }}
        if (byteLength > policy.maxPayloadBytes) {{
            return false;
        }}
        const now = Date.now();
        if (now - windowStartedAt >= policy.windowMs) {{
            windowStartedAt = now;
            sentInWindow = 0;
        }}
        if (sentInWindow >= policy.maxMessages) {{
            return false;
        }}
        sentInWindow += 1;
        channel(serialized);
        return true;
    }};
    const __harwBridge = Object.freeze({{ emit, policy }});
    (() => {{
{source}
    }})();
}}"#,
        source = indent_source(request.script()),
    )
}

fn indent_source(source: &str) -> String {
    source
        .lines()
        .map(|line| format!("        {line}"))
        .collect::<Vec<_>>()
        .join("\n")
}

fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::{bounded_function_declaration, channel_value};
    use harw_browser::ids::BrowserContextId;
    use harw_browser::page_bridge::{PageBridgeInstallRequest, PageBridgePolicy};
    use serde_json::json;
    use std::time::Duration;

    #[test]
    fn channel_value_uses_root_owned_bidi_wire_shape() {
        assert_eq!(
            channel_value("harwness-page-bridge-test"),
            json!({
                "type": "channel",
                "value": {
                    "channel": "harwness-page-bridge-test",
                    "ownership": "root",
                },
            })
        );
    }

    #[test]
    fn generated_preload_function_receives_channel_and_enforces_bounds() {
        let policy = PageBridgePolicy::new(512, 3, Duration::from_secs(2))
            .expect("valid page bridge policy");
        let request = PageBridgeInstallRequest::new(
            BrowserContextId::new(),
            "erp-chat",
            "v1",
            "const observer = new MutationObserver(() => emit({ schema: 'v1' }));",
            policy,
        )
        .expect("valid page bridge request");

        let source = bounded_function_declaration(&request);
        assert!(source.starts_with("(channel) =>"));
        assert!(source.contains("channel(serialized)"));
        assert!(source.contains("maxPayloadBytes: 512"));
        assert!(source.contains("maxMessages: 3"));
        assert!(source.contains("byteLength > policy.maxPayloadBytes"));
        assert!(source.contains("sentInWindow >= policy.maxMessages"));
        assert!(source.contains("const __harwBridge = Object.freeze({ emit, policy })"));
    }
}
