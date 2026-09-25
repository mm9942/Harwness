//! Embedded compiled-agent artifacts (#22 wave 3A).
//!
//! # Beschreibung
//! [`EmbeddedAgent`] hält alles, was `EntryKind::CompiledAgent` (`spec.rs`)
//! und [`crate::config::load_config_embedded`] brauchen, ohne `~/.harw` zu
//! lesen: die typisierte Root-IR ([`harw_agent_dsl::ir_v2::AgentIr`]), die IR
//! jedes weiteren Agenten der Delegationshülle, sowie deren Skill-,
//! Instruktions- und Wissens-Payloads aus dem Blob-Pool des Artefakts
//! (`harw_agent_artifact::bundle`). [`EffectiveRights`] ist das daraus
//! abgeleitete, nur verengbare Rechtebündel (`min(manifest, flags)`).
//!
//! # Vertragsabweichung
//! Der Wave-3-Vertrag nennt `EmbeddedAgent::from_bundle(bundle: Bundle) ->
//! Result<Self, RuntimeError>`. Das lässt sich nicht erfüllen:
//! [`harw_agent_artifact::bundle::Bundle`] speichert nach
//! [`harw_agent_artifact::bundle::Bundle::from_artifact`] nur noch
//! Blob-**Größen** (`blob_sizes`), nie die Bytes selbst —
//! [`harw_agent_artifact::bundle::Bundle::resolve`]/[`..::Bundle::file`]
//! nehmen dafür ausdrücklich `&Artifact` entgegen. [`EmbeddedAgent::payload`]
//! muss aber ohne eine an `self` gebundene Lebenszeit auf ein `Artifact`
//! auskommen (der Aufrufer darf das gelesene Artefakt nach dem Bau wieder
//! freigeben), deshalb nimmt [`EmbeddedAgent::from_bundle`] zusätzlich
//! `&Artifact` entgegen und kopiert jeden referenzierten Blob genau einmal
//! (dedupliziert über den Hash) in einen eigenen Puffer.

use std::collections::{BTreeMap, BTreeSet};

use harw_agent_artifact::{Artifact, ArtifactDigest, Bundle};
use harw_agent_dsl::ir_v2::{AgentIr, Budget, NetworkMode, Permissions};

use crate::error::{RuntimeError, RuntimeResult};

/// Ein geprüftes, vollständig im Speicher gehaltenes Agenten-Artefakt.
///
/// # Nebenläufigkeit
/// Reiner Wert (`Clone`), `Send + Sync`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmbeddedAgent {
    bundle: Bundle,
    blobs: BTreeMap<ArtifactDigest, Vec<u8>>,
    agent_irs: BTreeMap<String, AgentIr>,
    digest: ArtifactDigest,
    rights: EffectiveRights,
}

impl EmbeddedAgent {
    /// Baut ein `EmbeddedAgent` aus einem bereits hash-geprüften [`Bundle`]
    /// (`Bundle::from_artifact`) und dem [`Artifact`], aus dem es gelesen
    /// wurde (siehe Moduldoku „Vertragsabweichung").
    ///
    /// # Errors
    /// [`RuntimeError::Config`], wenn die Root- oder eine Kind-IR nicht als
    /// [`AgentIr`] parst, [`AgentIr::verify_snapshot`] fehlschlägt (die
    /// Snapshot-Prüfung ist die Manipulationserkennung: ein verändertes
    /// Artefakt startet nicht), oder eine Payload-Referenz auf keinen Blob
    /// zeigt (sollte für ein bereits verifiziertes Bundle nicht vorkommen —
    /// hier trotzdem fail-closed erneut geprüft).
    pub fn from_bundle(bundle: Bundle, artifact: &Artifact) -> RuntimeResult<Self> {
        let mut agent_irs = BTreeMap::new();
        let mut blobs: BTreeMap<ArtifactDigest, Vec<u8>> = BTreeMap::new();
        for (id, entry) in &bundle.agents {
            let ir = decode_ir(&entry.ir, id)?;
            agent_irs.insert(id.clone(), ir);
            for reference in &entry.payload_refs {
                if blobs.contains_key(&reference.blake3) {
                    continue;
                }
                let bytes = bundle
                    .resolve(artifact, reference)
                    .ok_or_else(|| RuntimeError::Config {
                        detail: format!(
                            "agent {id}: {} references the missing blob {}",
                            reference.logical_path, reference.blake3
                        ),
                    })?;
                blobs.insert(reference.blake3, bytes.to_vec());
            }
        }
        if !agent_irs.contains_key(&bundle.header.root) {
            return Err(RuntimeError::Config {
                detail: format!(
                    "bundle root '{}' has no agent entry",
                    bundle.header.root
                ),
            });
        }
        let root_permissions = agent_irs[&bundle.header.root].permissions.clone();
        let rights = EffectiveRights::from_manifest(&root_permissions);
        Ok(Self {
            digest: artifact.digest(),
            bundle,
            blobs,
            agent_irs,
            rights,
        })
    }

