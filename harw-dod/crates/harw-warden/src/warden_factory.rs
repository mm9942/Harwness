//! Zusammenbau des produktiven [`harw_dod_warden::Warden`]: echte
//! cgroup-v2-Ausführung, echte Zielbeobachtung, echtes Audit,
//! Netzisolation (nur wenn die Policy sie erlaubt), Warden-eigene Policy,
//! Schlüsselring und persistenter Zustand — plus der Sandbox-Bedarf, den
//! Landlock und seccomp danach noch erlauben müssen.
//!
//! # Warum ein eigenes Modul, nicht direkt in `main.rs`
//! `main::run` soll nur die Startreihenfolge orchestrieren, nicht selbst
//! entscheiden, welche Ausführungs- und Audit-Implementierungen ein
//! produktiver Warden bekommt — dieselbe Trennung, die
//! `harw-sentinel::sensors::build_sensors` für seine Sensor-Registrierung
//! zieht.
//!
//! # Was zusammengebaut wird
//! - `FreezeCgroup`/`ThawCgroup`/`KillProcessTree` und der Hold-Timer laufen
//!   über je eine eigene [`harw_dod_warden::CgroupV2Executor`]-Instanz
//!   (echte Dateisystemschreibzugriffe auf `cgroup_root`).
//! - `IsolateNetwork`/`LiftIsolation`: erlaubt die Policy eine dieser
//!   Aktionen ([`crate::isolation::network_actions_enabled`]), läuft sie über
//!   [`crate::isolation::NftNetworkIsolator`] mit dem `nft`-Kindprozess
//!   (nftables-Tabelle `inet harw_warden`, braucht `CAP_NET_ADMIN` und
//!   `nft` ≥ 0.9.4), und [`ProductionWarden::sandbox`] trägt dessen Bedarf.
//!   Sonst bekommt der Warden [`crate::isolation::DisabledNetworkIsolator`]
//!   und einen leeren Bedarf: kein `execve`, kein `socket`, keine
//!   Ausführungsrechte unter Landlock.
//! - Die Zielprüfung (Kernel-ID unter dem signierten Pfad) läuft über
//!   [`harw_dod_warden::CgroupFsObserver`] auf derselben Wurzel; der
//!   `CgroupV2Executor` prüft die Kernel-ID beim Eingriff noch einmal am
//!   geöffneten Verzeichnis und schreibt über denselben Deskriptor (B21).
//! - Die Eindämmungsmarker für harw liegen unter `holds_dir`
//!   ([`harw_dod_warden::ContainmentMarkers`], Vorgabe
//!   [`harw_dod_warden::DEFAULT_HOLDS_DIR`], `--holds-dir`); der Zusammenbau
//!   greift nicht darauf zu, erst der Abgleich beim Start
//!   ([`harw_dod_warden::Warden::reconcile_containments`]), jeder Freeze und
//!   jedes Freigeben.
//! - Das Audit-Ziel ist [`crate::audit::TracingAuditSink`].
//! - Schlüsselring, Policy und Zustand lädt `main::run` vorher (vor
//!   Landlock) und reicht sie hier nur durch.
//!
//! # Nebenläufigkeit
//! Reiner Zusammenbau ohne Dateisystem- oder Netzzugriff; darf deshalb vor
//! Landlock und seccomp laufen.

use std::path::Path;

use harw_dod_warden::{
    CgroupFsObserver, CgroupV2Executor, ContainmentMarkers, NetworkIsolator, Warden, WardenParts,
    WardenPolicy, WardenState,
};
use harw_dod_warden_proto::KeyRing;

use crate::audit::TracingAuditSink;
use crate::isolation::{
    DisabledNetworkIsolator, NftNetworkIsolator, SandboxNeeds, network_actions_enabled,
};

/// Der produktive Warden und der Sandbox-Bedarf seines Netfilter-Backends.
///
/// Kein `Debug`: [`Warden`] selbst hat keines (er hält Trait-Objekte).
pub struct ProductionWarden {
    /// Der einsatzbereite Warden.
    pub warden: Warden,
    /// Was Landlock und seccomp zusätzlich erlauben müssen (leer ohne
    /// erlaubte Netzaktion).
    pub sandbox: SandboxNeeds,
}

