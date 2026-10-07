//! `harw device list|set-tier|revoke`: die enrollten Geräte des Own-Cloud-
//! Listeners verwalten (PL-94, Zyklus N1).
//!
//! # Beschreibung
//! Dünne CLI-Schicht über [`harw_node_listener::DeviceRegistry`]
//! (`node-devices.conf` unter `<profil>/session-host/remote/`, dasselbe
//! Verzeichnis wie `harw gateway`). Geschrieben wird nie per Hand-Parsing,
//! sondern über `set_tier`/`mark_revoked` der Registry (atomar, Modus 0600).
//! Der Listener liest die Datei bei jedem Handshake neu; die Ladelogik
//! bleibt fail-closed — Zeilen, die der Parser verwirft, gewähren weiterhin
//! nichts und werden hier nur sichtbar gemacht.
//!
//! # Fehler
//! Deutsche `String`-Fehler; bei einem Fehler wird nichts geschrieben.

use std::path::Path;

use harw_node_listener::{DeviceRecord, DeviceRegistry, RejectedLine, TierChangeError};
use harw_types::{DeviceId, PermissionTier};
use serde_json::json;

use crate::cli::{DeviceAction, DeviceTier, GlobalArgs};
use crate::output::{Printer, render_table};

/// Längste Zeichenzahl einer verworfenen Zeile in der Warnung.
const MAX_WARNING_CHARS: usize = 120;

/// Führt `harw device …` aus.
///
/// # Errors
/// Ein deutscher Fehlertext bei nicht auflösbarem Home, unlesbarer oder
/// nicht schreibbarer Registry, unbekanntem/mehrdeutigem/widerrufenem Gerät
/// oder fehlendem `--yes-owner`.
pub(crate) fn run(g: &GlobalArgs, action: DeviceAction) -> Result<(), String> {
    let home = crate::home::resolve_home(g.home.clone())?;
    let dir = crate::session_serve_remote::remote_state_dir(&home)?;
    let printer = Printer::new(g.output());
    match action {
        DeviceAction::List => {
            let report = list(&dir)?;
            for warning in report.warnings() {
                eprintln!("{warning}");
            }
            printer.value(&report.text(), report.json())
        }
        DeviceAction::SetTier {
            device,
            tier,
            yes_owner,
        } => {
            let outcome = set_tier(&dir, &device, tier_of(tier), yes_owner)?;
            printer.value(&outcome.text(), outcome.json())
        }
        DeviceAction::Revoke { device } => {
            let outcome = revoke(&dir, &device)?;
            printer.value(&outcome.text(), outcome.json())
        }
    }
}

/// Der `ValueEnum` der Grammatik als Berechtigungsstufe.
pub(crate) fn tier_of(tier: DeviceTier) -> PermissionTier {
    match tier {
        DeviceTier::Observer => PermissionTier::Observer,
        DeviceTier::Operator => PermissionTier::Operator,
        DeviceTier::Maintainer => PermissionTier::Maintainer,
        DeviceTier::Owner => PermissionTier::Owner,
    }
}

/// Name der Stufe, wie ihn die Registry-Datei schreibt.
pub(crate) fn tier_name(tier: PermissionTier) -> &'static str {
    match tier {
        PermissionTier::Observer => "observer",
        PermissionTier::Operator => "operator",
        PermissionTier::Maintainer => "maintainer",
        PermissionTier::Owner => "owner",
    }
}

/// Ersetzt Steuerzeichen (z. B. ANSI-Escapes aus einem Label) durch `?`, damit
/// eine Registry-Zeile das Terminal nicht steuern kann.
pub(crate) fn clean(text: &str) -> String {
    text.chars()
        .map(|c| if c.is_control() { '?' } else { c })
        .collect()
}

fn parse_device(raw: &str) -> Result<DeviceId, String> {
    DeviceId::try_from_str(raw)
        .map_err(|error| format!("Ungültige Geräte-ID '{}': {error}", clean(raw)))
}