    /// Die typisierte IR des Wurzel-Agenten.
    ///
    /// # Panics
    /// Nie: [`Self::from_bundle`] stellt sicher, dass die Wurzel-ID einen
    /// Eintrag hat.
    #[must_use]
    pub fn root_ir(&self) -> &AgentIr {
        &self.agent_irs[&self.bundle.header.root]
    }

    /// Die ID des Wurzel-Agenten.
    #[must_use]
    pub fn root_id(&self) -> &str {
        &self.bundle.header.root
    }

    /// Die typisierte IR eines Agenten der Hülle (Wurzel eingeschlossen).
    #[must_use]
    pub fn agent_ir(&self, id: &str) -> Option<&AgentIr> {
        self.agent_irs.get(id)
    }

    /// Das zugrunde liegende Bundle (Kindbeziehungen, Payload-Referenzen).
    #[must_use]
    pub fn bundle(&self) -> &Bundle {
        &self.bundle
    }

    /// Der Identitäts-Hash des Artefakts.
    #[must_use]
    pub fn digest(&self) -> &ArtifactDigest {
        &self.digest
    }

    /// Die aktuell geltenden effektiven Rechte (Manifest, ggf. verengt über
    /// [`Self::with_rights`]).
    #[must_use]
    pub fn rights(&self) -> &EffectiveRights {
        &self.rights
    }

    /// Ersetzt die geltenden Rechte. Aufrufer verengen zuerst über
    /// [`EffectiveRights::narrowed_by`] — dieser Setter selbst prüft nichts,
    /// er ist der reine Ablagepunkt.
    #[must_use]
    pub fn with_rights(mut self, rights: EffectiveRights) -> Self {
        self.rights = rights;
        self
    }

    /// Die Bytes einer Payload eines Agenten (`kind`/`logical_path` genau wie
    /// im Bundle deklariert, z. B. `("skill",
    /// "skills/foo/instructions.md")`, `("instructions",
    /// "instructions/system.md")`, `("knowledge", "knowledge/x.md")`).
    ///
    /// # Rückgabe
    /// `None`, wenn der Agent, die Referenz oder ihr Blob nicht im Bundle
    /// enthalten ist.
    #[must_use]
    pub fn payload(&self, agent_id: &str, kind: &str, logical_path: &str) -> Option<&[u8]> {
        let entry = self.bundle.agents.get(agent_id)?;
        let reference = entry
            .payload_refs
            .iter()
            .find(|reference| reference.kind == kind && reference.logical_path == logical_path)?;
        self.blobs.get(&reference.blake3).map(Vec::as_slice)
    }
}

/// Deserialisiert und verifiziert eine Agenten-IR aus dem Bundle-JSON.
fn decode_ir(value: &serde_json::Value, id: &str) -> RuntimeResult<AgentIr> {
    let ir: AgentIr =
        serde_json::from_value(value.clone()).map_err(|error| RuntimeError::Config {
            detail: format!("agent {id}: invalid agent IR: {error}"),
        })?;
    if !ir.verify_snapshot() {
        return Err(RuntimeError::Config {
            detail: format!(
                "agent {id}: snapshot mismatch — the artifact may have been tampered with"
            ),
        });
    }
    Ok(ir)
}

