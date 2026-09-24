//! Idempotentes Anlegen des `~/.harw`-Root-Space.
//!
//! [`ensure_home`] legt Verzeichnisse und Default-Dateien an, **überschreibt
//! aber niemals** von der Nutzerin geänderte Dateien (wie das im README
//! dokumentierte `harw init`-Verhalten). Secrets landen ausschließlich in
//! `auth.toml` (chmod 600 unter Unix); `config.toml` trägt nur
//! Verhalten/Flags.
//!
//! Ausnahme seit Runde 7, Teil T6: die mitgelieferten Agenten- und
//! Skill-Dateien (Bundle) werden über `<home>/.bundle-manifest.toml`
//! aktualisiert, solange die Nutzerin sie nicht geändert hat; eine geänderte
//! Datei bleibt, die neue Fassung landet als `<datei>.harw-neu` daneben
//! (Regeln in [`crate::bundle_manifest`]).

use std::path::{Path, PathBuf};

use crate::error::{HomeError, HomeResult};
use crate::paths;

/// Bericht darüber, was [`ensure_home`] neu angelegt hat. Ermöglicht Aufrufern,
/// einen First-Run von einem No-op-Re-Run zu unterscheiden.
#[derive(Debug, Default, Clone)]
pub struct Scaffolded {
    /// `true`, wenn der Root-Space vor diesem Aufruf nicht existierte.
    pub created_home: bool,
    /// Der aufgelöste Root-Space-Pfad.
    pub home: PathBuf,
    /// Verzeichnis des aktiven Profils.
    pub profile_dir: PathBuf,
    /// Neu geschriebene Dateien (leer bei einem reinen Re-Run).
    pub written_files: Vec<PathBuf>,
    /// Runde 7, Teil T6: mitgelieferte Dateien, die unverändert waren und auf
    /// die neue Bundle-Fassung gehoben wurden.
    pub updated_files: Vec<PathBuf>,
    /// Runde 7, Teil T6: neu abgelegte `<datei>.harw-neu` neben
    /// mitgelieferten Dateien, die die Nutzerin geändert hat. Der Aufrufer
    /// sollte darauf hinweisen: die neue Fassung liegt daneben und kann von
    /// Hand übernommen werden.
    pub bundle_conflicts: Vec<PathBuf>,
}

/// Legt Root-Space und das effektive Profil an (idempotent).
///
/// # Arguments
/// - `home` (`&Path`): Zielpfad des Root-Space, üblicherweise aus
///   [`crate::paths::home_dir`].
///
/// # Returns
/// Ein [`Scaffolded`]-Bericht der neu erzeugten Struktur.
///
/// # Errors
/// [`HomeError::Io`] bei jedem fehlgeschlagenen Verzeichnis-/Datei-Zugriff.
pub fn ensure_home(home: &Path) -> HomeResult<Scaffolded> {
    let profile = paths::active_profile_name(home);
    ensure_home_with_profile(home, &profile)
}

