//! Integrationstests der Fassade `harw-dod` (Knoten AW6-09).
//!
//! Deckt die im Auftrag verlangten Szenarien ab:
//! 1. je Bereich ("beobachte den Host", "bewerte einen Befund") ein
//!    Beispiel, das die Fassade tatsächlich durchreicht;
//! 2. die Standardausstattung (kein `privileged`-Feature) zieht keine der
//!    drei Fähigkeitsklassen aus `xtask/src/gate_privileges.rs`s
//!    `CRATE_PRIVILEGE`-Tabelle als Cargo-Abhängigkeitskante herein — geprüft
//!    gegen die tatsächliche `Cargo.toml`, nach dem Vorbild von
//!    `harw-dod-flow`s `test_cargo_toml_declares_no_aya_dependency`.
//!
//! Der `compile_fail`-Doctest, der belegt, dass diese Fassade keinen Pfad zu
//! `Action<Authorized>` öffnet, steht im `//!`-Block von `src/lib.rs` selbst
//! (dort, wo die Entscheidung begründet wird), nicht hier.

mod common;

use common::{TestError, TestResult, ctx};
use harw_authority::NetworkScope;
use harw_dod::{
    Actor, Capability, CpuSensor, EgressFlowRule, EventKind, ReadScope, Rule, RuleContext,
    SecurityEvent, Sensor, SensorHandle, Verdict, run_rules,
};
use harw_types::{FindingId, SensorId};

/// "Beobachte den Host": ein Griff bauen, einen der neun re-exportierten
/// Sensor-Typen daraus konstruieren, abrufen — ausschließlich über
/// `harw_dod`-Namen, keine Innencrate direkt genannt. Liest eine
/// synthetische `stat`-Datei in einem `tempfile`-Verzeichnis, nicht das
/// reale `/proc` (Testrichtlinie: kein Dateisystem außerhalb eines eigenen
/// Tempdirs).
#[test]
fn test_facade_builds_and_polls_a_sensor_end_to_end() -> TestResult {
    let root = tempfile::tempdir().map_err(ctx("tempdir"))?;
    std::fs::write(root.path().join("stat"), "cpu 10 20 30 40\n")
        .map_err(ctx("write synthetic stat file"))?;

    let scope = ReadScope::from_roots([root.path().to_path_buf()]);
    let handle =
        SensorHandle::new(SensorId::from_str("cpu-0"), Capability::ReadProcStat).bind(scope);
    let sensor = CpuSensor::from(handle);

    let reading = sensor.poll(jiff::Timestamp::UNIX_EPOCH).map_err(ctx(
        "cpu sensor poll must succeed against a well-formed synthetic stat file",
    ))?;

    assert!(
        !reading.samples.is_empty(),
        "a well-formed cpu line must yield at least one HostSample"
    );
    Ok(())
}

/// "Bewerte einen Befund": eine Regel gegen ein beobachtetes Ereignis
/// auswerten und triagieren -- exakt der Pfad, den ein Konsument nach dem
/// zweiten Beispiel im `//!`-Block von `src/lib.rs` nimmt, hier als
/// Integrationstest mit echten Assertions statt nur "kompiliert".
#[test]
fn test_facade_evaluates_a_finding_end_to_end() -> TestResult {
    let scope = NetworkScope::from_hosts(["docs.rs".to_owned()]);
    let events = vec![SecurityEvent {
        sensor: SensorId::from_str("net-0"),
        observed_at: jiff::Timestamp::UNIX_EPOCH,
        actor: None::<Actor>,
        kind: EventKind::EgressFlow {
            destination: "evil.example.com".to_owned(),
            port: 443,
        },
    }];
    let ctx = RuleContext {
        now: jiff::Timestamp::UNIX_EPOCH,
        samples: &[],
        events: &events,
        baselines: &[],
        network_scope: &scope,
    };
    let fired: Vec<_> = run_rules(&ctx)
        .into_iter()
        .filter(|finding| finding.rule_id() == EgressFlowRule.id())
        .collect();
    assert_eq!(
        fired.len(),
        1,
        "a destination outside the allowed scope must trigger exactly one finding"
    );

    let finding = fired
        .into_iter()
        .next()
        .ok_or(TestError::Missing("checked above"))?;
    // `run_rules` liefert `Finding<Raw>`; `Raw -> RuleChecked` ist in
    // `harw-dod-rules` `pub(crate)`. Die Testhilfe `triaged_finding_for_test`
    // (Feature `test-support`) geht intern exakt `raw(..).check(id)` + `triage`.
    let triaged = harw_dod_rules::triaged_finding_for_test(
        finding.rule_id(),
        finding.kind(),
        finding.severity(),
        finding.hardness(),
        finding.summary(),
        finding.observed_at(),
        FindingId::new(),
        Verdict::Confirmed,
    );
    assert_eq!(*triaged.verdict(), Verdict::Confirmed);
    Ok(())
}