/// Die aus einem Manifest ([`Permissions`]) abgeleiteten, nur verengbaren
/// Rechte eines eingebetteten Laufs.
///
/// # Beschreibung
/// `EffectiveRights::from_manifest` liest ausschließlich das Manifest;
/// [`Self::narrowed_by`] wendet Laufzeit-Flags an und kann Rechte nur
/// **wegnehmen**, nie hinzufügen — auch `--full-access`
/// ([`RightsFlags::full_access`]) erweitert keine der Mengen unten, es
/// ändert nur, ob ein Aufruf innerhalb dieser Mengen automatisch freigegeben
/// wird.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EffectiveRights {
    /// Effektive Werkzeugnamen (`Permissions::tools`: admittiert minus
    /// verboten).
    pub tools: BTreeSet<String>,
    /// Erreichbare Hosts (`Permissions::network::hosts`); bedeutungslos ohne
    /// [`Self::network_open`].
    pub network_hosts: BTreeSet<String>,
    /// Ob überhaupt ein Netzwerkmodus zugelassen ist
    /// (`NetworkMode::Allowlist`).
    pub network_open: bool,
    /// Ob ein schreibendes Workspace-Werkzeug admittiert ist.
    pub write: bool,
    /// Ob ein Shell-/Prozessstart-Werkzeug admittiert ist.
    pub shell: bool,
    /// Ob ein Host-Werkzeug (außerhalb der Sandbox) admittiert ist.
    pub host: bool,
    /// Ob Werkzeugaufrufe innerhalb der obigen Mengen automatisch freigegeben
    /// werden (`--full-access`). Erweitert nie eine der Mengen selbst.
    pub full_access: bool,
    /// Das Ressourcenbudget des Manifests, falls vorhanden.
    pub budget: Option<Budget>,
}

impl EffectiveRights {
    /// Leitet die unverengten Rechte eines Manifests ab (`full_access:
    /// false`).
    #[must_use]
    pub fn from_manifest(permissions: &Permissions) -> Self {
        Self {
            tools: permissions.tools.iter().cloned().collect(),
            network_hosts: permissions.network.hosts.iter().cloned().collect(),
            network_open: permissions.network.mode == NetworkMode::Allowlist,
            write: permissions.filesystem.write,
            shell: permissions.shell,
            host: permissions.host,
            full_access: false,
            budget: permissions.budget.clone(),
        }
    }

    /// Wendet `flags` an. Verengt ausschließlich: jede Menge wird höchstens
    /// kleiner, nie größer, unabhängig davon, was `flags` verlangt.
    #[must_use]
    pub fn narrowed_by(mut self, flags: &RightsFlags) -> Self {
        for tool in &flags.deny_tools {
            self.tools.remove(tool);
        }
        if flags.no_network {
            self.network_hosts.clear();
            self.network_open = false;
        }
        if flags.read_only {
            self.write = false;
            self.shell = false;
        }
        // `full_access` erweitert keine der Mengen oben — es ändert nur das
        // Freigabeverhalten innerhalb dessen, was das Manifest ohnehin
        // zulässt. Deshalb ist ein direktes Setzen hier keine Erweiterung.
        self.full_access = flags.full_access;
        self.budget = narrow_budget(self.budget, flags.max_tokens);
        self
    }
}

/// Verengt (nie erweitert) `budget.max_tokens` auf `max_tokens`, falls
/// gesetzt.
fn narrow_budget(budget: Option<Budget>, max_tokens: Option<u64>) -> Option<Budget> {
    match (budget, max_tokens) {
        (Some(mut budget), Some(max)) => {
            budget.max_tokens = Some(budget.max_tokens.map_or(max, |existing| existing.min(max)));
            Some(budget)
        }
        (None, Some(max)) => Some(Budget {
            max_tokens: Some(max),
            ..Budget::default()
        }),
        (budget, None) => budget,
    }
}

