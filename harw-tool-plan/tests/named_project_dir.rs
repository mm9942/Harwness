//! #22: der Plan-Anzeigepfad (`.harw/plans/…` bzw. `.<name>/plans/…` einer
//! personalisierten harw) folgt [`harw_home::project_dir_name`].
//!
//! Eigene Integrationstest-Datei (eigener Prozess), weil
//! [`harw_home::set_named_home`] einen prozessweiten `OnceLock` genau einmal
//! setzt: in einem gemeinsamen Testbinary würde das jeden anderen Test, der
//! den Standardnamen `.harw` erwartet (etwa die `#[cfg(test)]`-Module in
//! `harw-tool-plan/src/*.rs`), nichtdeterministisch mit einer
//! personalisierten harw zurücklassen.

use harw_tool_plan::plan_file::{self, PlanDir};

#[test]
fn plan_display_prefix_and_resolve_follow_the_active_project_dir_name() {
    // Vor `set_named_home`: der Standardname `.harw`.
    assert_eq!(plan_file::plan_display_prefix(), ".harw/plans");
    assert_eq!(plan_file::display_path("auth"), ".harw/plans/auth.md");

    let temp = tempfile::tempdir().expect("tempdir");
    let default_dir = PlanDir::new(temp.path().join(".harw").join("plans"));
    assert_eq!(
        default_dir.resolve(".harw/plans/auth.md").ok().as_deref(),
        Some("auth")
    );

    harw_home::set_named_home("mia").expect("set_named_home");
    assert_eq!(harw_home::project_dir_name(), ".mia");

    // Danach: derselbe Anzeigepfad folgt dem personalisierten Namen.
    assert_eq!(plan_file::plan_display_prefix(), ".mia/plans");
    assert_eq!(plan_file::display_path("auth"), ".mia/plans/auth.md");

    let named_dir = PlanDir::new(temp.path().join(".mia").join("plans"));
    assert_eq!(
        named_dir.resolve(".mia/plans/auth.md").ok().as_deref(),
        Some("auth")
    );
    // The old display prefix no longer resolves once the name has changed.
    assert!(named_dir.resolve(".harw/plans/auth.md").is_err());
}
