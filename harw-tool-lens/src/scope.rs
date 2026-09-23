//! Woher `lens.ask` seinen [`ReadScope`] nimmt — und warum nicht vom Aufrufer.
//!
//! # Die Regel, die dieses Modul durchsetzt
//! `harw_lens_query::resolve_index` prüft den [`ReadScope`] als **erste**
//! Handlung, bevor irgendein Index geöffnet wird, und liefert bei Verstoß
//! einen Fehler statt `Ok(vec![])` (siehe `harw-lens-query`s
//! Moduldokumentation). Diese Zusage nützt aber nichts, wenn der `ReadScope`
//! selbst vom Modell kontrolliert wird: ein Agent, der sein eigenes
//! `ReadScope` in `call.arguments` mitgeben dürfte, wählte damit seine eigene
//! Berechtigung — das Feld wäre eine reine Deklaration ohne Durchsetzung.
//! [`LensAskArgs`](crate::ask_tool::LensAskArgs) trägt deshalb **kein**
//! `scope`-, `visibility`- oder `read_scope`-Feld; jedes derartige Feld in
//! `call.arguments` wird über `#[serde(deny_unknown_fields)]` abgewiesen
//! (siehe `test_ask_args_reject_caller_supplied_scope_field` in `ask_tool.rs`).
//!
//! # Der gescheiterte Versuch, dem Muster von `sandbox_guard.rs` zu folgen
//! `harw-tools/src/sandbox_guard.rs` holt die Grenze eines Werkzeugs aus
//! [`harw_tools::ToolExecutionContext::sandbox`] — einer serverseitig
//! aufgebauten, vom Modell nicht erreichbaren Autorität
//! ([`harw_authority::SandboxSpec`]). Dieses Modul hat genau dort nachgesehen,
//! **bevor** es eine eigene Lösung gebaut hat, wie der Auftrag verlangt. Der
//! Befund: [`harw_authority::SandboxSpec`] trägt zum Zeitpunkt dieses Knotens
//! genau drei Felder — `workspace` (eine [`harw_authority::WorkspaceBinding`]:
//! Dateisystempfade), `permissions` (eine geschlossene Menge aus
//! [`harw_authority::Permission::ReadWorkspace`],
//! `WriteWorkspace`, `ExecuteProcess`, `NetworkAccess`, `ReadSecrets`,
//! `ManagePlugins`, `ReadCargoRegistry`) und `network_scope` (erlaubte
//! Ziel-Hosts). Keines dieser drei Felder kennt eine Sichtbarkeits- oder
//! Vertrauensstufe, die sich auf einen Lens-`visibility`-Bucket
//! (`"workspace"` vs. `"operator-only"`) abbilden ließe — es gibt in der
//! Sandbox keinen Begriff von „Operator" gegenüber „gewöhnlicher Agent".
//!
//! **Das ist ein Befund, kein Grund, den Parameter durchzureichen.** Die
//! Auflage verlangt ausdrücklich, das Fehlen zu melden statt den Aufrufer
//! selbst wählen zu lassen. Dieses Modul trifft deshalb die einzige Wahl, die
//! mit der Sicherheitszusage von `resolve_index`/`federated_query`
//! vereinbar bleibt: [`derive_read_scope`] liefert **immer** genau
//! [`harw_lens::DEFAULT_VISIBILITY`] (`"workspace"`) und **nie**
//! [`harw_lens::OPERATOR_ONLY_VISIBILITY`] — unabhängig von `context`. Das
//! ist fail-closed (ein Agent verliert nie mehr als nötig sichtbare Treffer,
//! er bekommt nie mehr als die harmloseste Sichtbarkeit), aber es ist
//! bewusst **keine** echte Autorisierungsentscheidung: es ist eine feste
//! Konstante, die so lange gilt, bis `harw-sandbox` einen echten
//! Operator-Begriff bekommt (siehe den `//!`-Block von `crate` für den
//! vollständigen Befund und einen Vorschlag: eine
//! `Permission::ReadOperatorOnlyLensIndex`-Variante, analog zu
//! `Permission::ReadCargoRegistry` in `harw-tool-deps`, oder ein
//! `trust_level`-Feld auf `SandboxSpec`).
//!
//! `context` wird trotzdem als Parameter geführt (statt eine parameterlose
//! Konstante zurückzugeben): der Aufrufort in `ask_tool.rs` liest dadurch wie
//! ein normaler `sandbox_guard`-Aufruf, und sobald `harw-sandbox` einen
//! echten Operator-Begriff bekommt, ändert sich nur dieser eine
//! Funktionskörper — kein Aufrufer muss angepasst werden.
//!
//! # Der Indexkatalog: fest verdrahtet, nicht vom Aufrufer gewählt
//! Aus demselben Grund trägt [`LensAskArgs`](crate::ask_tool::LensAskArgs)
//! auch **keinen** `index_name`: ließe der Aufrufer den Index wählen, könnte
//! er implizit auch die Sichtbarkeit wählen (ein Index ist physisch an genau
//! eine Sichtbarkeit gebunden, siehe `harw-lens-source`s Moduldokumentation).
//! [`KNOWN_SELECTORS`] listet stattdessen **alle** Selektoren, die dieses
//! Werkzeug kennt — abgeleitet aus den von `harw-lens-source` tatsächlich
//! gebauten Kombinationen: `docs.design` entsteht ausschließlich bei
//! [`harw_lens::DEFAULT_VISIBILITY`] (`collect_design_docs` schreibt
//! `visibility` fest auf diesen Wert), `knowledge.palace` bei **beiden**
//! Sichtbarkeiten (`collect_palace_documents` leitet sie je Dokument über
//! `visibility_of_scope` ab). [`selectors_in_scope`] filtert diesen Katalog
//! gegen [`derive_read_scope`]s Ergebnis — der Aufrufer wählt nichts, er
//! bekommt, was sein (fester) Lesebereich hergibt.
use harw_lens::{
    DEFAULT_VISIBILITY, DOCS_DESIGN_INDEX, IndexSelector, KNOWLEDGE_PALACE_INDEX,
    OPERATOR_ONLY_VISIBILITY, ReadScope,
};
use harw_tools::ToolExecutionContext;