/// Die Standardausstattung (kein `privileged`-Feature) zieht keine der drei
/// Fähigkeitsklassen aus `xtask/src/gate_privileges.rs`s
/// `CRATE_PRIVILEGE`-Tabelle herein: weder als unbedingte
/// `[dependencies]`-Zeile noch als aktiviertes `default`-Feature. Geprüft
/// gegen den tatsächlichen Manifest-Text, nicht gegen das, was diese Crate
/// zu tun *behauptet*.
#[test]
fn test_default_feature_set_pulls_in_no_privileged_crate() -> TestResult {
    let manifest = include_str!("../Cargo.toml");

    for privileged_crate in [
        "harw-dod-authlog",
        "harw-dod-fsmon",
        "harw-dod-bpf",
        "harw-dod-procmon",
        "harw-dod-flow",
    ] {
        let needle = format!("{privileged_crate} = {{ path");
        let line = manifest
            .lines()
            .find(|line| line.trim_start().starts_with(&needle))
            .ok_or_else(|| {
                TestError::Unexpected(format!(
                    "{privileged_crate} must be declared as a path dependency"
                ))
            })?;
        assert!(
            line.contains("optional = true"),
            "{privileged_crate} must be `optional = true`, reachable only via the `privileged` feature: {line}"
        );
    }

    assert!(
        !manifest.contains("default = [\"privileged\"]")
            && !manifest.contains("default = [\"privileged\","),
        "no `[features]` default entry may enable `privileged` -- a bare `harw-dod` dependency must not carry any capability class"
    );
    Ok(())
}

/// Belegt, dass die fünf Fähigkeits-Crates tatsächlich hinter dem
/// `privileged`-Feature stehen (nicht nur `optional`, sondern von genau
/// diesem Feature aktiviert) -- Gegentest zum vorigen: ein `optional = true`
/// ohne zugehörigen Feature-Eintrag wäre für jeden Konsumenten unerreichbar
/// und ebenso falsch wie ein unbedingter Reexport.
#[test]
fn test_privileged_feature_activates_exactly_the_five_capability_crates() -> TestResult {
    let manifest = include_str!("../Cargo.toml");
    let features_section = manifest
        .split("[features]")
        .nth(1)
        .ok_or(TestError::Missing(
            "manifest must declare a [features] section",
        ))?
        .split("[dev-dependencies]")
        .next()
        .ok_or(TestError::Missing(
            "[features] section must be followed by [dev-dependencies]",
        ))?;

    for privileged_crate in [
        "harw-dod-authlog",
        "harw-dod-fsmon",
        "harw-dod-bpf",
        "harw-dod-procmon",
        "harw-dod-flow",
    ] {
        let needle = format!("dep:{privileged_crate}");
        assert!(
            features_section.contains(&needle),
            "the `privileged` feature must list `{needle}`"
        );
    }
    Ok(())
}
