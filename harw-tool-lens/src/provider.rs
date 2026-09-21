//! `LensToolProvider` — bündelt das eine Werkzeug dieser Crate.
//!
//! # Verantwortung
//! Enthält keinerlei Logik: die Provider-Struktur samt `tools()`,
//! `executor(name)` und `parallel_safe(name)` entsteht über
//! [`harw_tools::tool_provider!`] aus [`crate::ask_tool::LensAskTool`]. Name,
//! Spezifikation, Berechtigung stammen ausschließlich aus dem
//! `#[harw_macros::tool]`-Attribut in `ask_tool.rs` -- keine zweite Wahrheit.
//!
//! # Warum `lens.ask` nicht `parallel_safe` ist
//! `lens.ask` liest ausschließlich (kein Schreibpfad, siehe `ask_tool.rs`s
//! `//!`-Block) und wäre insofern ein Kandidat für `parallel_safe`. Es bleibt
//! trotzdem beim Makro-Default `false`: [`harw_lens_source::build_index`]
//! serialisiert konkurrierende Bau-Aufrufe für dasselbe
//! `(home, index_name, visibility)`-Tripel über eine `fs4`-Advisory-Lock,
//! aber `lens.ask` liest über [`harw_lens_query::resolve_index`] **ohne**
//! diese Sperre zu nehmen -- ein `lens.ask`, das nebenläufig zu einem
//! laufenden `harw_lens::build` desselben Index liefe, könnte einen Index in
//! einem inkonsistenten Zwischenzustand (Manifest geschrieben, Vektordatei
//! noch nicht) lesen. Diese Crate kann diese Race nicht selbst schließen
//! (Schreibpfad und Werkzeugoberfläche sind bewusst getrennt, siehe
//! `ask_tool.rs`); bis eine echte Lese-Sperre oder ein atomarer Indexwechsel
//! existiert, bleibt `parallel_safe = false` die konservative Wahl.
//!
//! # Schlüsseltypen
//! - [`LensToolProvider`] -- Unit-Struktur mit `impl ToolProvider`.
//!
//! # Berechtigungen
//! `lens.ask`: `ReadWorkspace` -- eine Näherung, siehe den `//!`-Block von
//! `crate` für den vollständigen Befund, warum keine passendere Permission
//! existiert.
//!
//! # Nebenläufigkeit
//! [`LensToolProvider`] ist eine zustandslose Unit-Struktur und damit
//! `Send + Sync + Copy`; pro `executor()`-Aufruf entsteht ein frischer `Arc`.
//!
//! # Beispiel
//! ```rust,no_run
//! use harw_extension_api::contributors::ToolProvider;
//! use harw_tool_lens::LensToolProvider;
//!
//! let provider = LensToolProvider::new();
//! assert_eq!(provider.tools().len(), 1);
//! ```

use crate::ask_tool::LensAskTool;

harw_tools::tool_provider! {
    /// Stellt das eine Werkzeug dieser Crate bereit: `lens.ask`.
    pub struct LensToolProvider {
        LensAskTool,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use harw_extension_api::contributors::ToolProvider as _;
    use harw_authority::Permission;
    use harw_tools::ToolName;

    /// Genau ein Werkzeug wird beworben -- die begründete Entscheidung aus
    /// `ask_tool.rs`s `//!`-Block, kein Versehen.
    #[test]
    fn test_provider_lists_exactly_one_tool() {
        let provider = LensToolProvider::new();
        let tools = provider.tools();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].name(), "lens.ask");
    }

    /// Das beworbene Werkzeug ist auch auflösbar; unbekannte Namen liefern
    /// keinen Executor.
    #[test]
    fn test_provider_resolves_lens_ask_and_nothing_else() {
        let provider = LensToolProvider::new();
        assert!(provider.executor(&ToolName::new("lens.ask")).is_some());
        assert!(provider.executor(&ToolName::new("lens.build")).is_none());
        assert!(provider.executor(&ToolName::new("lens.unbekannt")).is_none());
    }

    /// `lens.ask` deklariert `ReadWorkspace`.
    #[test]
    fn test_provider_declares_read_workspace_permission() {
        let permissions: Vec<Option<Permission>> =
            LensToolProvider::TOOL_PERMISSIONS.to_vec();
        assert_eq!(permissions, vec![Some(Permission::ReadWorkspace)]);
    }

    /// **Beleg: es gibt kein Werkzeug, das schreibt.** Weder `lens.build`
    /// noch irgendein anderer Name mit `write`/`build`/`index` im Namen wird
    /// von diesem Provider bedient -- der einzige beworbene Name ist
    /// `lens.ask`.
    #[test]
    fn test_provider_exposes_no_write_capable_tool() {
        let provider = LensToolProvider::new();
        let tools = provider.tools();
        let names: Vec<&str> = tools.iter().map(|spec| spec.name()).collect();
        assert_eq!(names, vec!["lens.ask"]);
        for forbidden in ["lens.build", "lens.write", "lens.index"] {
            assert!(
                provider.executor(&ToolName::new(forbidden)).is_none(),
                "{forbidden} darf nicht existieren"
            );
        }
    }

    /// Der Provider ist zustandslos: `new()` und die Unit-Konstruktion sind
    /// gleichwertig.
    #[test]
    fn test_provider_is_default_constructible() {
        let from_new = LensToolProvider::new();
        let unit = LensToolProvider;
        assert_eq!(format!("{from_new:?}"), format!("{unit:?}"));
    }
}