/// Ergebnis von `harw device list`.
#[derive(Debug)]
pub(crate) struct ListReport {
    path: String,
    records: Vec<DeviceRecord>,
    rejected: Vec<RejectedLine>,
    /// Kurze Schlüssel-Fingerabdrücke aus `node-peers.conf`, je node_id.
    fingerprints: std::collections::BTreeMap<String, String>,
}

/// Liest die Registry unter `remote_dir` samt verworfener Zeilen.
///
/// # Errors
/// Ein Fehlertext, wenn die Datei existiert, aber nicht lesbar ist.
pub(crate) fn list(remote_dir: &Path) -> Result<ListReport, String> {
    let registry = DeviceRegistry::new(remote_dir);
    let scan = registry
        .scan()
        .map_err(|error| format!("Geräte-Registry nicht lesbar: {error}"))?;
    // Optional: eine fehlende oder unlesbare Peers-Datei ist hier kein Fehler.
    let fingerprints =
        std::fs::read_to_string(remote_dir.join(crate::session_listener::PEERS_FILE))
            .map(|text| crate::session_listener::peer_fingerprints(&text))
            .unwrap_or_default();
    Ok(ListReport {
        path: registry.path().display().to_string(),
        records: scan.records,
        rejected: scan.rejected,
        fingerprints,
    })
}

impl ListReport {
    /// Eine Warnung je vom Parser verworfener Zeile (mit Zeilennummer).
    pub(crate) fn warnings(&self) -> Vec<String> {
        self.rejected
            .iter()
            .map(|line| {
                let shown: String = clean(&line.text).chars().take(MAX_WARNING_CHARS).collect();
                format!(
                    "Warnung: {} Zeile {} wird vom Parser verworfen und gewährt nichts: {shown}",
                    self.path, line.line
                )
            })
            .collect()
    }

    /// Tabelle (oder Hinweis bei leerer Registry).
    pub(crate) fn text(&self) -> String {
        if self.records.is_empty() {
            return format!("Keine Geräte enrollt ({}).", self.path);
        }
        let rows: Vec<Vec<String>> = self
            .records
            .iter()
            .map(|r| {
                vec![
                    clean(r.device.as_str()),
                    clean(r.node_id.as_str()),
                    clean(r.tenant.as_str()),
                    tier_name(r.tier).to_owned(),
                    status_name(r).to_owned(),
                    clean(&r.label),
                    self.fingerprints
                        .get(r.node_id.as_str())
                        .cloned()
                        .unwrap_or_else(|| "-".to_owned()),
                ]
            })
            .collect();
        render_table(
            &[
                "DEVICE",
                "NODE_ID",
                "TENANT",
                "TIER",
                "STATUS",
                "LABEL",
                "FINGERPRINT",
            ],
            &rows,
        )
        .trim_end()
        .to_owned()
    }

    /// `{"registry", "devices": [...], "rejected_lines": [...]}`.
    pub(crate) fn json(&self) -> serde_json::Value {
        let devices: Vec<_> = self
            .records
            .iter()
            .map(|r| {
                json!({
                    "device": r.device.as_str(),
                    "node_id": r.node_id.as_str(),
                    "tenant": r.tenant.as_str(),
                    "tier": tier_name(r.tier),
                    "status": status_name(r),
                    "label": r.label,
                    "approve_optin": r.approve_optin,
                    "fingerprint": self.fingerprints.get(r.node_id.as_str()),
                })
            })
            .collect();
        let rejected: Vec<_> = self
            .rejected
            .iter()
            .map(|l| json!({ "line": l.line, "text": l.text }))
            .collect();
        json!({ "registry": self.path, "devices": devices, "rejected_lines": rejected })
    }
}

fn status_name(record: &DeviceRecord) -> &'static str {
    if record.revoked { "revoked" } else { "active" }
}

/// Ergebnis von `harw device set-tier`.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct SetTierOutcome {
    device: String,
    before: PermissionTier,
    after: PermissionTier,
    written: bool,
}

