//! Mit dem Binary ausgelieferte Agenten- und Skill-Definitionen.
//!
//! # Verantwortung
//! Dieses Modul besitzt die eingebettete Startausstattung des Root-Space: die
//! Delegationshierarchie (Child-Orchestratoren und Worker als
//! DSL-Definitionen `agents/<name>/definition.toml` mit `system.md` als
//! Arbeitsanweisung, Plan R9 Teil B; nur `coding-orchestrator` bleibt ein
//! Legacy-`agent.toml`), die Skills, auf die diese Agenten verweisen, sowie
//! eigenständige Anleitungs-Skills (`skill.toml` + `instructions.md`, u. a.
//! `rust-*`, deren sprachunabhängige Varianten wie
//! `error-type-design`, `version-bump`). Es besitzt **nicht** das Schreiben — das leistet
//! [`crate::scaffold::ensure_home`].
//!
//! # Schlüsseltypen
//! - [`BundledFile`] — ein eingebetteter Dateiinhalt samt Zielpfad.
//!
//! # Abgrenzung
//! Die UIA ist bewusst **nicht** Teil des Bundles: sie entsteht pro Nutzer über
//! `harw uia new`. Das Bundle liefert nur, was unterhalb der UIA delegiert wird.
//!
//! # Nebenläufigkeit
//! Ausschließlich `&'static`-Daten ohne innere Veränderlichkeit: `Send + Sync`,
//! beliebig teilbar, keine Sperren.
//!
//! # Fehler
//! Dieses Modul erzeugt keine Fehler; es liefert nur Konstanten.
//!
//! # Beispiele
//! ```rust
//! let agents = harw_home::bundled_files()
//!     .iter()
//!     .filter(|file| file.relative_path.starts_with("agents/"))
//!     .count();
//! assert!(agents > 0);
//! ```

use std::path::PathBuf;

/// Eine eingebettete Bundle-Datei samt ihrem Ziel im Root-Space.
///
/// # Description
/// `relative_path` ist stets mit `/` getrennt, unabhängig von der Plattform;
/// [`Self::target_in`] setzt daraus einen plattformgerechten Pfad zusammen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BundledFile {
    /// Pfad relativ zum Root-Space, z. B. `"agents/debugger/agent.toml"`.
    pub relative_path: &'static str,
    /// Vollständiger Dateiinhalt, zur Compile-Zeit eingebettet.
    pub contents: &'static str,
}

impl BundledFile {
    /// Setzt den Zielpfad dieser Datei unterhalb von `home` zusammen.
    ///
    /// # Arguments
    /// - `home` (`&std::path::Path`): Root-Space, üblicherweise `~/.harw`.
    ///
    /// # Returns
    /// Der plattformgerechte Zielpfad.
    ///
    /// # Concurrency
    /// Reine Berechnung, aus jedem Thread aufrufbar.
    ///
    /// # Examples
    /// ```rust
    /// use std::path::Path;
    ///
    /// let file = harw_home::bundled_files()[0];
    /// assert!(file.target_in(Path::new("/tmp/harw")).starts_with("/tmp/harw"));
    /// ```
    #[must_use]
    pub fn target_in(&self, home: &std::path::Path) -> PathBuf {
        let mut target = home.to_path_buf();
        for segment in self.relative_path.split('/') {
            target.push(segment);
        }
        target
    }
}

/// Liefert alle eingebetteten Agenten- und Skill-Dateien.
///
/// # Description
/// Die Reihenfolge ist stabil (Pfad-sortiert) und Teil des beobachtbaren
/// Verhaltens, damit ein Scaffold-Bericht deterministisch bleibt.
///
/// # Returns
/// Ein `&'static`-Slice aller Bundle-Dateien.
///
/// # Concurrency
/// Gibt nur statische Daten zurück; aus jedem Thread aufrufbar.
///
/// # Examples
/// ```rust
/// assert!(
///     harw_home::bundled_files()
///         .iter()
///         .any(|file| file.relative_path == "agents/coding-orchestrator/agent.toml")
/// );
/// ```
#[must_use]
pub fn bundled_files() -> &'static [BundledFile] {
    BUNDLED_FILES
}