/// Alle Selektoren, die `lens.ask` kennt — der vollständige, fest verdrahtete
/// Indexkatalog dieses Werkzeugs.
///
/// # Description
/// Kein Aufrufer wählt diese Liste; sie spiegelt ausschließlich, welche
/// `(index_name, visibility)`-Kombinationen `harw-lens-source` tatsächlich
/// baut (siehe den `//!`-Block dieses Moduls). Eine künftige Erweiterung
/// (weiterer Index, weitere Sichtbarkeit) ändert nur diese Konstante, nie
/// die Argumentform von `lens.ask`.
pub const KNOWN_SELECTORS: &[(&str, &str)] = &[
    (DOCS_DESIGN_INDEX, DEFAULT_VISIBILITY),
    (KNOWLEDGE_PALACE_INDEX, DEFAULT_VISIBILITY),
    (KNOWLEDGE_PALACE_INDEX, OPERATOR_ONLY_VISIBILITY),
];

/// Leitet den [`ReadScope`] eines Werkzeugaufrufs aus dem vertrauten
/// [`ToolExecutionContext`] ab — niemals aus `call.arguments`.
///
/// # Description
/// Siehe den `//!`-Block dieses Moduls für den vollständigen Befund: die
/// laufende Sandbox kennt zum Zeitpunkt dieses Knotens keinen
/// Operator-Begriff, gegen den sich `"operator-only"` freischalten ließe.
/// Diese Funktion liefert deshalb bewusst konstant genau
/// [`harw_lens::DEFAULT_VISIBILITY`] und ignoriert `context` inhaltlich
/// (siehe Signaturbegründung oben) — das ist die fail-closed Wahl, nicht die
/// vollständige. `harw-sandbox` müsste einen echten Operator-Begriff
/// ergänzen, damit diese Funktion je `"operator-only"` freigeben kann.
///
/// # Arguments
/// - `context` (`&ToolExecutionContext`): die vertraute Ausführungsgrenze
///   dieses Aufrufs. Zum Zeitpunkt dieses Knotens ungenutzt (siehe oben),
///   aber Teil der Signatur, damit ein künftiger echter Operator-Begriff
///   ohne Signaturänderung eingebaut werden kann.
///
/// # Returns
/// Einen [`ReadScope`], der ausschließlich
/// [`harw_lens::DEFAULT_VISIBILITY`] freigibt.
///
/// # Examples
/// ```rust
/// use harw_lens::DEFAULT_VISIBILITY;
/// use harw_authority::{PermissionSet, SandboxSpec, WorkspaceRegistration, WorkspaceRegistry};
/// use harw_tool_lens::scope::derive_read_scope;
/// use harw_tools::ToolExecutionContext;
/// use harw_types::{SessionId, TenantId, TurnId, WorkspaceId};
/// use std::path::PathBuf;
///
/// let base = std::env::temp_dir().join("harw-tool-lens-doctest-scope");
/// std::fs::create_dir_all(base.join("ws")).unwrap();
/// let registry = WorkspaceRegistry::build(
///     &base,
///     [WorkspaceRegistration {
///         tenant: TenantId::from_str("t"),
///         workspace: WorkspaceId::from_str("w"),
///         root: PathBuf::from("ws"),
///     }],
/// )
/// .unwrap();
/// let binding = registry
///     .resolve(&TenantId::from_str("t"), &WorkspaceId::from_str("w"))
///     .unwrap();
/// let sandbox = SandboxSpec::from_resolved(binding, PermissionSet::empty());
/// let context = ToolExecutionContext::new(SessionId::new(), TurnId::new(), sandbox);
///
/// let scope = derive_read_scope(&context);
/// assert!(scope.allows(DEFAULT_VISIBILITY));
/// assert!(!scope.allows("operator-only"));
/// ```
#[must_use]
pub fn derive_read_scope(context: &ToolExecutionContext) -> ReadScope {
    let _ = context;
    ReadScope::single(DEFAULT_VISIBILITY)
}