/// Setzt den Tier von `device`.
///
/// Ein Erhöhen auf `owner` verlangt `yes_owner`; ohne das Flag wird nichts
/// geschrieben. Unbekanntes, mehrdeutiges oder widerrufenes Gerät ist ein
/// Fehler, ebenfalls ohne Schreiben.
///
/// # Errors
/// Ein deutscher Fehlertext (siehe oben) oder ein E/A-Fehler.
pub(crate) fn set_tier(
    remote_dir: &Path,
    device: &str,
    tier: PermissionTier,
    yes_owner: bool,
) -> Result<SetTierOutcome, String> {
    let id = parse_device(device)?;
    let registry = DeviceRegistry::new(remote_dir);
    let path = registry.path().display().to_string();
    if tier == PermissionTier::Owner && !yes_owner {
        let scan = registry
            .scan()
            .map_err(|error| format!("Geräte-Registry nicht lesbar: {error}"))?;
        let mut hits = scan.records.iter().filter(|r| r.device == id);
        if let (Some(current), None) = (hits.next(), hits.next()) {
            if current.tier != PermissionTier::Owner {
                return Err(format!(
                    "Das Erhöhen von Gerät '{}' ({} -> owner) verlangt --yes-owner; nichts geschrieben.",
                    clean(id.as_str()),
                    tier_name(current.tier)
                ));
            }
        }
    }
    let change = registry.set_tier(&id, tier).map_err(|error| match error {
        TierChangeError::UnknownDevice => format!(
            "Gerät '{}' ist in {path} nicht eingetragen; nichts geschrieben.",
            clean(id.as_str())
        ),
        TierChangeError::Ambiguous { count } => format!(
            "{count} Einträge in {path} passen auf Gerät '{}' (mehrdeutig); nichts geschrieben. Die Datei muss bereinigt werden.",
            clean(id.as_str())
        ),
        TierChangeError::Revoked => format!(
            "Gerät '{}' ist widerrufen; sein Tier wird nicht geändert.",
            clean(id.as_str())
        ),
        TierChangeError::Io(detail) => format!("Geräte-Registry nicht schreibbar: {detail}"),
    })?;
    Ok(SetTierOutcome {
        device: id.as_str().to_owned(),
        before: change.before.tier,
        after: change.after.tier,
        written: change.written,
    })
}

impl SetTierOutcome {
    pub(crate) fn text(&self) -> String {
        let device = clean(&self.device);
        if !self.written {
            return format!(
                "Gerät '{device}' hat den Tier {} bereits; nichts geändert.",
                tier_name(self.after)
            );
        }
        format!(
            "Gerät '{device}': Tier {} -> {}.\nHinweis: wirkt beim nächsten Handshake des Geräts; eine bestehende Verbindung behält ihren bisherigen Tier bis dahin.",
            tier_name(self.before),
            tier_name(self.after)
        )
    }

    pub(crate) fn json(&self) -> serde_json::Value {
        json!({
            "device": self.device,
            "before": tier_name(self.before),
            "after": tier_name(self.after),
            "written": self.written,
        })
    }
}

/// Ergebnis von `harw device revoke`.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct RevokeOutcome {
    device: String,
    changed: usize,
}

/// Widerruft `device` (idempotent: ein bereits widerrufenes Gerät ist `Ok`
/// mit `changed == 0`).
///
/// # Errors
/// Unbekanntes Gerät (kein Eintrag) oder ein E/A-Fehler.
pub(crate) fn revoke(remote_dir: &Path, device: &str) -> Result<RevokeOutcome, String> {
    let id = parse_device(device)?;
    let registry = DeviceRegistry::new(remote_dir);
    let scan = registry
        .scan()
        .map_err(|error| format!("Geräte-Registry nicht lesbar: {error}"))?;
    if !scan.records.iter().any(|r| r.device == id) {
        return Err(format!(
            "Gerät '{}' ist in {} nicht eingetragen; nichts geändert.",
            clean(id.as_str()),
            registry.path().display()
        ));
    }
    let changed = registry
        .mark_revoked(&id)
        .map_err(|error| format!("Geräte-Registry nicht schreibbar: {error}"))?;
    Ok(RevokeOutcome {
        device: id.as_str().to_owned(),
        changed,
    })
}