/// Scaffolds `home` using an already-resolved profile name.
///
/// Keeping profile selection outside the filesystem-writing path makes the
/// first-run behavior deterministic for callers that already own the
/// selection, and avoids requiring process-global environment mutation in
/// tests.
fn ensure_home_with_profile(home: &Path, profile: &str) -> HomeResult<Scaffolded> {
    let created_home = !home.exists();
    let profile_dir = paths::profile_dir(home, profile)?;

    let mut report = Scaffolded {
        created_home,
        home: home.to_path_buf(),
        profile_dir: profile_dir.clone(),
        written_files: Vec::new(),
        updated_files: Vec::new(),
        bundle_conflicts: Vec::new(),
    };

    // Root-Verzeichnisse.
    create_dir(home)?;
    harden_dir(home)?;
    create_dir(&paths::cache_dir(home))?;
    // Kein `<home>/jobs`: der `JobStore` lebt profilbezogen unter
    // `profiles/<name>/jobs` und legt sein Verzeichnis selbst an.

    // Profil-Verzeichnisse.
    create_dir(&profile_dir)?;
    harden_dir(&profile_dir)?;
    for sub in ["providers", "models", "channels", "sessions", "memories"] {
        create_dir(&profile_dir.join(sub))?;
    }
    create_dir(&paths::knowledge_dir(&profile_dir))?;

    // Root-Dateien (nur schreiben, wenn absent).
    write_if_absent(
        &paths::active_profile_path(home),
        &format!("{profile}\n"),
        &mut report,
    )?;
    write_if_absent(
        &home.join("config.toml"),
        GLOBAL_CONFIG_TEMPLATE,
        &mut report,
    )?;
    write_if_absent(
        &paths::cache_dir(home).join(".gitignore"),
        "*\n",
        &mut report,
    )?;
    write_installation_id(home, &mut report)?;
    write_auth_file(home, &mut report)?;

    // Profil-Dateien.
    write_if_absent(
        &profile_dir.join("config.toml"),
        PROFILE_CONFIG_TEMPLATE,
        &mut report,
    )?;
    write_if_absent(
        &profile_dir.join("memories").join("MEMORY.md"),
        MEMORY_TEMPLATE,
        &mut report,
    )?;
    write_if_absent(
        &profile_dir.join("agents").join("worker").join("agent.toml"),
        DEFAULT_WORKER_AGENT_TEMPLATE,
        &mut report,
    )?;

    write_bundle(home, &mut report)?;

    Ok(report)
}

/// Schreibt bzw. aktualisiert die mit dem Binary ausgelieferte
/// Agenten-/Skill-Startausstattung unter `~/.harw/agents` bzw.
/// `~/.harw/skills`.
///
/// # Description
/// Die Dateien landen auf der Home-Ebene, nicht im Profil: sie sind
/// profilunabhängig und liegen damit im schwächsten Config-Layer
/// ([`crate::paths::config_layers`]), sodass Profil und Projekt sie überstimmen
/// können. Runde 7, Teil T6: fehlende Dateien werden geschrieben, unveränderte
/// auf die neue Bundle-Fassung gehoben, von der Nutzerin geänderte nie
/// überschrieben — deren neue Fassung liegt als `<datei>.harw-neu` daneben
/// (Manifest `<home>/.bundle-manifest.toml`, siehe
/// [`crate::bundle_manifest::sync_bundle`]).
///
/// # Errors
/// [`HomeError::Io`] bei jedem fehlgeschlagenen Datei-Zugriff.
fn write_bundle(home: &Path, report: &mut Scaffolded) -> HomeResult<()> {
    let sync = crate::bundle_manifest::sync_bundle(home, crate::bundle::bundled_files())?;
    report.written_files.extend(sync.written);
    report.updated_files.extend(sync.updated);
    report.bundle_conflicts.extend(sync.conflicts);
    Ok(())
}

/// Schreibt `installation_id` mit einer frischen UUID v7, falls sie fehlt.
fn write_installation_id(home: &Path, report: &mut Scaffolded) -> HomeResult<()> {
    let path = paths::installation_id_path(home);
    if path.exists() {
        return Ok(());
    }
    let id = uuid::Uuid::now_v7();
    write_if_absent(&path, &format!("{id}\n"), report)
}

/// Schreibt eine leere `auth.toml` mit restriktiven Rechten (chmod 600 unter
/// Unix), falls sie fehlt. Diese Datei ist der einzige Ort für Credential-Refs.
fn write_auth_file(home: &Path, report: &mut Scaffolded) -> HomeResult<()> {
    let path = paths::auth_path(home);
    if path.exists() {
        return Ok(());
    }
    write_if_absent(&path, AUTH_TEMPLATE, report)?;
    harden_permissions(&path)?;
    Ok(())
}

/// Setzt `0o600` auf `path` (nur Unix; auf anderen Plattformen ein No-op).
#[cfg(unix)]
fn harden_permissions(path: &Path) -> HomeResult<()> {
    use std::os::unix::fs::PermissionsExt;
    let permissions = std::fs::Permissions::from_mode(0o600);
    std::fs::set_permissions(path, permissions).map_err(|error| HomeError::io(path, error))
}

#[cfg(not(unix))]
fn harden_permissions(_path: &Path) -> HomeResult<()> {
    Ok(())
}

