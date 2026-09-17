//! `/status` — Session- und Turn-Status-Operation.
//!
//! # Verantwortungsbereich
//! Implementiert die `status`-Operation, die als `/status`-Command (channel_parity)
//! und als readonly Model-Tool (approval = none) verfügbar ist. Gibt Informationen
//! über die aktuelle Session, den aktuellen Turn und den Sandbox-Kontext zurück.
//!
//! # Schlüsseltypen
//! - [`StatusArgs`] — leerer Argument-Container (keine Parameter erforderlich)
//! - `StatusOperation` — generiert vom `#[operation]`-Makro
//!
//! # Nebenläufigkeit
//! `StatusOperation` ist `Send + Sync` (Unit-Struct ohne inneren Zustand, generiert
//! durch `#[operation]`-Makro).
//!
//! # Fehlertypen
//! Diese Operation erzeugt keine Fehler; `Ok(OpOutput::from(text))` wird immer zurückgegeben.
//!
//! # Beispiel
//! ```rust,no_run
//! use harw_ops::status::StatusArgs;
//! // Die Operation wird über den harw-operations-Registry-Mechanismus aufgerufen.
//! ```

use harw_macros::operation;
use harw_operations::{OpContext, OpError, OpOutput};
use harw_operations::session_control::SharedSessionController;

/// Leerer Argument-Container für die `/status`-Operation.
///
/// # Beschreibung
/// Die Status-Operation benötigt keine Eingabeparameter — alle relevanten Daten
/// werden aus dem [`OpContext`] bezogen. Dieser Typ existiert, weil das
/// `#[operation]`-Makro einen `Deserialize + Default`-Args-Typ erfordert.
///
/// # Spec-Referenz
/// Plan v2 — `/status` Meta-Definition.
#[derive(Default, serde::Deserialize, harw_macros::FromRawArgs)]
pub struct StatusArgs {}

/// Gibt den aktuellen Session- und Turn-Status zurück.
///
/// # Beschreibung
/// Liest Session-ID, Turn-ID und Sandbox-Informationen aus dem [`OpContext`]
/// und formatiert sie als lesbare mehrzeilige Textausgabe. Der Berechtigungs-
/// zähler ergibt sich aus der Anzahl der im [`harw_sandbox::PermissionSet`]
/// enthaltenen Einträge.
///
/// # Argumente
/// - `ctx` (`&OpContext`): Unveränderlicher Ausführungs-Kontext mit Session-,
///   Turn- und Sandbox-Daten.
/// - `_args` (`StatusArgs`): Leer — keine Parameter.
///
/// # Rückgabe
/// `Ok(OpOutput::from(text))` mit mehrzeiligem Statustext.
///
/// # Fehler
/// Diese Funktion gibt niemals `Err` zurück.
///
/// # Nebenläufigkeit
/// Zustandslos und sicher aus mehreren Threads aufrufbar.
///
/// # Beispiel
/// ```rust,no_run
/// // Aufruf erfolgt über Operation::run() — direkte Verwendung nur im Test-Kontext.
/// ```
#[operation(
    name = "status",
    summary = "Zeigt aktuellen Session- und Turn-Status.",
    domain = "session",
    permission = "observer",
    command(path = "/status", visibility = "channel_parity"),
    model_tool(readonly, approval = "none"),
    // Web-Fläche übernimmt exakt dieselbe Achse wie das ModelTool oben:
    // `method = "get"`, weil die Operation nur `ctx.session_id()`/`ctx.turn_id()`
    // liest, approval "none", weil ein reiner Statusabruf keine Bestätigung
    // erfordert.
    web(path = "/api/status", method = "get", approval = "none")
)]
async fn status(ctx: &OpContext, _args: StatusArgs) -> Result<OpOutput, OpError> {
    let session_id = ctx.session_id();
    let turn_id = ctx.turn_id();
    let sandbox = ctx.sandbox();

    let workspace_id = sandbox.workspace().workspace();
    let tenant_id = sandbox.workspace().tenant();
    let permission_count = sandbox.permissions().iter().count();

    // Provider und Modell aus dem Session-Controller abfragen, falls in der
    // ServiceMap registriert. `None` bedeutet: kein Controller in dieser
    // Ausfuehrungsumgebung (z. B. Test oder CLI-Echo-Pfad).
    let (provider, model) = match ctx.service::<SharedSessionController>() {
        Some(controller) => {
            let snap = controller.snapshot();
            (snap.active_provider, snap.active_model)
        }
        None => (None, None),
    };

    let mut lines = vec![
        format!("Session: {session_id}"),
        format!("Turn:    {turn_id}"),
        format!("Sandbox: {workspace_id} ({tenant_id})"),
        format!("Permissions: {permission_count} aktiv"),
    ];

    // Provider- und Modellzeile nur anzeigen, wenn Informationen vorhanden.
    if let Some(p) = &provider {
        lines.push(format!("Provider: {p}"));
    }
    if let Some(m) = &model {
        lines.push(format!("Modell: {m}"));
    }

    let text = lines.join("\n");

    Ok(OpOutput::from(text))
}

#[cfg(test)]
mod tests {
    use super::StatusArgs;
    use crate::testutil::toks;
    use harw_operations::FromRawArgs;

    #[test]
    fn test_status_args_from_raw_args_empty_tokens_returns_ok() {
        let result = StatusArgs::from_raw_args(&toks(&[]));
        assert!(result.is_ok());
    }

    #[test]
    fn test_status_args_from_raw_args_ignores_extra_tokens() {
        let result = StatusArgs::from_raw_args(&toks(&["ignored"]));
        assert!(result.is_ok());
    }
}