impl RevokeOutcome {
    pub(crate) fn text(&self) -> String {
        let device = clean(&self.device);
        let head = if self.changed == 0 {
            format!("Gerät '{device}' war bereits widerrufen; nichts zu tun.")
        } else {
            format!(
                "Gerät '{device}' widerrufen (geänderte Einträge: {}).",
                self.changed
            )
        };
        format!(
            "{head}\nHinweis: neue Handshakes werden sofort abgelehnt (die Registry wird bei jedem Handshake gelesen); ein laufendes Gateway kappt bestehende Verbindungen beim nächsten Registry-Scan (Standard: alle 5 s)."
        )
    }

    pub(crate) fn json(&self) -> serde_json::Value {
        json!({ "device": self.device, "changed": self.changed })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult, ctx};

    const FILE: &str = "# devices of this host\n\
        node-a|dev-1|acme|observer|active|phone\n\
        \n\
        this line is broken\n\
        node-b|dev-2|acme|operator|active|laptop|approve\n\
        # end\n";

    /// Temp-Home mit Registry im echten Zustandsverzeichnis des Profils.
    fn fixture(text: &str) -> TestResult<(tempfile::TempDir, std::path::PathBuf)> {
        let home = tempfile::tempdir().map_err(ctx("temp home"))?;
        let dir = crate::session_serve_remote::remote_state_dir(home.path())
            .map_err(TestError::Unexpected)?;
        std::fs::create_dir_all(&dir).map_err(ctx("remote dir"))?;
        std::fs::write(dir.join("node-devices.conf"), text).map_err(ctx("write registry"))?;
        Ok((home, dir))
    }

    fn read(dir: &Path) -> TestResult<String> {
        std::fs::read_to_string(dir.join("node-devices.conf")).map_err(ctx("read registry"))
    }

    #[test]
    fn state_dir_is_the_gateways_remote_directory() -> TestResult {
        let home = tempfile::tempdir().map_err(ctx("temp home"))?;
        let dir = crate::session_serve_remote::remote_state_dir(home.path())
            .map_err(TestError::Unexpected)?;
        let relative = dir.strip_prefix(home.path()).map_err(ctx("under home"))?;
        let parts: Vec<_> = relative.iter().map(|p| p.to_string_lossy()).collect();
        assert_eq!(parts.first().map(AsRef::as_ref), Some("profiles"));
        assert_eq!(
            parts
                .iter()
                .rev()
                .take(2)
                .map(AsRef::as_ref)
                .collect::<Vec<&str>>(),
            vec!["remote", "session-host"]
        );
        Ok(())
    }

    #[test]
    fn list_of_a_missing_registry_is_empty() -> TestResult {
        let home = tempfile::tempdir().map_err(ctx("temp home"))?;
        let dir = crate::session_serve_remote::remote_state_dir(home.path())
            .map_err(TestError::Unexpected)?;
        let report = list(&dir).map_err(TestError::Unexpected)?;
        assert!(report.text().starts_with("Keine Geräte enrollt"));
        assert!(report.warnings().is_empty());
        assert_eq!(report.json()["devices"], json!([]));
        Ok(())
    }

    #[test]
    fn list_shows_rows_and_warns_about_the_broken_line() -> TestResult {
        let (_home, dir) = fixture(FILE)?;
        let report = list(&dir).map_err(TestError::Unexpected)?;
        let text = report.text();
        for needle in ["DEVICE", "NODE_ID", "TENANT", "TIER", "STATUS", "LABEL"] {
            assert!(text.contains(needle), "{text}");
        }
        assert!(text.contains("dev-1") && text.contains("observer") && text.contains("phone"));
        assert!(text.contains("dev-2") && text.contains("operator") && text.contains("laptop"));
        let warnings = report.warnings();
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(warnings[0].contains("Zeile 4"), "{}", warnings[0]);
        assert!(warnings[0].contains("this line is broken"));
        let json = report.json();
        assert_eq!(json["devices"].as_array().map(Vec::len), Some(2));
        assert_eq!(json["devices"][1]["approve_optin"], json!(true));
        assert_eq!(json["rejected_lines"][0]["line"], json!(4));
        Ok(())
    }