/// Laufzeit-Flags, die ein [`EffectiveRights`] verengen können
/// (`--deny-tool`, `--no-network`, `--read-only`, `--full-access`,
/// `--max-tokens`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RightsFlags {
    /// Werkzeugnamen, die zusätzlich zum Manifest verboten werden.
    pub deny_tools: Vec<String>,
    /// Netz vollständig abschalten, unabhängig vom Manifest.
    pub no_network: bool,
    /// Schreiben und Shell abschalten, unabhängig vom Manifest.
    pub read_only: bool,
    /// Werkzeugaufrufe innerhalb des Manifests automatisch freigeben.
    pub full_access: bool,
    /// Zusätzliche Obergrenze der Modell-Tokens.
    pub max_tokens: Option<u64>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use harw_agent_artifact::bundle::{AgentInput, BundleBuilder, ChildLink};
    use harw_agent_dsl::diagnostics::{Diagnostics, SourceFile};
    use harw_agent_dsl::ids::DefinitionId;
    use harw_agent_dsl::layers::DefinitionLayer;
    use harw_agent_dsl::lower_v2::{LowerSources, compile_agent};
    use time::OffsetDateTime;

    type TestResult<T = ()> = std::result::Result<T, Box<dyn std::error::Error>>;

    const ROOT_DEF: &str = r#"
schema = "harwness.agent/v1"
id = "acme.agent.embedded-root@1"
version = "1.0.0"
role = "worker"
specialization = "embedded-root"

[tools]
admitted = ["fs.read", "fs.write", "shell.exec", "web.fetch"]

[network]
hosts = ["example.com"]

[spawn.budget]
max_tokens = 10000
"#;

    const CHILD_DEF: &str = r#"
schema = "harwness.agent/v1"
id = "acme.agent.embedded-child@1"
version = "1.0.0"
role = "worker"
specialization = "embedded-child"