/// Baut den produktiven Warden für das gegebene cgroup-v2-Wurzelverzeichnis.
///
/// # Arguments
/// - `cgroup_root` (`&Path`): Wurzelverzeichnis der cgroup-v2-Hierarchie
///   (Produktion: `/sys/fs/cgroup`, siehe [`crate::cli::DEFAULT_CGROUP_ROOT`]).
/// - `holds_dir` (`&Path`): Indexverzeichnis der Eindämmungsmarker für harw
///   (Produktion: [`harw_dod_warden::DEFAULT_HOLDS_DIR`]).
/// - `keys` (`KeyRing`): gültige Proof-Schlüssel aus `proof_key_dir`.
/// - `policy` (`WardenPolicy`): Policy aus `warden.toml`.
/// - `state` (`WardenState`): geöffnetes StateDirectory.
///
/// # Returns
/// Den Warden samt Sandbox-Bedarf.
#[must_use]
pub fn build_production_warden(
    cgroup_root: &Path,
    holds_dir: &Path,
    keys: KeyRing,
    policy: WardenPolicy,
    state: WardenState,
) -> ProductionWarden {
    let (isolator, sandbox) = network_isolator(cgroup_root, &policy);
    let warden = Warden::new(WardenParts {
        freezer: Box::new(CgroupV2Executor::new(cgroup_root)),
        thawer: Box::new(CgroupV2Executor::new(cgroup_root)),
        isolator,
        killer: Box::new(CgroupV2Executor::new(cgroup_root)),
        observer: Box::new(CgroupFsObserver::new(cgroup_root)),
        audit: Box::new(TracingAuditSink::new()),
        keys,
        policy,
        state,
        markers: ContainmentMarkers::new(holds_dir),
    });
    ProductionWarden { warden, sandbox }
}