/// Die eingebettete Startausstattung. Neue Dateien unter `assets/` müssen hier
/// eingetragen werden; `test_every_asset_file_is_embedded` erzwingt das.
static BUNDLED_FILES: &[BundledFile] = &[
    BundledFile {
        relative_path: "agents/business-author/definition.toml",
        contents: include_str!("../assets/agents/business-author/definition.toml"),
    },
    BundledFile {
        relative_path: "agents/business-author/system.md",
        contents: include_str!("../assets/agents/business-author/system.md"),
    },
    BundledFile {
        relative_path: "agents/business-reviewer/definition.toml",
        contents: include_str!("../assets/agents/business-reviewer/definition.toml"),
    },
    BundledFile {
        relative_path: "agents/business-reviewer/system.md",
        contents: include_str!("../assets/agents/business-reviewer/system.md"),
    },
    BundledFile {
        relative_path: "agents/coding-orchestrator/agent.toml",
        contents: include_str!("../assets/agents/coding-orchestrator/agent.toml"),
    },
    BundledFile {
        relative_path: "agents/coding-orchestrator/system.md",
        contents: include_str!("../assets/agents/coding-orchestrator/system.md"),
    },
    BundledFile {
        relative_path: "agents/debugger/definition.toml",
        contents: include_str!("../assets/agents/debugger/definition.toml"),
    },
    BundledFile {
        relative_path: "agents/debugger/system.md",
        contents: include_str!("../assets/agents/debugger/system.md"),
    },
    BundledFile {
        relative_path: "agents/debug-orchestrator/definition.toml",
        contents: include_str!("../assets/agents/debug-orchestrator/definition.toml"),
    },
    BundledFile {
        relative_path: "agents/debug-orchestrator/system.md",
        contents: include_str!("../assets/agents/debug-orchestrator/system.md"),
    },
    BundledFile {
        relative_path: "agents/dependency-analyst/definition.toml",
        contents: include_str!("../assets/agents/dependency-analyst/definition.toml"),
    },
    BundledFile {
        relative_path: "agents/dependency-analyst/system.md",
        contents: include_str!("../assets/agents/dependency-analyst/system.md"),
    },
    BundledFile {
        relative_path: "agents/dependency-research-orchestrator/definition.toml",
        contents: include_str!("../assets/agents/dependency-research-orchestrator/definition.toml"),
    },
    BundledFile {
        relative_path: "agents/dependency-research-orchestrator/system.md",
        contents: include_str!("../assets/agents/dependency-research-orchestrator/system.md"),
    },
    BundledFile {
        relative_path: "agents/dependency-security-reviewer/definition.toml",
        contents: include_str!("../assets/agents/dependency-security-reviewer/definition.toml"),
    },
    BundledFile {
        relative_path: "agents/dependency-security-reviewer/system.md",
        contents: include_str!("../assets/agents/dependency-security-reviewer/system.md"),
    },
    BundledFile {
        relative_path: "agents/implementation-orchestrator/definition.toml",
        contents: include_str!("../assets/agents/implementation-orchestrator/definition.toml"),
    },
    BundledFile {
        relative_path: "agents/implementation-orchestrator/system.md",
        contents: include_str!("../assets/agents/implementation-orchestrator/system.md"),
    },
    BundledFile {
        relative_path: "agents/incident-triager/definition.toml",
        contents: include_str!("../assets/agents/incident-triager/definition.toml"),
    },
    BundledFile {
        relative_path: "agents/incident-triager/system.md",
        contents: include_str!("../assets/agents/incident-triager/system.md"),
    },
    BundledFile {
        relative_path: "agents/latex-writer/definition.toml",
        contents: include_str!("../assets/agents/latex-writer/definition.toml"),
    },
    BundledFile {
        relative_path: "agents/latex-writer/system.md",
        contents: include_str!("../assets/agents/latex-writer/system.md"),
    },
    BundledFile {
        relative_path: "agents/log-analyst/definition.toml",
        contents: include_str!("../assets/agents/log-analyst/definition.toml"),
    },
    BundledFile {
        relative_path: "agents/log-analyst/system.md",
        contents: include_str!("../assets/agents/log-analyst/system.md"),
    },
    BundledFile {
        relative_path: "agents/matrix-scenario-author/definition.toml",
        contents: include_str!("../assets/agents/matrix-scenario-author/definition.toml"),
    },
    BundledFile {
        relative_path: "agents/matrix-scenario-author/system.md",
        contents: include_str!("../assets/agents/matrix-scenario-author/system.md"),
    },
    BundledFile {
        relative_path: "agents/refactorer/definition.toml",
        contents: include_str!("../assets/agents/refactorer/definition.toml"),
    },
    BundledFile {
        relative_path: "agents/refactorer/system.md",
        contents: include_str!("../assets/agents/refactorer/system.md"),
    },
    BundledFile {
        relative_path: "agents/rust-implementer/definition.toml",
        contents: include_str!("../assets/agents/rust-implementer/definition.toml"),
    },
    BundledFile {
        relative_path: "agents/rust-implementer/system.md",
        contents: include_str!("../assets/agents/rust-implementer/system.md"),
    },
    BundledFile {
        relative_path: "agents/secret-scanner/definition.toml",
        contents: include_str!("../assets/agents/secret-scanner/definition.toml"),
    },
    BundledFile {
        relative_path: "agents/secret-scanner/system.md",
        contents: include_str!("../assets/agents/secret-scanner/system.md"),
    },
    BundledFile {
        relative_path: "agents/security-auditor/definition.toml",
        contents: include_str!("../assets/agents/security-auditor/definition.toml"),
    },
    BundledFile {
        relative_path: "agents/security-auditor/system.md",
        contents: include_str!("../assets/agents/security-auditor/system.md"),
    },
    BundledFile {
        relative_path: "agents/security-inspection-orchestrator/definition.toml",
        contents: include_str!("../assets/agents/security-inspection-orchestrator/definition.toml"),
    },
    BundledFile {
        relative_path: "agents/security-inspection-orchestrator/system.md",
        contents: include_str!("../assets/agents/security-inspection-orchestrator/system.md"),
    },
    BundledFile {
        relative_path: "agents/slides-builder/definition.toml",
        contents: include_str!("../assets/agents/slides-builder/definition.toml"),
    },
    BundledFile {
        relative_path: "agents/slides-builder/system.md",
        contents: include_str!("../assets/agents/slides-builder/system.md"),
    },
    BundledFile {
        relative_path: "agents/source-researcher/definition.toml",
        contents: include_str!("../assets/agents/source-researcher/definition.toml"),
    },
    BundledFile {
        relative_path: "agents/source-researcher/system.md",
        contents: include_str!("../assets/agents/source-researcher/system.md"),
    },
    BundledFile {
        relative_path: "agents/system-observer/definition.toml",
        contents: include_str!("../assets/agents/system-observer/definition.toml"),
    },
    BundledFile {
        relative_path: "agents/system-observer/system.md",
        contents: include_str!("../assets/agents/system-observer/system.md"),
    },
    BundledFile {
        relative_path: "agents/test-engineer/definition.toml",
        contents: include_str!("../assets/agents/test-engineer/definition.toml"),
    },
    BundledFile {
        relative_path: "agents/test-engineer/system.md",
        contents: include_str!("../assets/agents/test-engineer/system.md"),
    },
    BundledFile {
        relative_path: "skills/author-review-pipeline/instructions.md",
        contents: include_str!("../assets/skills/author-review-pipeline/instructions.md"),
    },
    BundledFile {
        relative_path: "skills/author-review-pipeline/skill.toml",
        contents: include_str!("../assets/skills/author-review-pipeline/skill.toml"),
    },
    BundledFile {
        relative_path: "skills/business-writing-pyramid/instructions.md",
        contents: include_str!("../assets/skills/business-writing-pyramid/instructions.md"),
    },
    BundledFile {
        relative_path: "skills/business-writing-pyramid/skill.toml",
        contents: include_str!("../assets/skills/business-writing-pyramid/skill.toml"),
    },
    BundledFile {
        relative_path: "skills/cli-command-surface-map/instructions.md",
        contents: include_str!("../assets/skills/cli-command-surface-map/instructions.md"),
    },
    BundledFile {
        relative_path: "skills/cli-command-surface-map/skill.toml",
        contents: include_str!("../assets/skills/cli-command-surface-map/skill.toml"),
    },
    BundledFile {
        relative_path: "skills/cli-doc-drift-review/instructions.md",
        contents: include_str!("../assets/skills/cli-doc-drift-review/instructions.md"),
    },
    BundledFile {
        relative_path: "skills/cli-doc-drift-review/skill.toml",
        contents: include_str!("../assets/skills/cli-doc-drift-review/skill.toml"),
    },
    BundledFile {
        relative_path: "skills/cli-live-review/instructions.md",
        contents: include_str!("../assets/skills/cli-live-review/instructions.md"),
    },
    BundledFile {
        relative_path: "skills/cli-live-review/skill.toml",
        contents: include_str!("../assets/skills/cli-live-review/skill.toml"),
    },
    BundledFile {
        relative_path: "skills/contract-fanout-migration/instructions.md",
        contents: include_str!("../assets/skills/contract-fanout-migration/instructions.md"),
    },
    BundledFile {
        relative_path: "skills/contract-fanout-migration/skill.toml",
        contents: include_str!("../assets/skills/contract-fanout-migration/skill.toml"),
    },
    BundledFile {
        relative_path: "skills/debugging/instructions.md",
        contents: include_str!("../assets/skills/debugging/instructions.md"),
    },
    BundledFile {
        relative_path: "skills/debugging/skill.toml",
        contents: include_str!("../assets/skills/debugging/skill.toml"),
    },
    BundledFile {
        relative_path: "skills/dependency-add-and-research/instructions.md",
        contents: include_str!("../assets/skills/dependency-add-and-research/instructions.md"),
    },
    BundledFile {
        relative_path: "skills/dependency-add-and-research/skill.toml",
        contents: include_str!("../assets/skills/dependency-add-and-research/skill.toml"),
    },
    BundledFile {
        relative_path: "skills/dependency-research/instructions.md",
        contents: include_str!("../assets/skills/dependency-research/instructions.md"),
    },
    BundledFile {
        relative_path: "skills/dependency-research/skill.toml",
        contents: include_str!("../assets/skills/dependency-research/skill.toml"),
    },
    BundledFile {
        relative_path: "skills/error-propagation/instructions.md",
        contents: include_str!("../assets/skills/error-propagation/instructions.md"),
    },
    BundledFile {
        relative_path: "skills/error-propagation/skill.toml",
        contents: include_str!("../assets/skills/error-propagation/skill.toml"),
    },
    BundledFile {
        relative_path: "skills/error-type-design/instructions.md",
        contents: include_str!("../assets/skills/error-type-design/instructions.md"),
    },
    BundledFile {
        relative_path: "skills/error-type-design/skill.toml",
        contents: include_str!("../assets/skills/error-type-design/skill.toml"),
    },
    BundledFile {
        relative_path: "skills/exhaustive-case-handling/instructions.md",
        contents: include_str!("../assets/skills/exhaustive-case-handling/instructions.md"),
    },
    BundledFile {
        relative_path: "skills/exhaustive-case-handling/skill.toml",
        contents: include_str!("../assets/skills/exhaustive-case-handling/skill.toml"),
    },
    BundledFile {
        relative_path: "skills/function-signature-design/instructions.md",
        contents: include_str!("../assets/skills/function-signature-design/instructions.md"),
    },
    BundledFile {
        relative_path: "skills/function-signature-design/skill.toml",
        contents: include_str!("../assets/skills/function-signature-design/skill.toml"),
    },
    // PL-97 D4: Harw-DoD-Lese-Skill der Werkzeuge `dod.*` samt Triggern.
    BundledFile {
        relative_path: "skills/harw-dod/instructions.md",
        contents: include_str!("../assets/skills/harw-dod/instructions.md"),
    },
    BundledFile {
        relative_path: "skills/harw-dod/skill.toml",
        contents: include_str!("../assets/skills/harw-dod/skill.toml"),
    },
    BundledFile {
        relative_path: "skills/harw-dod/triggers.toml",
        contents: include_str!("../assets/skills/harw-dod/triggers.toml"),
    },
    BundledFile {
        relative_path: "skills/implementation/instructions.md",
        contents: include_str!("../assets/skills/implementation/instructions.md"),
    },
    BundledFile {
        relative_path: "skills/implementation/skill.toml",
        contents: include_str!("../assets/skills/implementation/skill.toml"),
    },
    BundledFile {
        relative_path: "skills/incident-response/instructions.md",
        contents: include_str!("../assets/skills/incident-response/instructions.md"),
    },
    BundledFile {
        relative_path: "skills/incident-response/skill.toml",
        contents: include_str!("../assets/skills/incident-response/skill.toml"),
    },
    BundledFile {
        relative_path: "skills/interfaces-and-abstraction/instructions.md",
        contents: include_str!("../assets/skills/interfaces-and-abstraction/instructions.md"),
    },
    BundledFile {
        relative_path: "skills/interfaces-and-abstraction/skill.toml",
        contents: include_str!("../assets/skills/interfaces-and-abstraction/skill.toml"),
    },
    BundledFile {
        relative_path: "skills/latex-report/instructions.md",
        contents: include_str!("../assets/skills/latex-report/instructions.md"),
    },
    BundledFile {
        relative_path: "skills/latex-report/skill.toml",
        contents: include_str!("../assets/skills/latex-report/skill.toml"),
    },
    BundledFile {
        relative_path: "skills/latex-report/templates/bericht.tex",
        contents: include_str!("../assets/skills/latex-report/templates/bericht.tex"),
    },
    BundledFile {
        relative_path: "skills/latex-report/templates/business-paper.tex",
        contents: include_str!("../assets/skills/latex-report/templates/business-paper.tex"),
    },
    BundledFile {
        relative_path: "skills/latex-report/templates/handbuch.tex",
        contents: include_str!("../assets/skills/latex-report/templates/handbuch.tex"),
    },
    BundledFile {
        relative_path: "skills/latex-report/templates/harw-report.sty",
        contents: include_str!("../assets/skills/latex-report/templates/harw-report.sty"),
    },
    BundledFile {
        relative_path: "skills/latex-writing/instructions.md",
        contents: include_str!("../assets/skills/latex-writing/instructions.md"),
    },
    BundledFile {
        relative_path: "skills/latex-writing/skill.toml",
        contents: include_str!("../assets/skills/latex-writing/skill.toml"),
    },
    BundledFile {
        relative_path: "skills/learning-loop/instructions.md",
        contents: include_str!("../assets/skills/learning-loop/instructions.md"),
    },
    BundledFile {
        relative_path: "skills/learning-loop/skill.toml",
        contents: include_str!("../assets/skills/learning-loop/skill.toml"),
    },
    BundledFile {
        relative_path: "skills/loops-and-control-flow/instructions.md",
        contents: include_str!("../assets/skills/loops-and-control-flow/instructions.md"),
    },
    BundledFile {
        relative_path: "skills/loops-and-control-flow/skill.toml",
        contents: include_str!("../assets/skills/loops-and-control-flow/skill.toml"),
    },
    BundledFile {
        relative_path: "skills/matrix-scenario-design/instructions.md",
        contents: include_str!("../assets/skills/matrix-scenario-design/instructions.md"),
    },
    BundledFile {
        relative_path: "skills/matrix-scenario-design/skill.toml",
        contents: include_str!("../assets/skills/matrix-scenario-design/skill.toml"),
    },
    BundledFile {
        relative_path: "skills/null-and-optional-handling/instructions.md",
        contents: include_str!("../assets/skills/null-and-optional-handling/instructions.md"),
    },
    BundledFile {
        relative_path: "skills/null-and-optional-handling/skill.toml",
        contents: include_str!("../assets/skills/null-and-optional-handling/skill.toml"),
    },
    BundledFile {
        relative_path: "skills/ownership-and-resource-lifetimes/instructions.md",
        contents: include_str!("../assets/skills/ownership-and-resource-lifetimes/instructions.md"),
    },
    BundledFile {
        relative_path: "skills/ownership-and-resource-lifetimes/skill.toml",
        contents: include_str!("../assets/skills/ownership-and-resource-lifetimes/skill.toml"),
    },
    BundledFile {
        relative_path: "skills/planning/instructions.md",
        contents: include_str!("../assets/skills/planning/instructions.md"),
    },
    BundledFile {
        relative_path: "skills/planning/skill.toml",
        contents: include_str!("../assets/skills/planning/skill.toml"),
    },
    BundledFile {
        relative_path: "skills/rust-arc-sharing/instructions.md",
        contents: include_str!("../assets/skills/rust-arc-sharing/instructions.md"),
    },
    BundledFile {
        relative_path: "skills/rust-arc-sharing/skill.toml",
        contents: include_str!("../assets/skills/rust-arc-sharing/skill.toml"),
    },
    BundledFile {
        relative_path: "skills/rust-as-ref/instructions.md",
        contents: include_str!("../assets/skills/rust-as-ref/instructions.md"),
    },
    BundledFile {
        relative_path: "skills/rust-as-ref/skill.toml",
        contents: include_str!("../assets/skills/rust-as-ref/skill.toml"),
    },
    BundledFile {
        relative_path: "skills/rust-borrow-checker-e0505/instructions.md",
        contents: include_str!("../assets/skills/rust-borrow-checker-e0505/instructions.md"),
    },
    BundledFile {
        relative_path: "skills/rust-borrow-checker-e0505/skill.toml",
        contents: include_str!("../assets/skills/rust-borrow-checker-e0505/skill.toml"),
    },
    BundledFile {
        relative_path: "skills/rust-borrow-vs-owned-params/instructions.md",
        contents: include_str!("../assets/skills/rust-borrow-vs-owned-params/instructions.md"),
    },
    BundledFile {
        relative_path: "skills/rust-borrow-vs-owned-params/skill.toml",
        contents: include_str!("../assets/skills/rust-borrow-vs-owned-params/skill.toml"),
    },
    BundledFile {
        relative_path: "skills/rust-box-dyn/instructions.md",
        contents: include_str!("../assets/skills/rust-box-dyn/instructions.md"),
    },
    BundledFile {
        relative_path: "skills/rust-box-dyn/skill.toml",
        contents: include_str!("../assets/skills/rust-box-dyn/skill.toml"),
    },
    BundledFile {
        relative_path: "skills/rust-cargo-add-and-dep-research/instructions.md",
        contents: include_str!("../assets/skills/rust-cargo-add-and-dep-research/instructions.md"),
    },
    BundledFile {
        relative_path: "skills/rust-cargo-add-and-dep-research/skill.toml",
        contents: include_str!("../assets/skills/rust-cargo-add-and-dep-research/skill.toml"),
    },
    BundledFile {
        relative_path: "skills/rust-cow/instructions.md",
        contents: include_str!("../assets/skills/rust-cow/instructions.md"),
    },
    BundledFile {
        relative_path: "skills/rust-cow/skill.toml",
        contents: include_str!("../assets/skills/rust-cow/skill.toml"),
    },
    BundledFile {
        relative_path: "skills/rust-deref-coercion/instructions.md",
        contents: include_str!("../assets/skills/rust-deref-coercion/instructions.md"),
    },
    BundledFile {
        relative_path: "skills/rust-deref-coercion/skill.toml",
        contents: include_str!("../assets/skills/rust-deref-coercion/skill.toml"),
    },
    BundledFile {
        relative_path: "skills/rust-deref-star/instructions.md",
        contents: include_str!("../assets/skills/rust-deref-star/instructions.md"),
    },
    BundledFile {
        relative_path: "skills/rust-deref-star/skill.toml",
        contents: include_str!("../assets/skills/rust-deref-star/skill.toml"),
    },
    BundledFile {
        relative_path: "skills/rust-error-enum-design/instructions.md",
        contents: include_str!("../assets/skills/rust-error-enum-design/instructions.md"),
    },
    BundledFile {
        relative_path: "skills/rust-error-enum-design/skill.toml",
        contents: include_str!("../assets/skills/rust-error-enum-design/skill.toml"),
    },
    BundledFile {
        relative_path: "skills/rust-error-from-impls/instructions.md",
        contents: include_str!("../assets/skills/rust-error-from-impls/instructions.md"),
    },
    BundledFile {
        relative_path: "skills/rust-error-from-impls/skill.toml",
        contents: include_str!("../assets/skills/rust-error-from-impls/skill.toml"),
    },
    BundledFile {
        relative_path: "skills/rust-explicit-lifetimes/instructions.md",
        contents: include_str!("../assets/skills/rust-explicit-lifetimes/instructions.md"),
    },
    BundledFile {
        relative_path: "skills/rust-explicit-lifetimes/skill.toml",
        contents: include_str!("../assets/skills/rust-explicit-lifetimes/skill.toml"),
    },
    BundledFile {
        relative_path: "skills/rust-generic-functions/instructions.md",
        contents: include_str!("../assets/skills/rust-generic-functions/instructions.md"),
    },
    BundledFile {
        relative_path: "skills/rust-generic-functions/skill.toml",
        contents: include_str!("../assets/skills/rust-generic-functions/skill.toml"),
    },
    BundledFile {
        relative_path: "skills/rust-if-let-let-else/instructions.md",
        contents: include_str!("../assets/skills/rust-if-let-let-else/instructions.md"),
    },
    BundledFile {
        relative_path: "skills/rust-if-let-let-else/skill.toml",
        contents: include_str!("../assets/skills/rust-if-let-let-else/skill.toml"),
    },
    BundledFile {
        relative_path: "skills/rust-lifetime-elision/instructions.md",
        contents: include_str!("../assets/skills/rust-lifetime-elision/instructions.md"),
    },
    BundledFile {
        relative_path: "skills/rust-lifetime-elision/skill.toml",
        contents: include_str!("../assets/skills/rust-lifetime-elision/skill.toml"),
    },
    BundledFile {
        relative_path: "skills/rust-loops-and-labels/instructions.md",
        contents: include_str!("../assets/skills/rust-loops-and-labels/instructions.md"),
    },
    BundledFile {
        relative_path: "skills/rust-loops-and-labels/skill.toml",
        contents: include_str!("../assets/skills/rust-loops-and-labels/skill.toml"),
    },
    BundledFile {
        relative_path: "skills/rust-map-err/instructions.md",
        contents: include_str!("../assets/skills/rust-map-err/instructions.md"),
    },
    BundledFile {
        relative_path: "skills/rust-map-err/skill.toml",
        contents: include_str!("../assets/skills/rust-map-err/skill.toml"),
    },
    BundledFile {
        relative_path: "skills/rust-match-exhaustive/instructions.md",
        contents: include_str!("../assets/skills/rust-match-exhaustive/instructions.md"),
    },
    BundledFile {
        relative_path: "skills/rust-match-exhaustive/skill.toml",
        contents: include_str!("../assets/skills/rust-match-exhaustive/skill.toml"),
    },
    BundledFile {
        relative_path: "skills/rust-monomorphization-vs-dyn/instructions.md",
        contents: include_str!("../assets/skills/rust-monomorphization-vs-dyn/instructions.md"),
    },
    BundledFile {
        relative_path: "skills/rust-monomorphization-vs-dyn/skill.toml",
        contents: include_str!("../assets/skills/rust-monomorphization-vs-dyn/skill.toml"),
    },
    BundledFile {
        relative_path: "skills/rust-move-semantics/instructions.md",
        contents: include_str!("../assets/skills/rust-move-semantics/instructions.md"),
    },
    BundledFile {
        relative_path: "skills/rust-move-semantics/skill.toml",
        contents: include_str!("../assets/skills/rust-move-semantics/skill.toml"),
    },
    BundledFile {
        relative_path: "skills/rust-ok-or-else/instructions.md",
        contents: include_str!("../assets/skills/rust-ok-or-else/instructions.md"),
    },
    BundledFile {
        relative_path: "skills/rust-ok-or-else/skill.toml",
        contents: include_str!("../assets/skills/rust-ok-or-else/skill.toml"),
    },
    BundledFile {
        relative_path: "skills/rust-phantomdata-typestate/instructions.md",
        contents: include_str!("../assets/skills/rust-phantomdata-typestate/instructions.md"),
    },
    BundledFile {
        relative_path: "skills/rust-phantomdata-typestate/skill.toml",
        contents: include_str!("../assets/skills/rust-phantomdata-typestate/skill.toml"),
    },
    BundledFile {
        relative_path: "skills/rust-question-mark-operator/instructions.md",
        contents: include_str!("../assets/skills/rust-question-mark-operator/instructions.md"),
    },
    BundledFile {
        relative_path: "skills/rust-question-mark-operator/skill.toml",
        contents: include_str!("../assets/skills/rust-question-mark-operator/skill.toml"),
    },
    BundledFile {
        relative_path: "skills/rust-references-vs-raw-pointers/instructions.md",
        contents: include_str!("../assets/skills/rust-references-vs-raw-pointers/instructions.md"),
    },
    BundledFile {
        relative_path: "skills/rust-references-vs-raw-pointers/skill.toml",
        contents: include_str!("../assets/skills/rust-references-vs-raw-pointers/skill.toml"),
    },
    BundledFile {
        relative_path: "skills/rust-result-type-alias/instructions.md",
        contents: include_str!("../assets/skills/rust-result-type-alias/instructions.md"),
    },
    BundledFile {
        relative_path: "skills/rust-result-type-alias/skill.toml",
        contents: include_str!("../assets/skills/rust-result-type-alias/skill.toml"),
    },
    BundledFile {
        relative_path: "skills/rust-struct-lifetimes/instructions.md",
        contents: include_str!("../assets/skills/rust-struct-lifetimes/instructions.md"),
    },
    BundledFile {
        relative_path: "skills/rust-struct-lifetimes/skill.toml",
        contents: include_str!("../assets/skills/rust-struct-lifetimes/skill.toml"),
    },
    BundledFile {
        relative_path: "skills/rust-to-owned-vs-clone/instructions.md",
        contents: include_str!("../assets/skills/rust-to-owned-vs-clone/instructions.md"),
    },
    BundledFile {
        relative_path: "skills/rust-to-owned-vs-clone/skill.toml",
        contents: include_str!("../assets/skills/rust-to-owned-vs-clone/skill.toml"),
    },
    BundledFile {
        relative_path: "skills/rust-trait-bounds/instructions.md",
        contents: include_str!("../assets/skills/rust-trait-bounds/instructions.md"),
    },
    BundledFile {
        relative_path: "skills/rust-trait-bounds/skill.toml",
        contents: include_str!("../assets/skills/rust-trait-bounds/skill.toml"),
    },
    BundledFile {
        relative_path: "skills/rust-traits-defining/instructions.md",
        contents: include_str!("../assets/skills/rust-traits-defining/instructions.md"),
    },
    BundledFile {
        relative_path: "skills/rust-traits-defining/skill.toml",
        contents: include_str!("../assets/skills/rust-traits-defining/skill.toml"),
    },
    BundledFile {
        relative_path: "skills/rust-while-let/instructions.md",
        contents: include_str!("../assets/skills/rust-while-let/instructions.md"),
    },
    BundledFile {
        relative_path: "skills/rust-while-let/skill.toml",
        contents: include_str!("../assets/skills/rust-while-let/skill.toml"),
    },
    BundledFile {
        relative_path: "skills/rust-zeroize-secrets/instructions.md",
        contents: include_str!("../assets/skills/rust-zeroize-secrets/instructions.md"),
    },
    BundledFile {
        relative_path: "skills/rust-zeroize-secrets/skill.toml",
        contents: include_str!("../assets/skills/rust-zeroize-secrets/skill.toml"),
    },
    BundledFile {
        relative_path: "skills/schematron-beschaffung/instructions.md",
        contents: include_str!("../assets/skills/schematron-beschaffung/instructions.md"),
    },
    BundledFile {
        relative_path: "skills/schematron-beschaffung/skill.toml",
        contents: include_str!("../assets/skills/schematron-beschaffung/skill.toml"),
    },
    BundledFile {
        relative_path: "skills/secret-handling/instructions.md",
        contents: include_str!("../assets/skills/secret-handling/instructions.md"),
    },
    BundledFile {
        relative_path: "skills/secret-handling/skill.toml",
        contents: include_str!("../assets/skills/secret-handling/skill.toml"),
    },
    BundledFile {
        relative_path: "skills/security-inspection/instructions.md",
        contents: include_str!("../assets/skills/security-inspection/instructions.md"),
    },
    BundledFile {
        relative_path: "skills/security-inspection/skill.toml",
        contents: include_str!("../assets/skills/security-inspection/skill.toml"),
    },
    BundledFile {
        relative_path: "skills/server-operations/instructions.md",
        contents: include_str!("../assets/skills/server-operations/instructions.md"),
    },
    BundledFile {
        relative_path: "skills/server-operations/skill.toml",
        contents: include_str!("../assets/skills/server-operations/skill.toml"),
    },
    BundledFile {
        relative_path: "skills/shared-state-and-concurrency/instructions.md",
        contents: include_str!("../assets/skills/shared-state-and-concurrency/instructions.md"),
    },
    BundledFile {
        relative_path: "skills/shared-state-and-concurrency/skill.toml",
        contents: include_str!("../assets/skills/shared-state-and-concurrency/skill.toml"),
    },
    BundledFile {
        relative_path: "skills/typestate-and-illegal-states/instructions.md",
        contents: include_str!("../assets/skills/typestate-and-illegal-states/instructions.md"),
    },
    BundledFile {
        relative_path: "skills/typestate-and-illegal-states/skill.toml",
        contents: include_str!("../assets/skills/typestate-and-illegal-states/skill.toml"),
    },
    BundledFile {
        relative_path: "skills/verification/instructions.md",
        contents: include_str!("../assets/skills/verification/instructions.md"),
    },
    BundledFile {
        relative_path: "skills/verification/skill.toml",
        contents: include_str!("../assets/skills/verification/skill.toml"),
    },
    BundledFile {
        relative_path: "skills/version-bump/instructions.md",
        contents: include_str!("../assets/skills/version-bump/instructions.md"),
    },
    BundledFile {
        relative_path: "skills/version-bump/skill.toml",
        contents: include_str!("../assets/skills/version-bump/skill.toml"),
    },
    BundledFile {
        relative_path: "skills/xelatex-compile/instructions.md",
        contents: include_str!("../assets/skills/xelatex-compile/instructions.md"),
    },
    BundledFile {
        relative_path: "skills/xelatex-compile/skill.toml",
        contents: include_str!("../assets/skills/xelatex-compile/skill.toml"),
    },
    // ── Analyse-Familie (Plan R9, Teil D) ─────────────────────────────────
    // Zehn Agenten (`definition.toml` + `system.md`) und ihre Methoden-Skills
    // als ein zusammenhängender Block; `tests/analysis_family.rs` prüft sie.
    BundledFile {
        relative_path: "agents/evidence-collector/definition.toml",
        contents: include_str!("../assets/agents/evidence-collector/definition.toml"),
    },
    BundledFile {
        relative_path: "agents/evidence-collector/system.md",
        contents: include_str!("../assets/agents/evidence-collector/system.md"),
    },
    BundledFile {
        relative_path: "agents/evidence-critic/definition.toml",
        contents: include_str!("../assets/agents/evidence-critic/definition.toml"),
    },
    BundledFile {
        relative_path: "agents/evidence-critic/system.md",
        contents: include_str!("../assets/agents/evidence-critic/system.md"),
    },
    BundledFile {
        relative_path: "agents/evidence-review-orchestrator/definition.toml",
        contents: include_str!("../assets/agents/evidence-review-orchestrator/definition.toml"),
    },
    BundledFile {
        relative_path: "agents/evidence-review-orchestrator/system.md",
        contents: include_str!("../assets/agents/evidence-review-orchestrator/system.md"),
    },
    BundledFile {
        relative_path: "agents/intel-analysis-orchestrator/definition.toml",
        contents: include_str!("../assets/agents/intel-analysis-orchestrator/definition.toml"),
    },
    BundledFile {
        relative_path: "agents/intel-analysis-orchestrator/system.md",
        contents: include_str!("../assets/agents/intel-analysis-orchestrator/system.md"),
    },
    BundledFile {
        relative_path: "agents/method-auditor/definition.toml",
        contents: include_str!("../assets/agents/method-auditor/definition.toml"),
    },
    BundledFile {
        relative_path: "agents/method-auditor/system.md",
        contents: include_str!("../assets/agents/method-auditor/system.md"),
    },
    BundledFile {
        relative_path: "agents/pattern-analyst/definition.toml",
        contents: include_str!("../assets/agents/pattern-analyst/definition.toml"),
    },
    BundledFile {
        relative_path: "agents/pattern-analyst/system.md",
        contents: include_str!("../assets/agents/pattern-analyst/system.md"),
    },
    BundledFile {
        relative_path: "agents/scenario-player/definition.toml",
        contents: include_str!("../assets/agents/scenario-player/definition.toml"),
    },
    BundledFile {
        relative_path: "agents/scenario-player/system.md",
        contents: include_str!("../assets/agents/scenario-player/system.md"),
    },
    BundledFile {
        relative_path: "agents/synthesis-writer/definition.toml",
        contents: include_str!("../assets/agents/synthesis-writer/definition.toml"),
    },
    BundledFile {
        relative_path: "agents/synthesis-writer/system.md",
        contents: include_str!("../assets/agents/synthesis-writer/system.md"),
    },
    BundledFile {
        relative_path: "agents/systems-modeller/definition.toml",
        contents: include_str!("../assets/agents/systems-modeller/definition.toml"),
    },
    BundledFile {
        relative_path: "agents/systems-modeller/system.md",
        contents: include_str!("../assets/agents/systems-modeller/system.md"),
    },
    BundledFile {
        relative_path: "agents/wargaming-orchestrator/definition.toml",
        contents: include_str!("../assets/agents/wargaming-orchestrator/definition.toml"),
    },
    BundledFile {
        relative_path: "agents/wargaming-orchestrator/system.md",
        contents: include_str!("../assets/agents/wargaming-orchestrator/system.md"),
    },
    BundledFile {
        relative_path: "skills/analysis-workflow/instructions.md",
        contents: include_str!("../assets/skills/analysis-workflow/instructions.md"),
    },
    BundledFile {
        relative_path: "skills/analysis-workflow/skill.toml",
        contents: include_str!("../assets/skills/analysis-workflow/skill.toml"),
    },
    BundledFile {
        relative_path: "skills/competing-hypotheses/instructions.md",
        contents: include_str!("../assets/skills/competing-hypotheses/instructions.md"),
    },
    BundledFile {
        relative_path: "skills/competing-hypotheses/skill.toml",
        contents: include_str!("../assets/skills/competing-hypotheses/skill.toml"),
    },
    BundledFile {
        relative_path: "skills/confidence-and-uncertainty/instructions.md",
        contents: include_str!("../assets/skills/confidence-and-uncertainty/instructions.md"),
    },
    BundledFile {
        relative_path: "skills/confidence-and-uncertainty/skill.toml",
        contents: include_str!("../assets/skills/confidence-and-uncertainty/skill.toml"),
    },
    BundledFile {
        relative_path: "skills/evidence-quality-review/instructions.md",
        contents: include_str!("../assets/skills/evidence-quality-review/instructions.md"),
    },
    BundledFile {
        relative_path: "skills/evidence-quality-review/skill.toml",
        contents: include_str!("../assets/skills/evidence-quality-review/skill.toml"),
    },
    BundledFile {
        relative_path: "skills/feedback-loops-and-thresholds/instructions.md",
        contents: include_str!("../assets/skills/feedback-loops-and-thresholds/instructions.md"),
    },
    BundledFile {
        relative_path: "skills/feedback-loops-and-thresholds/skill.toml",
        contents: include_str!("../assets/skills/feedback-loops-and-thresholds/skill.toml"),
    },
    BundledFile {
        relative_path: "skills/key-assumptions-check/instructions.md",
        contents: include_str!("../assets/skills/key-assumptions-check/instructions.md"),
    },
    BundledFile {
        relative_path: "skills/key-assumptions-check/skill.toml",
        contents: include_str!("../assets/skills/key-assumptions-check/skill.toml"),
    },
    BundledFile {
        relative_path: "skills/link-and-pattern-analysis/instructions.md",
        contents: include_str!("../assets/skills/link-and-pattern-analysis/instructions.md"),
    },
    BundledFile {
        relative_path: "skills/link-and-pattern-analysis/skill.toml",
        contents: include_str!("../assets/skills/link-and-pattern-analysis/skill.toml"),
    },
    BundledFile {
        relative_path: "skills/method-validation/instructions.md",
        contents: include_str!("../assets/skills/method-validation/instructions.md"),
    },
    BundledFile {
        relative_path: "skills/method-validation/skill.toml",
        contents: include_str!("../assets/skills/method-validation/skill.toml"),
    },
    BundledFile {
        relative_path: "skills/premortem-and-red-team/instructions.md",
        contents: include_str!("../assets/skills/premortem-and-red-team/instructions.md"),
    },
    BundledFile {
        relative_path: "skills/premortem-and-red-team/skill.toml",
        contents: include_str!("../assets/skills/premortem-and-red-team/skill.toml"),
    },
    BundledFile {
        relative_path: "skills/scenario-wargaming/instructions.md",
        contents: include_str!("../assets/skills/scenario-wargaming/instructions.md"),
    },
    BundledFile {
        relative_path: "skills/scenario-wargaming/skill.toml",
        contents: include_str!("../assets/skills/scenario-wargaming/skill.toml"),
    },
    // ── Web-Recherche mit Quellen (Plan R9, Matrix-Game mit Internet) ─────
    // `intel-web-researcher` (auf `researcher-web`) und sein Quellen-Skill;
    // `tests/analysis_family.rs` prüft beide.
    BundledFile {
        relative_path: "agents/intel-web-researcher/definition.toml",
        contents: include_str!("../assets/agents/intel-web-researcher/definition.toml"),
    },
    BundledFile {
        relative_path: "agents/intel-web-researcher/system.md",
        contents: include_str!("../assets/agents/intel-web-researcher/system.md"),
    },
    BundledFile {
        relative_path: "skills/osint-web-research/instructions.md",
        contents: include_str!("../assets/skills/osint-web-research/instructions.md"),
    },
    BundledFile {
        relative_path: "skills/osint-web-research/skill.toml",
        contents: include_str!("../assets/skills/osint-web-research/skill.toml"),
    },
    // ── Nutzungs-Skills der Kommando-Werkzeuge (PL-93, PL-97 B4) ─────────
    // Quelle sind ihre Werkzeug-Crates (`<crate>/skills/<name>/`); hier liegt
    // eine vendorte Kopie unter `assets/skills/`, weil `include_str!` das
    // Paket nicht verlassen darf (`cargo package` baut aus einem isolierten
    // Tarball, PR #154). `tests::TOOL_CRATE_SKILLS` führt die Zuordnung,
    // `test_vendored_tool_crate_skills_match_their_source` prüft
    // Vollständigkeit und Byte-Gleichheit mit der Quelle (Drift-Test).
    BundledFile {
        relative_path: "skills/cargo-tools/instructions.md",
        contents: include_str!("../assets/skills/cargo-tools/instructions.md"),
    },
    BundledFile {
        relative_path: "skills/cargo-tools/skill.toml",
        contents: include_str!("../assets/skills/cargo-tools/skill.toml"),
    },
    BundledFile {
        relative_path: "skills/cargo-tools/triggers.toml",
        contents: include_str!("../assets/skills/cargo-tools/triggers.toml"),
    },
    BundledFile {
        relative_path: "skills/fsread-tools/instructions.md",
        contents: include_str!("../assets/skills/fsread-tools/instructions.md"),
    },
    BundledFile {
        relative_path: "skills/fsread-tools/skill.toml",
        contents: include_str!("../assets/skills/fsread-tools/skill.toml"),
    },
    BundledFile {
        relative_path: "skills/fsread-tools/triggers.toml",
        contents: include_str!("../assets/skills/fsread-tools/triggers.toml"),
    },
    BundledFile {
        relative_path: "skills/git-read-tools/instructions.md",
        contents: include_str!("../assets/skills/git-read-tools/instructions.md"),
    },
    BundledFile {
        relative_path: "skills/git-read-tools/skill.toml",
        contents: include_str!("../assets/skills/git-read-tools/skill.toml"),
    },
    BundledFile {
        relative_path: "skills/git-read-tools/triggers.toml",
        contents: include_str!("../assets/skills/git-read-tools/triggers.toml"),
    },
    BundledFile {
        relative_path: "skills/sys-tools/instructions.md",
        contents: include_str!("../assets/skills/sys-tools/instructions.md"),
    },
    BundledFile {
        relative_path: "skills/sys-tools/skill.toml",
        contents: include_str!("../assets/skills/sys-tools/skill.toml"),
    },
    BundledFile {
        relative_path: "skills/sys-tools/triggers.toml",
        contents: include_str!("../assets/skills/sys-tools/triggers.toml"),
    },
];

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestResult, ctx};

    /// Die Nutzungs-Skills, deren Quelle in ihren Werkzeug-Crates liegt
    /// (PL-93, PL-97 B4): `(Skill-Verzeichnis, Werkzeug-Crate)`. Ihre
    /// vendorte Kopie liegt unter `assets/skills/<Verzeichnis>/`.
    const TOOL_CRATE_SKILLS: &[(&str, &str)] = &[
        ("cargo-tools", "harw-tool-cargo"),
        ("fsread-tools", "harw-tool-fsread"),
        ("git-read-tools", "harw-tool-gitread"),
        ("sys-tools", "harw-tool-sys"),
    ];

    /// Jede Datei unter `assets/` muss im Bundle stehen — sonst läge sie im
    /// Repo, käme aber nie beim Nutzer an.
    #[test]
    fn test_every_asset_file_is_embedded() -> TestResult {
        let assets = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("assets");
        let mut on_disk = Vec::new();
        collect_files(&assets, &assets, &mut on_disk)?;
        on_disk.sort();

        let mut embedded: Vec<String> = bundled_files()
            .iter()
            .map(|file| file.relative_path.to_owned())
            .collect();
        embedded.sort();

        assert_eq!(
            on_disk, embedded,
            "assets/ und BUNDLED_FILES laufen auseinander"
        );
        Ok(())
    }

    /// PR #154 [P1]: kein `include_str!` darf das Paket `harw-home`
    /// verlassen, sonst bricht `cargo package` (isolierter Tarball).
    #[test]
    fn test_bundle_includes_stay_inside_the_package() -> TestResult {
        let source = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/bundle.rs"),
        )
        .map_err(ctx("bundle.rs lesbar"))?;
        let escaping = ["include_str!(\"../..", "include_bytes!(\"../.."];
        for line in source.lines() {
            assert!(
                !escaping.iter().any(|needle| line.contains(needle)),
                "Einbettung verlässt das Paket: {}",
                line.trim()
            );
        }
        Ok(())
    }

    /// Drift-Test (PL-97 B4, PR #154): die vendorte Kopie unter
    /// `assets/skills/<dir>/` ist byte-gleich mit der Quelle unter
    /// `<Werkzeug-Crate>/skills/<dir>/` — gleiche Dateimenge, gleicher
    /// Inhalt. Außerhalb des Workspaces (Test aus dem gepackten Crate ohne
    /// Geschwister-Crates) gibt es keine Quelle zum Vergleichen.
    #[test]
    fn test_vendored_tool_crate_skills_match_their_source() -> TestResult {
        let manifest_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let workspace = manifest_dir
            .parent()
            .ok_or(crate::test_support::TestError::Missing("Workspace-Wurzel"))?;
        if !workspace.join("harw-home").join("Cargo.toml").is_file() {
            // Kein Workspace-Checkout (entpacktes Crate heißt `harw-home-<ver>`):
            // nichts zu vergleichen.
            return Ok(());
        }
        for (dir, krate) in TOOL_CRATE_SKILLS {
            let source = workspace.join(krate).join("skills").join(dir);
            // Fehlende oder umbenannte Quell-Crate muss laut scheitern, nicht still bestehen.
            assert!(
                source.is_dir(),
                "Quelle {krate}/skills/{dir} fehlt im Workspace — Werkzeug-Crate umbenannt \
                 oder verschoben? TOOL_CRATE_SKILLS und assets/skills/{dir} anpassen"
            );
            let vendored = manifest_dir.join("assets/skills").join(dir);
            let mut source_files = Vec::new();
            collect_files(&source, &source, &mut source_files)?;
            source_files.sort();
            let mut vendored_files = Vec::new();
            collect_files(&vendored, &vendored, &mut vendored_files)?;
            vendored_files.sort();
            assert_eq!(
                source_files, vendored_files,
                "Dateimenge von assets/skills/{dir} weicht von {krate}/skills/{dir} ab"
            );
            for name in ["skill.toml", "instructions.md", "triggers.toml"] {
                assert!(
                    vendored_files.iter().any(|file| file == name),
                    "{dir}/{name} fehlt"
                );
            }
            for relative in &source_files {
                let original =
                    std::fs::read(source.join(relative)).map_err(ctx("Werkzeug-Skill lesbar"))?;
                let copy =
                    std::fs::read(vendored.join(relative)).map_err(ctx("vendorte Kopie lesbar"))?;
                assert!(
                    original == copy,
                    "assets/skills/{dir}/{relative} weicht von {krate}/skills/{dir}/{relative} ab \
                     — Kopie aus der Werkzeug-Crate erneuern"
                );
                let embedded = bundled_files()
                    .iter()
                    .find(|file| file.relative_path == format!("skills/{dir}/{relative}"))
                    .ok_or(crate::test_support::TestError::Missing("Bundle-Eintrag"))?;
                assert_eq!(
                    embedded.contents.as_bytes(),
                    original.as_slice(),
                    "skills/{dir}/{relative} im Bundle"
                );
            }
        }
        Ok(())
    }

    fn collect_files(
        root: &std::path::Path,
        dir: &std::path::Path,
        out: &mut Vec<String>,
    ) -> TestResult {
        let entries = std::fs::read_dir(dir).map_err(ctx("assets-Verzeichnis lesbar"))?;
        for entry in entries {
            let path = entry.map_err(ctx("Verzeichniseintrag lesbar"))?.path();
            if path.is_dir() {
                collect_files(root, &path, out)?;
            } else {
                let relative = path
                    .strip_prefix(root)
                    .map_err(ctx("Pfad liegt unter assets"))?;
                out.push(relative.to_string_lossy().replace('\\', "/"));
            }
        }
        Ok(())
    }

    #[test]
    fn test_no_bundled_file_is_empty() {
        for file in bundled_files() {
            assert!(
                !file.contents.trim().is_empty(),
                "{} ist leer",
                file.relative_path
            );
        }
    }

    #[test]
    fn test_bundled_agent_toml_parses_and_declares_no_uia_role() -> TestResult {
        let mut agents = 0;
        for file in bundled_files() {
            if !file.relative_path.ends_with("/agent.toml") {
                continue;
            }
            agents += 1;
            let agent: harw_config::AgentToml = toml::from_str(file.contents).map_err(|error| {
                crate::test_support::TestError::Unexpected(format!(
                    "{}: {error}",
                    file.relative_path
                ))
            })?;
            assert_ne!(
                agent.role, "user-interface",
                "{} darf keine UIA sein — die entsteht pro Nutzer über `harw uia new`",
                file.relative_path
            );
        }
        // Plan R9, Teil B: 21 der 22 Legacy-Agenten sind nach
        // `definition.toml` migriert (siehe
        // `test_migrated_agents_ship_a_definition_and_no_agent_toml`); nur
        // `coding-orchestrator` bleibt `agent.toml` — er bindet Skills an die
        // gleichnamige eingebaute Rolle, eine Definition gleichen Namens würde
        // als Kollision mit der eingebauten Rolle verworfen.
        assert_eq!(
            agents, 1,
            "das Bundle liefert nur noch ein Legacy-agent.toml"
        );
        Ok(())
    }

    /// Plan R9, Teil B: jeder Agent-Ordner des Bundles trägt entweder eine
    /// `definition.toml` oder (nur `coding-orchestrator`) ein Legacy-
    /// `agent.toml`, nie beides; jede Definition verweist per
    /// `instructions_file` auf ihr mitgeliefertes `system.md`.
    #[test]
    fn test_migrated_agents_ship_a_definition_and_no_agent_toml() -> TestResult {
        let mut dirs: Vec<&str> = bundled_files()
            .iter()
            .filter_map(|file| file.relative_path.strip_prefix("agents/"))
            .filter_map(|rest| rest.split('/').next())
            .collect();
        dirs.sort_unstable();
        dirs.dedup();
        let has = |dir: &str, file: &str| {
            let path = format!("agents/{dir}/{file}");
            bundled_files()
                .iter()
                .find(|entry| entry.relative_path == path)
                .map(|entry| entry.contents)
        };
        for dir in dirs {
            let definition = has(dir, "definition.toml");
            let legacy = has(dir, "agent.toml");
            if dir == "coding-orchestrator" {
                assert!(legacy.is_some() && definition.is_none(), "{dir}");
                continue;
            }
            let definition = definition.ok_or(crate::test_support::TestError::Missing(
                "definition.toml eines mitgelieferten Agenten",
            ))?;
            assert!(legacy.is_none(), "{dir}: kein Legacy-agent.toml mehr");
            let table: toml::Table =
                toml::from_str(definition).map_err(ctx("definition.toml parst"))?;
            assert_eq!(
                table.get("specialization").and_then(toml::Value::as_str),
                Some(dir),
                "{dir}: specialization = Ordnername"
            );
            let instructions = table
                .get("instructions_file")
                .and_then(toml::Value::as_str)
                .ok_or(crate::test_support::TestError::Missing("instructions_file"))?;
            assert!(
                has(dir, instructions).is_some(),
                "{dir}: {instructions} fehlt"
            );
        }
        Ok(())
    }

    #[test]
    fn test_bundled_skill_toml_parses() -> TestResult {
        let mut skills = 0;
        for file in bundled_files() {
            if !file.relative_path.ends_with("/skill.toml") {
                continue;
            }
            skills += 1;
            toml::from_str::<harw_config::SkillToml>(file.contents).map_err(|error| {
                crate::test_support::TestError::Unexpected(format!(
                    "{}: {error}",
                    file.relative_path
                ))
            })?;
        }
        // Runde 7, Teil T6/T7: +`latex-report`.
        // Plan R9, Teil D: +10 Methoden-Skills der Analyse-Familie.
        // Plan R9, Web-Recherche: +`osint-web-research`.
        // PL-97 B4: +4 Nutzungs-Skills der Kommando-Werkzeuge (PL-93).
        // PL-97 D4: +`harw-dod`.
        assert_eq!(skills, 80, "das Bundle liefert 80 Skills");
        Ok(())
    }

    /// Ein Agent, der auf einen nicht mitgelieferten Skill verweist, wäre beim
    /// Nutzer sofort kaputt.
    #[test]
    fn test_every_referenced_skill_ships_with_the_bundle() -> TestResult {
        let available: Vec<String> = bundled_files()
            .iter()
            .filter_map(|file| {
                file.relative_path
                    .strip_prefix("skills/")
                    .and_then(|rest| rest.strip_suffix("/skill.toml"))
                    .map(str::to_owned)
            })
            .collect();

        for file in bundled_files() {
            if !file.relative_path.ends_with("/agent.toml") {
                continue;
            }
            let agent: harw_config::AgentToml =
                toml::from_str(file.contents).map_err(ctx("agent.toml parst"))?;
            for skill in &agent.skills {
                assert!(
                    available.contains(skill),
                    "{} verweist auf den fehlenden Skill '{skill}'",
                    file.relative_path
                );
            }
        }
        Ok(())
    }

    /// Runde 7, Teil T7: die Gerüste der Berichtsvorlage tragen genau die
    /// Platzhalter, auf denen `latex.template` aufbaut; `harw-report.sty`
    /// trägt keinen (sie ist ohne Ersetzung gültiges LaTeX) und alle
    /// Vorlagendateien sind neutral (keine Firmen-, Produkt- oder
    /// Personennamen aus dem Vorbild).
    #[test]
    fn test_latex_report_templates_carry_the_documented_placeholders() -> TestResult {
        const PLACEHOLDERS: &[&str] = &[
            "%%TITLE%%",
            "%%SUBTITLE%%",
            "%%AUTHOR%%",
            "%%DATE%%",
            "%%ACCENT%%",
            "%%WARN%%",
            "%%MAINFONT%%",
            "%%SANSFONT%%",
            "%%MONOFONT%%",
            "%%LANGUAGE%%",
        ];
        let template = |name: &str| {
            bundled_files()
                .iter()
                .find(|file| file.relative_path == format!("skills/latex-report/templates/{name}"))
                .map(|file| file.contents)
                .ok_or(crate::test_support::TestError::Missing(
                    "latex-report template",
                ))
        };
        for kind in ["bericht.tex", "business-paper.tex", "handbuch.tex"] {
            let text = template(kind)?;
            for placeholder in PLACEHOLDERS {
                assert!(text.contains(placeholder), "{kind}: fehlt {placeholder}");
            }
            for building_block in [
                "\\documentclass[11pt,a4paper]{scrartcl}",
                "\\usepackage{harw-report}",
                "\\begin{achtung}[Wichtig vorab]",
                "\\begin{merke}[Die Idee in drei Sätzen]",
                "\\begin{tikzpicture}",
                "\\begin{longtable}",
                "\\endhead",
                "\\begin{thebibliography}",
            ] {
                assert!(
                    text.contains(building_block),
                    "{kind}: fehlt {building_block}"
                );
            }
        }
        let sty = template("harw-report.sty")?;
        assert!(
            !sty.contains("%%"),
            "harw-report.sty darf keine Platzhalter tragen"
        );
        for needed in [
            "\\babelprovide[import,main]",
            "\\newcolumntype{L}",
            "\\newtcolorbox{merke}",
            "\\newtcolorbox{achtung}",
            "\\newtcolorbox{beispiel}",
            "harwbox/.style",
            "harwpfeil/.style",
            "harwlabel/.style",
            "\\emergencystretch=3em",
            "tocnumwidth",
            "{harwaccent}{HTML}",
            "714B67",
            "C0392B",
            "DejaVu Serif",
        ] {
            assert!(sty.contains(needed), "harw-report.sty: fehlt {needed}");
        }
        Ok(())
    }

    #[test]
    fn test_target_in_joins_segments_platform_correctly() {
        let file = BundledFile {
            relative_path: "agents/debugger/agent.toml",
            contents: "",
        };
        let target = file.target_in(std::path::Path::new("/tmp/harw"));
        assert_eq!(
            target,
            std::path::Path::new("/tmp/harw")
                .join("agents")
                .join("debugger")
                .join("agent.toml")
        );
    }
}