[tools]
admitted = ["fs.read"]
"#;

    fn compile(source: &str, target: &str) -> TestResult<AgentIr> {
        let files = vec![SourceFile::new(
            DefinitionLayer::UserGlobal,
            "agents/embedded/definition.toml",
            source,
        )];
        let sources = LowerSources::new(&files);
        let target = DefinitionId::parse(target)?;
        compile_agent(&target, &sources, OffsetDateTime::UNIX_EPOCH)
            .map_err(|diagnostics: Diagnostics| -> Box<dyn std::error::Error> {
                diagnostics.to_string().into()
            })
    }

    fn build_bundle() -> TestResult<(Bundle, Artifact)> {
        let root_ir = compile(ROOT_DEF, "acme.agent.embedded-root@1")?;
        let child_ir = compile(CHILD_DEF, "acme.agent.embedded-child@1")?;
        let root = AgentInput {
            id: "root".to_owned(),
            name: "root".to_owned(),
            ir: serde_json::to_value(&root_ir)?,
            files: vec![(
                "skill".to_owned(),
                "skills/review/instructions.md".to_owned(),
                b"# review skill".to_vec(),
            )],
            children: vec![ChildLink {
                name: "child".to_owned(),
                id: "child".to_owned(),
                via: "delegation".to_owned(),
            }],
        };
        let child = AgentInput {
            id: "child".to_owned(),
            name: "child".to_owned(),
            ir: serde_json::to_value(&child_ir)?,
            files: vec![(
                "skill".to_owned(),
                "skills/review/instructions.md".to_owned(),
                b"# review skill".to_vec(),
            )],
            children: vec![],
        };
        let artifact = BundleBuilder::new(root).add_agent(child).build()?;
        let bundle = Bundle::from_artifact(&artifact)?;
        Ok((bundle, artifact))
    }

    #[test]
    fn from_bundle_roundtrips_root_and_child_ir_and_payloads() -> TestResult {
        let (bundle, artifact) = build_bundle()?;
        let agent = EmbeddedAgent::from_bundle(bundle, &artifact)?;

        assert_eq!(agent.root_id(), "root");
        assert_eq!(agent.root_ir().specialization, "embedded-root");
        assert_eq!(
            agent.agent_ir("child").map(|ir| ir.specialization.as_str()),
            Some("embedded-child")
        );
        assert_eq!(agent.digest(), &artifact.digest());

        // Der geteilte Skill ist über beide Agenten hinweg gleich erreichbar
        // (die Deduplizierung im Pool ändert die logische Sicht nicht).
        assert_eq!(
            agent.payload("root", "skill", "skills/review/instructions.md"),
            Some(&b"# review skill"[..])
        );
        assert_eq!(
            agent.payload("child", "skill", "skills/review/instructions.md"),
            Some(&b"# review skill"[..])
        );
        assert_eq!(agent.payload("root", "skill", "skills/missing.md"), None);
        assert_eq!(agent.payload("no-such-agent", "skill", "x"), None);
        Ok(())
    }

    #[test]
    fn from_bundle_rejects_a_tampered_snapshot() -> TestResult {
        let root_ir = compile(ROOT_DEF, "acme.agent.embedded-root@1")?;
        let mut tampered = serde_json::to_value(&root_ir)?;
        tampered["specialization"] = serde_json::Value::String("tampered".to_owned());
        let root = AgentInput {
            id: "root".to_owned(),
            name: "root".to_owned(),
            ir: tampered,
            files: vec![],
            children: vec![],
        };
        let artifact = BundleBuilder::new(root).build()?;
        let bundle = Bundle::from_artifact(&artifact)?;
        let result = EmbeddedAgent::from_bundle(bundle, &artifact);
        assert!(matches!(result, Err(RuntimeError::Config { .. })), "{result:?}");
        Ok(())
    }

    #[test]
    fn from_manifest_carries_the_manifest_exactly() -> TestResult {
        let root_ir = compile(ROOT_DEF, "acme.agent.embedded-root@1")?;
        let rights = EffectiveRights::from_manifest(&root_ir.permissions);
        assert!(rights.tools.contains("fs.write"));
        assert!(rights.tools.contains("shell.exec"));
        assert!(rights.write);
        assert!(rights.shell);
        assert!(rights.network_open);
        assert!(rights.network_hosts.contains("example.com"));
        assert!(!rights.full_access);
        assert_eq!(
            rights.budget.as_ref().and_then(|budget| budget.max_tokens),
            Some(10000)
        );
        Ok(())
    }

    #[test]
    fn narrowed_by_never_widens_anything() -> TestResult {
        let root_ir = compile(ROOT_DEF, "acme.agent.embedded-root@1")?;
        let base = EffectiveRights::from_manifest(&root_ir.permissions);

        let denied = base.clone().narrowed_by(&RightsFlags {
            deny_tools: vec!["shell.exec".to_owned()],
            ..RightsFlags::default()
        });
        assert!(!denied.tools.contains("shell.exec"));
        assert!(denied.tools.contains("fs.write"), "unrelated tools stay");
        assert!(denied.tools.is_subset(&base.tools));

        let no_net = base.clone().narrowed_by(&RightsFlags {
            no_network: true,
            ..RightsFlags::default()
        });
        assert!(!no_net.network_open);
        assert!(no_net.network_hosts.is_empty());

        let read_only = base.clone().narrowed_by(&RightsFlags {
            read_only: true,
            ..RightsFlags::default()
        });
        assert!(!read_only.write);
        assert!(!read_only.shell);

        let capped = base.clone().narrowed_by(&RightsFlags {
            max_tokens: Some(1),
            ..RightsFlags::default()
        });
        assert_eq!(
            capped.budget.as_ref().and_then(|budget| budget.max_tokens),
            Some(1)
        );

        // `--full-access` innerhalb des Manifests: setzt das Flag, erweitert
        // aber keine der Mengen über das Manifest hinaus.
        let full_access = base.clone().narrowed_by(&RightsFlags {
            full_access: true,
            ..RightsFlags::default()
        });
        assert!(full_access.full_access);
        assert_eq!(full_access.tools, base.tools);
        assert_eq!(full_access.network_hosts, base.network_hosts);
        assert_eq!(full_access.write, base.write);
        assert_eq!(full_access.shell, base.shell);
        assert_eq!(full_access.host, base.host);

        // Kombiniert mit einer echten Verengung bleibt `full_access` niemals
        // eine Hintertür zu mehr Rechten als das (bereits verengte) Manifest.
        let combined = base.narrowed_by(&RightsFlags {
            deny_tools: vec!["shell.exec".to_owned()],
            full_access: true,
            ..RightsFlags::default()
        });
        assert!(!combined.tools.contains("shell.exec"));
        assert!(combined.full_access);
        Ok(())
    }
}