    #[test]
    fn list_shows_a_key_fingerprint_when_the_peer_is_pinned() -> TestResult {
        let (_home, dir) = fixture(FILE)?;
        let key = "ab".repeat(1952);
        std::fs::write(
            dir.join("node-peers.conf"),
            format!("node-a|{key}\nnode-b|zz\n"),
        )
        .map_err(ctx("peers"))?;
        let report = list(&dir).map_err(TestError::Unexpected)?;
        let expected = {
            let bytes = vec![0xab_u8; 1952];
            let hash = blake3::hash(&bytes);
            let hex: String = hash.as_bytes()[..8]
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect();
            format!("blake3:{hex}")
        };
        let json = report.json();
        assert_eq!(json["devices"][0]["fingerprint"], json!(expected));
        // node-b has an invalid key: no fingerprint, no error.
        assert_eq!(json["devices"][1]["fingerprint"], json!(null));
        let text = report.text();
        assert!(
            text.contains("FINGERPRINT") && text.contains(&expected),
            "{text}"
        );
        // No peers file at all is fine.
        std::fs::remove_file(dir.join("node-peers.conf")).map_err(ctx("rm"))?;
        let none = list(&dir).map_err(TestError::Unexpected)?;
        assert_eq!(none.json()["devices"][0]["fingerprint"], json!(null));
        Ok(())
    }

    #[test]
    fn list_neutralises_control_characters() -> TestResult {
        let (_home, dir) = fixture("n|d|t|observer|active|\u{1b}[31mred\n\u{1b}[2J bad\n")?;
        let report = list(&dir).map_err(TestError::Unexpected)?;
        assert!(!report.text().contains('\u{1b}'));
        assert!(report.warnings().iter().all(|w| !w.contains('\u{1b}')));
        Ok(())
    }

    #[test]
    fn set_tier_changes_only_the_target_line_and_keeps_comments() -> TestResult {
        let (_home, dir) = fixture(FILE)?;
        let outcome = set_tier(&dir, "dev-1", PermissionTier::Operator, false)
            .map_err(TestError::Unexpected)?;
        assert_eq!(outcome.before, PermissionTier::Observer);
        assert_eq!(outcome.after, PermissionTier::Operator);
        assert!(outcome.text().contains("observer -> operator"));
        assert_eq!(
            read(&dir)?,
            FILE.replace(
                "node-a|dev-1|acme|observer|active|phone",
                "node-a|dev-1|acme|operator|active|phone"
            )
        );
        Ok(())
    }

    #[test]
    fn owner_without_yes_owner_is_refused_and_writes_nothing() -> TestResult {
        let (_home, dir) = fixture(FILE)?;
        let result = set_tier(&dir, "dev-1", PermissionTier::Owner, false);
        let error = result.err().ok_or(TestError::Missing("an error"))?;
        assert!(error.contains("--yes-owner"), "{error}");
        assert_eq!(read(&dir)?, FILE);
        Ok(())
    }

    #[test]
    fn owner_with_yes_owner_is_written_and_reread_by_the_parser() -> TestResult {
        let (_home, dir) = fixture(FILE)?;
        set_tier(&dir, "dev-1", PermissionTier::Owner, true).map_err(TestError::Unexpected)?;
        // Round trip through the existing parser (`records`), not our own view.
        let records = DeviceRegistry::new(&dir)
            .records()
            .map_err(ctx("records"))?;
        let dev1 = records
            .iter()
            .find(|r| r.device.as_str() == "dev-1")
            .ok_or(TestError::Missing("dev-1"))?;
        assert_eq!(dev1.tier, PermissionTier::Owner);
        assert_eq!(records.len(), 2);
        // Already owner: no flag needed, nothing to write.
        let again =
            set_tier(&dir, "dev-1", PermissionTier::Owner, false).map_err(TestError::Unexpected)?;
        assert!(!again.written);
        Ok(())
    }