/// Filtert [`KNOWN_SELECTORS`] gegen einen [`ReadScope`].
///
/// # Description
/// Liefert nur die Selektoren, deren Sichtbarkeit `scope` freigibt, in der
/// Reihenfolge von [`KNOWN_SELECTORS`]. `lens.ask` befragt ausschließlich
/// diese gefilterte Liste — ein Selektor außerhalb von `scope` wird gar
/// nicht erst an [`harw_lens_federation::federated_query`] übergeben. Die
/// Sichtbarkeitsprüfung in `resolve_index`/`federated_query` bleibt trotzdem
/// als zweite, unabhängige Verteidigungslinie bestehen: siehe
/// `test_federated_query_rejects_selector_outside_scope_even_though_ask_tool_never_sends_one`
/// in `ask_tool.rs`.
///
/// # Arguments
/// - `scope` (`&ReadScope`): der über [`derive_read_scope`] ermittelte
///   Lesebereich.
///
/// # Returns
/// Die freigegebenen Selektoren als `Vec<IndexSelector>`, ggf. leer.
///
/// # Examples
/// ```rust
/// use harw_lens::{DEFAULT_VISIBILITY, ReadScope};
/// use harw_tool_lens::scope::selectors_in_scope;
///
/// let scope = ReadScope::single(DEFAULT_VISIBILITY);
/// let selectors = selectors_in_scope(&scope);
/// assert!(selectors.iter().all(|s| s.visibility == DEFAULT_VISIBILITY));
/// assert!(!selectors.is_empty());
/// ```
#[must_use]
pub fn selectors_in_scope(scope: &ReadScope) -> Vec<IndexSelector> {
    KNOWN_SELECTORS
        .iter()
        .filter(|(_, visibility)| scope.allows(visibility))
        .map(|(index_name, visibility)| IndexSelector::new(*index_name, *visibility))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestResult, ctx};
    use harw_authority::{
        Permission, PermissionSet, SandboxSpec, WorkspaceRegistration, WorkspaceRegistry,
    };
    use harw_types::{SessionId, TenantId, TurnId, WorkspaceId};
    use std::path::PathBuf;

    fn make_context(label: &str, permissions: Vec<Permission>) -> TestResult<ToolExecutionContext> {
        let base = std::env::temp_dir().join(format!(
            "harw-tool-lens-scope-tests-{}-{label}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(base.join("ws")).map_err(ctx("Workspace anlegen"))?;
        let registry = WorkspaceRegistry::build(
            &base,
            [WorkspaceRegistration {
                tenant: TenantId::from_str("t"),
                workspace: WorkspaceId::from_str("w"),
                root: PathBuf::from("ws"),
            }],
        )
        .map_err(ctx("Registry bauen"))?;
        let binding = registry
            .resolve(&TenantId::from_str("t"), &WorkspaceId::from_str("w"))
            .map_err(ctx("Workspace auflösen"))?;
        let sandbox = SandboxSpec::from_resolved(binding, PermissionSet::from_policy(permissions));
        Ok(ToolExecutionContext::new(
            SessionId::new(),
            TurnId::new(),
            sandbox,
        ))
    }

    /// Der Kern der Auflage: unabhängig davon, welche Berechtigungen der
    /// Aufrufer trägt, bleibt der abgeleitete `ReadScope` auf
    /// `DEFAULT_VISIBILITY` beschränkt -- es gibt keine Kombination aus
    /// Berechtigungen, die `"operator-only"` freischaltet, weil die Sandbox
    /// diesen Begriff nicht kennt.
    #[test]
    fn test_derive_read_scope_never_allows_operator_only_regardless_of_permissions() -> TestResult {
        let all_permissions = vec![
            Permission::ReadWorkspace,
            Permission::WriteWorkspace,
            Permission::ExecuteProcess,
            Permission::NetworkAccess,
            Permission::ReadSecrets,
            Permission::ManagePlugins,
            Permission::ReadCargoRegistry,
        ];
        let context = make_context("all-perms", all_permissions)?;

        let scope = derive_read_scope(&context);

        assert!(scope.allows(DEFAULT_VISIBILITY));
        assert!(!scope.allows(OPERATOR_ONLY_VISIBILITY));
        Ok(())
    }

    /// Ohne jede Berechtigung bleibt der Lesebereich identisch -- der
    /// abgeleitete Scope hängt nicht von `PermissionSet` ab, weil keine der
    /// dort vorhandenen Berechtigungen einen Sichtbarkeitsbegriff trägt.
    #[test]
    fn test_derive_read_scope_identical_with_no_permissions_at_all() -> TestResult {
        let context = make_context("no-perms", vec![])?;

        let scope = derive_read_scope(&context);

        assert!(scope.allows(DEFAULT_VISIBILITY));
        assert!(!scope.allows(OPERATOR_ONLY_VISIBILITY));
        Ok(())
    }

    #[test]
    fn test_selectors_in_scope_returns_only_default_visibility_selectors() {
        let scope = ReadScope::single(DEFAULT_VISIBILITY);

        let selectors = selectors_in_scope(&scope);

        assert_eq!(selectors.len(), 2);
        assert!(
            selectors
                .iter()
                .all(|selector| selector.visibility == DEFAULT_VISIBILITY)
        );
        assert!(
            selectors
                .iter()
                .any(|selector| selector.index_name == DOCS_DESIGN_INDEX)
        );
        assert!(
            selectors
                .iter()
                .any(|selector| selector.index_name == KNOWLEDGE_PALACE_INDEX)
        );
    }

    #[test]
    fn test_selectors_in_scope_empty_scope_returns_nothing() {
        let scope = ReadScope::default();

        let selectors = selectors_in_scope(&scope);

        assert!(selectors.is_empty());
    }

    #[test]
    fn test_selectors_in_scope_operator_only_scope_returns_only_palace() {
        let scope = ReadScope::single(OPERATOR_ONLY_VISIBILITY);

        let selectors = selectors_in_scope(&scope);

        assert_eq!(selectors.len(), 1);
        assert_eq!(selectors[0].index_name, KNOWLEDGE_PALACE_INDEX);
        assert_eq!(selectors[0].visibility, OPERATOR_ONLY_VISIBILITY);
    }
}