/// Setzt `0o700` auf ein Verzeichnis (nur Unix; sonst No-op).
#[cfg(unix)]
fn harden_dir(path: &Path) -> HomeResult<()> {
    use std::os::unix::fs::PermissionsExt;
    let permissions = std::fs::Permissions::from_mode(0o700);
    std::fs::set_permissions(path, permissions).map_err(|error| HomeError::io(path, error))
}

#[cfg(not(unix))]
fn harden_dir(_path: &Path) -> HomeResult<()> {
    Ok(())
}

/// Legt ein Verzeichnis (rekursiv) an; No-op, wenn es bereits existiert.
fn create_dir(path: &Path) -> HomeResult<()> {
    std::fs::create_dir_all(path).map_err(|error| HomeError::io(path, error))
}

/// Schreibt `contents` nach `path`, sofern die Datei noch nicht existiert.
fn write_if_absent(path: &Path, contents: &str, report: &mut Scaffolded) -> HomeResult<()> {
    if path.exists() {
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        create_dir(parent)?;
    }
    std::fs::write(path, contents).map_err(|error| HomeError::io(path, error))?;
    report.written_files.push(path.to_path_buf());
    Ok(())
}

/// Globale `config.toml`: nur Verhalten/Flags, nie Secrets. Der aktive
/// Provider/Model wird im Profil-Overlay gesetzt.
const GLOBAL_CONFIG_TEMPLATE: &str = "\
# Harwness — globale Konfiguration (~/.harw/config.toml)
# Nur Verhalten und Feature-Flags. Niemals Secrets — die liegen in auth.toml.

[logging]
level = \"info\"

[onboarding]
# First-Run-Fortschritt (Hermes-Muster). Wird vom Onboarding-Wizard gesetzt.
[onboarding.seen]
provider = false
model = false
channel = false
";

/// Profil-`config.toml`: trägt den aktiven Provider/Model und lokale Overrides.
const PROFILE_CONFIG_TEMPLATE: &str = "\
# Harwness — Profil-Overlay (~/.harw/profiles/<name>/config.toml)
# default_provider/default_model werden vom Onboarding-Wizard gefüllt.

# default_provider = \"openai\"
# default_model = \"gpt-5.4\"

[mcp_listener]
enabled = false
listen_addr = \"127.0.0.1:1337\"
path = \"/mcp\"
";

/// Leere `auth.toml` mit erklärendem Kopf. Credential-Refs (`env:`/`file:`)
/// trägt der Onboarding-Wizard ein.
const AUTH_TEMPLATE: &str = "\
# Harwness — Credential-Referenzen (chmod 600, niemals versionieren).
# Werte sind Secret-Refs wie \"env:OPENAI_API_KEY\" oder \"file:/pfad/zum/key\",
# niemals literale Schlüssel.
";

/// Startvorlage für das dateibasierte Profil-Gedächtnis.
const MEMORY_TEMPLATE: &str = "# Memory\n";

/// Default-Agentendefinition, damit Child-Spawning (`/agent`) direkt nach dem
/// Setup ohne manuelle Konfiguration verfügbar ist. `role` fehlt bewusst und
/// fällt in `AgentToml` auf `"worker"` zurück (vom TUI-Spawner erlaubte Rolle).
const DEFAULT_WORKER_AGENT_TEMPLATE: &str = "\
# Harwness — Default-Worker-Agent (~/.harw/profiles/<name>/agents/worker/agent.toml)
# Wird beim ersten Setup angelegt, damit Child-Spawning sofort funktioniert.
# Passe diese Datei an, um die Worker-Rolle zu spezialisieren, oder lege
# weitere agents/<name>/agent.toml-Verzeichnisse fuer zusaetzliche Rollen an.