    #[test]
    fn lowering_from_owner_needs_no_flag() -> TestResult {
        let (_home, dir) = fixture("n|d|t|owner|active|x\n")?;
        set_tier(&dir, "d", PermissionTier::Observer, false).map_err(TestError::Unexpected)?;
        assert_eq!(read(&dir)?, "n|d|t|observer|active|x\n");
        Ok(())
    }

    #[test]
    fn unknown_ambiguous_and_invalid_device_write_nothing() -> TestResult {
        let text = "n1|dup|t|observer|active|a\nn2|dup|t|observer|active|b\n";
        let (_home, dir) = fixture(text)?;
        for (device, needle) in [
            ("nope", "nicht eingetragen"),
            ("dup", "mehrdeutig"),
            ("", "Ungültige Geräte-ID"),
        ] {
            let error = set_tier(&dir, device, PermissionTier::Operator, false)
                .err()
                .ok_or(TestError::Missing("an error"))?;
            assert!(error.contains(needle), "{device}: {error}");
        }
        // Even with --yes-owner an ambiguous device stays untouched.
        assert!(set_tier(&dir, "dup", PermissionTier::Owner, true).is_err());
        assert_eq!(read(&dir)?, text);
        Ok(())
    }

    #[test]
    fn revoke_is_idempotent_and_takes_effect_for_record_for() -> TestResult {
        let (_home, dir) = fixture(FILE)?;
        let first = revoke(&dir, "dev-2").map_err(TestError::Unexpected)?;
        assert_eq!(first.changed, 1);
        assert!(first.text().contains("nächsten Handshake") || first.text().contains("Handshakes"));
        let second = revoke(&dir, "dev-2").map_err(TestError::Unexpected)?;
        assert_eq!(second.changed, 0);
        assert!(second.text().contains("bereits widerrufen"));
        let node = harw_types::NodeId::try_from_str("node-b").map_err(ctx("node id"))?;
        let record = DeviceRegistry::new(&dir)
            .record_for(&node)
            .map_err(ctx("record_for"))?
            .ok_or(TestError::Missing("record"))?;
        assert!(record.revoked);
        assert!(revoke(&dir, "nope").is_err());
        Ok(())
    }

    #[test]
    fn tier_changes_on_a_revoked_device_are_refused() -> TestResult {
        let (_home, dir) = fixture(FILE)?;
        revoke(&dir, "dev-1").map_err(TestError::Unexpected)?;
        let before = read(&dir)?;
        let error = set_tier(&dir, "dev-1", PermissionTier::Operator, false)
            .err()
            .ok_or(TestError::Missing("an error"))?;
        assert!(error.contains("widerrufen"), "{error}");
        assert_eq!(read(&dir)?, before);
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn written_files_are_mode_0600() -> TestResult {
        use std::os::unix::fs::PermissionsExt as _;
        let (_home, dir) = fixture(FILE)?;
        let path = dir.join("node-devices.conf");
        let mode = |p: &Path| -> TestResult<u32> {
            Ok(std::fs::metadata(p)
                .map_err(ctx("metadata"))?
                .permissions()
                .mode()
                & 0o777)
        };
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644))
            .map_err(ctx("chmod"))?;
        set_tier(&dir, "dev-1", PermissionTier::Operator, false).map_err(TestError::Unexpected)?;
        assert_eq!(mode(&path)?, 0o600);
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644))
            .map_err(ctx("chmod"))?;
        revoke(&dir, "dev-2").map_err(TestError::Unexpected)?;
        assert_eq!(mode(&path)?, 0o600);
        Ok(())
    }

    #[test]
    fn run_works_against_a_temp_home() -> TestResult {
        let (home, _dir) = fixture(FILE)?;
        let global = GlobalArgs {
            home: Some(home.path().to_path_buf()),
            ..GlobalArgs::default()
        };
        run(&global, DeviceAction::List).map_err(TestError::Unexpected)?;
        let error = run(
            &global,
            DeviceAction::SetTier {
                device: "dev-1".to_owned(),
                tier: DeviceTier::Owner,
                yes_owner: false,
            },
        )
        .err()
        .ok_or(TestError::Missing("an error"))?;
        assert!(error.contains("--yes-owner"));
        Ok(())
    }
}