// Netfilter-Backend nur bei erlaubter Netzaktion, sonst der abgeschaltete
// Isolator ohne Sandbox-Bedarf.
fn network_isolator(
    cgroup_root: &Path,
    policy: &WardenPolicy,
) -> (Box<dyn NetworkIsolator + Send + Sync>, SandboxNeeds) {
    if network_actions_enabled(policy.actions()) {
        let isolator = NftNetworkIsolator::from_cgroup_root(cgroup_root);
        let sandbox = isolator.sandbox_needs();
        let boxed: Box<dyn NetworkIsolator + Send + Sync> = Box::new(isolator);
        (boxed, sandbox)
    } else {
        let boxed: Box<dyn NetworkIsolator + Send + Sync> = Box::new(DisabledNetworkIsolator);
        (boxed, SandboxNeeds::default())
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};

    use super::build_production_warden;
    use crate::isolation::SandboxNeeds;
    use crate::test_support::{TestError, TestResult, ctx, private_tempdir};
    use harw_dod_warden::{ActionAllowlist, DenyList, WardenOutcome, WardenPolicy, WardenState};
    use harw_dod_warden_proto::{
        AuthorizationBody, CgroupKernelId, CgroupTarget, Denial, EscalationStage, KeyId, KeyRing,
        ProofKey, ProofPolicy, SignedAuthorization, WardenAction, WardenRequest,
    };
    use harw_types::{ApprovalActor, CgroupId, FindingId};
    use jiff::Timestamp;

    const KEY: [u8; 32] = [0x33; 32];

    fn at(second: i64) -> TestResult<Timestamp> {
        Timestamp::from_second(second).map_err(ctx("timestamp"))
    }

    fn keys() -> TestResult<KeyRing> {
        let mut ring = KeyRing::new();
        ring.insert(
            KeyId::new("k1").map_err(ctx("key id"))?,
            ProofKey::from_bytes(KEY),
        )
        .map_err(ctx("ring"))?;
        Ok(ring)
    }

    fn policy() -> TestResult<WardenPolicy> {
        Ok(WardenPolicy::new(
            ProofPolicy::new(60, 5, vec!["harw.slice".to_owned()]).map_err(ctx("proof"))?,
            ActionAllowlist::freeze_and_thaw(),
            DenyList::builtin(),
        ))
    }

    fn request(action: &WardenAction, kernel_id: u64, nonce: u8) -> TestResult<WardenRequest> {
        let body = AuthorizationBody {
            finding_id: FindingId::try_from_str("very-identifiable-finding")
                .map_err(ctx("finding"))?,
            approver: ApprovalActor::Operator {
                id: "operator-1".to_owned(),
            },
            action: action.clone(),
            stage: EscalationStage::RuleTriggered,
            target: CgroupTarget::new(
                action.cgroup().clone(),
                CgroupKernelId::new(kernel_id).map_err(ctx("kid"))?,
                at(1_799_999_000)?,
            ),
            hold_secs: None,
            irreversible_ack: false,
            nonce: [nonce; 16],
            issued_at: at(1_800_000_000)?,
            expires_at: at(1_800_000_030)?,
        };
        Ok(WardenRequest::new(
            action.clone(),
            SignedAuthorization::sign(
                &ProofKey::from_bytes(KEY),
                &KeyId::new("k1").map_err(ctx("key id"))?,
                body,
            ),
        ))
    }

    // Ein Indexverzeichnis wie `/run/harw-warden/holds` (`0755`, eigener Nutzer).
    fn holds_dir(run: &tempfile::TempDir) -> TestResult<std::path::PathBuf> {
        let holds = run.path().join("holds");
        std::fs::create_dir(&holds).map_err(ctx("holds dir"))?;
        std::fs::set_permissions(&holds, std::fs::Permissions::from_mode(0o755))
            .map_err(ctx("chmod holds dir"))?;
        Ok(holds)
    }

    /// Ende zu Ende über die produktive Verdrahtung, aber gegen ein Tempdir
    /// als cgroup-Wurzel: Zielprüfung über die Inode, echtes Schreiben von
    /// `cgroup.freeze`, Ledger im StateDirectory, Marker für harw unter dem
    /// Indexverzeichnis (benannt nach der Inode der cgroup) bis zum Thaw.
    #[test]
    fn test_production_warden_freezes_a_fixture_cgroup_after_inode_check() -> TestResult {
        let root = tempfile::tempdir().map_err(ctx("cgroup root"))?;
        let run = tempfile::tempdir().map_err(ctx("run dir"))?;
        let holds = holds_dir(&run)?;
        let state_dir = private_tempdir()?;
        let job = root.path().join("harw.slice").join("job-1");
        std::fs::create_dir_all(&job).map_err(ctx("mkdir"))?;
        let inode = std::fs::metadata(&job).map_err(ctx("metadata"))?.ino();
        let state =
            WardenState::open(state_dir.path(), at(1_800_000_000)?).map_err(ctx("state"))?;
        let warden = build_production_warden(root.path(), &holds, keys()?, policy()?, state).warden;

        let action = WardenAction::FreezeCgroup {
            cgroup: CgroupId::try_from_str("harw.slice/job-1").map_err(ctx("cgroup"))?,
        };
        // Falsche Kernel-ID: keine Wirkung.
        let wrong = warden.handle(
            &request(&action, inode.saturating_add(1), 1)?,
            at(1_800_000_001)?,
        );
        assert_eq!(wrong, WardenOutcome::Denied(Denial::TargetChanged));
        assert!(!job.join("cgroup.freeze").exists());

        let outcome = warden.handle(&request(&action, inode, 2)?, at(1_800_000_001)?);
        if !matches!(outcome, WardenOutcome::Executed(_)) {
            return Err(TestError::Unexpected(format!(
                "expected Executed, got {outcome:?}"
            )));
        }
        let written =
            std::fs::read_to_string(job.join("cgroup.freeze")).map_err(ctx("freeze written"))?;
        assert_eq!(written, "1");
        let marker = holds.join(inode.to_string());
        assert!(marker.is_file());
        // Der Abgleich (wie nach einem Neustart) lässt die eingefrorene
        // Eindämmung samt Marker stehen.
        let reconciled = warden.reconcile_containments().map_err(ctx("reconcile"))?;
        assert_eq!(reconciled.kept, 1);
        assert_eq!(reconciled.vanished, 0);
        assert_eq!(reconciled.thawed, 0);
        assert!(marker.is_file());

        let thaw = WardenAction::ThawCgroup {
            cgroup: CgroupId::try_from_str("harw.slice/job-1").map_err(ctx("cgroup"))?,
        };
        let thawed = warden.handle(&request(&thaw, inode, 3)?, at(1_800_000_002)?);
        if !matches!(thawed, WardenOutcome::Executed(_)) {
            return Err(TestError::Unexpected(format!(
                "expected Executed, got {thawed:?}"
            )));
        }
        let written =
            std::fs::read_to_string(job.join("cgroup.freeze")).map_err(ctx("thaw written"))?;
        assert_eq!(written, "0");
        assert!(!marker.exists());
        Ok(())
    }

    /// Wird die cgroup zwischen Zielprüfung und Eingriff unter demselben Pfad
    /// neu angelegt, trifft der Freeze die Nachfolgerin nicht: der
    /// Ausführer prüft die Inode am geöffneten Verzeichnis.
    #[test]
    fn test_production_executor_refuses_a_cgroup_recreated_at_the_signed_path() -> TestResult {
        let root = tempfile::tempdir().map_err(ctx("cgroup root"))?;
        let job = root.path().join("harw.slice").join("job-1");
        std::fs::create_dir_all(&job).map_err(ctx("mkdir"))?;
        let inode = std::fs::metadata(&job).map_err(ctx("metadata"))?.ino();
        // Die alte cgroup bleibt (umbenannt) bestehen, damit die neue sicher
        // eine andere Inode bekommt.
        std::fs::rename(&job, root.path().join("harw.slice").join("parked"))
            .map_err(ctx("park"))?;
        std::fs::create_dir(&job).map_err(ctx("recreate"))?;
        let executor = harw_dod_warden::CgroupV2Executor::new(root.path());
        let target = CgroupTarget::new(
            CgroupId::try_from_str("harw.slice/job-1").map_err(ctx("cgroup"))?,
            CgroupKernelId::new(inode).map_err(ctx("kid"))?,
            at(1_799_999_000)?,
        );
        let result = harw_dod_warden::CgroupFreezer::freeze(&executor, &target);
        assert!(matches!(
            result,
            Err(harw_dod_warden::WardenError::TargetChanged)
        ));
        assert!(!job.join("cgroup.freeze").exists());
        Ok(())
    }

    /// Ohne erlaubte Netzaktion (Vorgabe) bekommt der Warden keinen
    /// Sandbox-Bedarf: kein `execve`, kein `socket`, kein `nft`.
    #[test]
    fn test_default_policy_needs_no_backend_sandbox() -> TestResult {
        let state_dir = private_tempdir()?;
        let state =
            WardenState::open(state_dir.path(), at(1_800_000_000)?).map_err(ctx("state"))?;
        let production = build_production_warden(
            std::path::Path::new("/sys/fs/cgroup"),
            std::path::Path::new(harw_dod_warden::DEFAULT_HOLDS_DIR),
            keys()?,
            policy()?,
            state,
        );
        assert_eq!(production.sandbox, SandboxNeeds::default());
        Ok(())
    }

    /// Mit erlaubter Isolation trägt der Bedarf das `nft`-Programm,
    /// `/dev/null` und die Prozessstart-Aufrufe.
    #[test]
    fn test_network_policy_carries_the_nft_sandbox_needs() -> TestResult {
        let state_dir = private_tempdir()?;
        let state =
            WardenState::open(state_dir.path(), at(1_800_000_000)?).map_err(ctx("state"))?;
        let policy = WardenPolicy::new(
            ProofPolicy::new(60, 5, vec!["harw.slice".to_owned()]).map_err(ctx("proof"))?,
            ActionAllowlist::new([
                "freeze-cgroup",
                "thaw-cgroup",
                "isolate-network",
                "lift-isolation",
            ])
            .map_err(ctx("allowlist"))?,
            DenyList::builtin(),
        );
        let production = build_production_warden(
            std::path::Path::new("/sys/fs/cgroup"),
            std::path::Path::new(harw_dod_warden::DEFAULT_HOLDS_DIR),
            keys()?,
            policy,
            state,
        );
        assert!(production.sandbox.read_exec_paths.iter().any(
            |path| path.as_path() == std::path::Path::new(crate::isolation::DEFAULT_NFT_BINARY)
        ));
        assert_eq!(
            production.sandbox.read_write_files,
            vec![std::path::PathBuf::from("/dev/null")]
        );
        assert!(
            production
                .sandbox
                .syscalls
                .iter()
                .any(|syscall| syscall.name == "execve")
        );
        Ok(())
    }

    /// Eine Ablehnung bleibt inhaltsfrei und berührt das Dateisystem nicht.
    #[test]
    fn test_production_warden_denial_is_content_free_and_touches_nothing() -> TestResult {
        let state_dir = private_tempdir()?;
        let state =
            WardenState::open(state_dir.path(), at(1_800_000_000)?).map_err(ctx("state"))?;
        let warden = build_production_warden(
            std::path::Path::new("/nonexistent-harw-warden-mock-root"),
            std::path::Path::new("/nonexistent-harw-warden-mock-holds"),
            keys()?,
            policy()?,
            state,
        )
        .warden;
        let action = WardenAction::FreezeCgroup {
            cgroup: CgroupId::try_from_str("harw.slice/very-identifiable-cgroup")
                .map_err(ctx("cgroup"))?,
        };
        let outcome = warden.handle(&request(&action, 77, 1)?, at(1_800_000_001)?);
        assert_eq!(outcome, WardenOutcome::Denied(Denial::TargetChanged));
        let json = serde_json::to_string(&outcome.to_reply()).map_err(ctx("serializes"))?;
        assert!(!json.contains("very-identifiable"));
        Ok(())
    }
}