name = \"worker\"
description = \"Default child-agent role, scaffolded automatically for immediate use.\"
";

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestResult, ctx};

    #[test]
    fn profile_config_template_guides_current_default_model() {
        assert!(PROFILE_CONFIG_TEMPLATE.contains("# default_model = \"gpt-5.4\""));
        assert!(!PROFILE_CONFIG_TEMPLATE.contains("# default_model = \"gpt-5\""));
    }

    #[test]
    fn first_scaffold_persists_selected_non_default_profile() -> TestResult {
        let home =
            std::env::temp_dir().join(format!("harw-home-scaffold-{}", uuid::Uuid::now_v7()));
        let profile = "foo";

        let report = ensure_home_with_profile(&home, profile).map_err(ctx("scaffold home"))?;

        assert!(report.created_home);
        assert_eq!(report.profile_dir, home.join("profiles").join(profile));
        assert_eq!(
            std::fs::read_to_string(paths::active_profile_path(&home))
                .map_err(ctx("active profile"))?,
            "foo\n"
        );
        assert!(report.profile_dir.join("config.toml").is_file());
        assert!(!home.join("profiles").join(paths::DEFAULT_PROFILE).exists());

        let rerun = ensure_home_with_profile(&home, profile).map_err(ctx("re-scaffold home"))?;

        assert!(!rerun.created_home);
        assert!(rerun.written_files.is_empty());
        assert_eq!(
            std::fs::read_to_string(paths::active_profile_path(&home))
                .map_err(ctx("active profile"))?,
            "foo\n"
        );

        std::fs::remove_dir_all(&home).map_err(ctx("remove temporary scaffold"))?;
        Ok(())
    }

    #[test]
    fn first_scaffold_creates_profile_knowledge_dir_but_no_root_jobs_dir() -> TestResult {
        let home =
            std::env::temp_dir().join(format!("harw-home-scaffold-{}", uuid::Uuid::now_v7()));

        let report = ensure_home_with_profile(&home, "analysis").map_err(ctx("scaffold home"))?;

        let knowledge = paths::knowledge_dir(&report.profile_dir);
        assert_eq!(knowledge, home.join("profiles/analysis/knowledge"));
        assert!(knowledge.is_dir());
        assert!(report.profile_dir.join("memories").is_dir());
        assert!(report.profile_dir.join("sessions").is_dir());
        assert!(!home.join("jobs").exists());

        let rerun = ensure_home_with_profile(&home, "analysis").map_err(ctx("re-scaffold home"))?;
        assert!(rerun.written_files.is_empty());
        assert!(knowledge.is_dir());

        std::fs::remove_dir_all(&home).map_err(ctx("remove temporary scaffold"))?;
        Ok(())
    }

    #[test]
    fn scaffold_preserves_existing_active_profile_pointer() -> TestResult {
        let home =
            std::env::temp_dir().join(format!("harw-home-scaffold-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(&home).map_err(ctx("create temporary home"))?;
        std::fs::write(paths::active_profile_path(&home), "existing\n")
            .map_err(ctx("write active profile"))?;

        let report = ensure_home(&home).map_err(ctx("scaffold home"))?;

        assert_eq!(report.profile_dir, home.join("profiles").join("existing"));
        assert_eq!(
            std::fs::read_to_string(paths::active_profile_path(&home))
                .map_err(ctx("active profile"))?,
            "existing\n"
        );

        std::fs::remove_dir_all(&home).map_err(ctx("remove temporary scaffold"))?;
        Ok(())
    }

    #[test]
    fn first_scaffold_writes_default_worker_agent_toml() -> TestResult {
        let home =
            std::env::temp_dir().join(format!("harw-home-scaffold-{}", uuid::Uuid::now_v7()));
        let profile = "foo";

        let report = ensure_home_with_profile(&home, profile).map_err(ctx("scaffold home"))?;

        let agent_path = report
            .profile_dir
            .join("agents")
            .join("worker")
            .join("agent.toml");
        assert!(agent_path.is_file());
        assert!(
            report.written_files.contains(&agent_path),
            "the default worker agent.toml must be reported as newly written on first run"
        );

        let contents =
            std::fs::read_to_string(&agent_path).map_err(ctx("read default agent.toml"))?;
        let agent: harw_config::AgentToml = toml::from_str(&contents)
            .map_err(ctx("default worker agent.toml must be valid AgentToml"))?;
        assert_eq!(agent.name, "worker");
        assert_eq!(
            agent.role, "worker",
            "an omitted role must default to the worker role"
        );

        std::fs::remove_dir_all(&home).map_err(ctx("remove temporary scaffold"))?;
        Ok(())
    }

    #[test]
    fn rerun_does_not_overwrite_default_worker_agent_toml() -> TestResult {
        let home =
            std::env::temp_dir().join(format!("harw-home-scaffold-{}", uuid::Uuid::now_v7()));
        let profile = "foo";

        let first = ensure_home_with_profile(&home, profile).map_err(ctx("scaffold home"))?;
        let agent_path = first
            .profile_dir
            .join("agents")
            .join("worker")
            .join("agent.toml");
        let custom_contents = "name = \"worker\"\ndescription = \"customized\"\n";
        std::fs::write(&agent_path, custom_contents).map_err(ctx("simulate user customization"))?;

        let rerun = ensure_home_with_profile(&home, profile).map_err(ctx("re-scaffold home"))?;

        assert!(
            !rerun.written_files.contains(&agent_path),
            "a re-run must not report the already-present agent.toml as newly written"
        );
        assert_eq!(
            std::fs::read_to_string(&agent_path).map_err(ctx("read agent.toml after re-run"))?,
            custom_contents,
            "a re-run must never overwrite an existing agent.toml"
        );

        std::fs::remove_dir_all(&home).map_err(ctx("remove temporary scaffold"))?;
        Ok(())
    }

    #[test]
    fn first_scaffold_writes_the_bundled_agents_and_skills() -> TestResult {
        let home =
            std::env::temp_dir().join(format!("harw-home-scaffold-{}", uuid::Uuid::now_v7()));

        let report = ensure_home_with_profile(&home, "foo").map_err(ctx("scaffold home"))?;

        for file in crate::bundle::bundled_files() {
            let target = file.target_in(&home);
            assert!(target.is_file(), "{} fehlt", file.relative_path);
            assert!(
                report.written_files.contains(&target),
                "{} muss als neu geschrieben gemeldet werden",
                file.relative_path
            );
        }

        std::fs::remove_dir_all(&home).map_err(ctx("remove temporary scaffold"))?;
        Ok(())
    }

    #[test]
    fn rerun_does_not_overwrite_a_customized_bundled_agent() -> TestResult {
        let home =
            std::env::temp_dir().join(format!("harw-home-scaffold-{}", uuid::Uuid::now_v7()));

        ensure_home_with_profile(&home, "foo").map_err(ctx("scaffold home"))?;
        let customized = home.join("agents").join("debugger").join("system.md");
        let contents = "# meine eigene Fassung\n";
        std::fs::write(&customized, contents).map_err(ctx("simulate user customization"))?;

        let rerun = ensure_home_with_profile(&home, "foo").map_err(ctx("re-scaffold home"))?;

        assert!(!rerun.written_files.contains(&customized));
        assert_eq!(
            std::fs::read_to_string(&customized).map_err(ctx("read after re-run"))?,
            contents
        );

        std::fs::remove_dir_all(&home).map_err(ctx("remove temporary scaffold"))?;
        Ok(())
    }

    /// Runde 7, Teil T6: eine unveränderte mitgelieferte Datei mit altem
    /// Inhalt (simuliert über ein Manifest, das genau diesen alten Inhalt als
    /// installiert führt) wird beim nächsten Lauf aktualisiert; eine von der
    /// Nutzerin geänderte bleibt und bekommt eine `.harw-neu` daneben.
    #[test]
    fn rerun_updates_unchanged_bundle_files_and_protects_customized_ones() -> TestResult {
        let home =
            std::env::temp_dir().join(format!("harw-home-scaffold-{}", uuid::Uuid::now_v7()));
        ensure_home_with_profile(&home, "foo").map_err(ctx("scaffold home"))?;

        let stale = home.join("agents").join("debugger").join("agent.toml");
        let customized = home.join("agents").join("debugger").join("system.md");
        let old_contents = "# alte mitgelieferte Fassung\n";
        std::fs::write(&stale, old_contents).map_err(ctx("simulate old bundle file"))?;
        std::fs::write(&customized, "# meine eigene Fassung\n")
            .map_err(ctx("simulate user customization"))?;
        // Das Manifest führt für beide Dateien eine ältere Fassung als
        // installiert: genau die Lage nach einem Update des Binarys. Für
        // `agent.toml` ist das auch der aktuelle Inhalt (unverändert), für
        // `system.md` nicht (von der Nutzerin geändert).
        let manifest_path = home.join(crate::bundle_manifest::BUNDLE_MANIFEST_FILE);
        let mut manifest =
            std::fs::read_to_string(&manifest_path).map_err(ctx("read bundle manifest"))?;
        let bundled = |path: &str| {
            crate::bundle::bundled_files()
                .iter()
                .find(|file| file.relative_path == path)
                .map(|file| file.contents)
                .ok_or(crate::test_support::TestError::Missing(
                    "bundled debugger file",
                ))
        };
        let agent_toml = bundled("agents/debugger/agent.toml")?;
        let system_md = bundled("agents/debugger/system.md")?;
        manifest = manifest.replace(
            &sha256_hex_for_test(agent_toml),
            &sha256_hex_for_test(old_contents),
        );
        manifest = manifest.replace(
            &sha256_hex_for_test(system_md),
            &sha256_hex_for_test("# alte mitgelieferte system.md\n"),
        );
        std::fs::write(&manifest_path, manifest).map_err(ctx("rewrite bundle manifest"))?;

        let rerun = ensure_home_with_profile(&home, "foo").map_err(ctx("re-scaffold home"))?;

        assert_eq!(rerun.updated_files, vec![stale.clone()]);
        assert_eq!(
            std::fs::read_to_string(&stale).map_err(ctx("read updated"))?,
            agent_toml
        );
        let sidecar = home
            .join("agents")
            .join("debugger")
            .join("system.md.harw-neu");
        assert_eq!(rerun.bundle_conflicts, vec![sidecar.clone()]);
        assert_eq!(
            std::fs::read_to_string(&customized).map_err(ctx("read customized"))?,
            "# meine eigene Fassung\n"
        );
        assert!(sidecar.is_file());
        assert!(rerun.written_files.is_empty());

        std::fs::remove_dir_all(&home).map_err(ctx("remove temporary scaffold"))?;
        Ok(())
    }

    /// SHA-256 als Hex, wie ihn das Bundle-Manifest führt.
    fn sha256_hex_for_test(contents: &str) -> String {
        use sha2::Digest;
        format!("{:x}", sha2::Sha256::digest(contents.as_bytes()))
    }

    /// Das Bundle liegt auf der Home-Ebene und muss über die reguläre
    /// Layer-Kette gefunden werden — sonst hilft es dem Nutzer nicht.
    #[test]
    fn bundled_agents_are_discoverable_over_the_home_layer() -> TestResult {
        let home =
            std::env::temp_dir().join(format!("harw-home-scaffold-{}", uuid::Uuid::now_v7()));

        let report = ensure_home_with_profile(&home, "foo").map_err(ctx("scaffold home"))?;

        let resolved = harw_config::discover_config(&[home.clone(), report.profile_dir.clone()])
            .map_err(ctx("discovery over home and profile layer must succeed"))?;

        assert!(resolved.agents.contains_key("coding-orchestrator"));
        assert!(resolved.agents.contains_key("rust-implementer"));
        assert_eq!(
            resolved.agents.len(),
            23,
            "22 Bundle-Agenten plus der Profil-Default-Worker"
        );

        std::fs::remove_dir_all(&home).map_err(ctx("remove temporary scaffold"))?;
        Ok(())
    }

    #[test]
    fn scaffolded_worker_agent_is_discoverable_via_harw_config() -> TestResult {
        let home =
            std::env::temp_dir().join(format!("harw-home-scaffold-{}", uuid::Uuid::now_v7()));
        let profile = "foo";

        let report = ensure_home_with_profile(&home, profile).map_err(ctx("scaffold home"))?;

        let resolved = harw_config::discover_config(std::slice::from_ref(&report.profile_dir))
            .map_err(ctx(
                "discovery over the scaffolded profile dir must succeed",
            ))?;

        assert_eq!(
            resolved.agents.len(),
            1,
            "the scaffolded default worker agent must be the only discovered agent"
        );
        assert!(resolved.agents.contains_key("worker"));

        std::fs::remove_dir_all(&home).map_err(ctx("remove temporary scaffold"))?;
        Ok(())
    }
}
