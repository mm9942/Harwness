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
                let bytes =
                    bundle
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
                detail: format!("bundle root '{}' has no agent entry", bundle.header.root),
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

    /// Alle Agenten-IDs der Delegationshülle (Wurzel eingeschlossen),
    /// aufsteigend sortiert.
    pub fn agent_ids(&self) -> impl Iterator<Item = &str> + '_ {
        self.agent_irs.keys().map(String::as_str)
    }

    /// Jede `skill`-Payload der ganzen Delegationshülle als
    /// `(logischer Pfad, Inhalt)` — bereit für
    /// [`harw_catalog::SkillIndex::build_with_bundle`] als eingebettetes
    /// Bündel. Der erste Agent, der einen gegebenen logischen Pfad
    /// deklariert, gewinnt (Skills sind ohnehin über den Blob-Pool
    /// dedupliziert, siehe Moduldoku von `harw_agent_artifact::bundle`). Eine
    /// Payload, deren Bytes kein gültiges UTF-8 sind, wird übersprungen
    /// (Skills sind immer Text).
    #[must_use]
    pub fn skill_bundle(&self) -> Vec<(String, String)> {
        let mut seen = BTreeSet::new();
        let mut out = Vec::new();
        for entry in self.bundle.agents.values() {
            for reference in &entry.payload_refs {
                if reference.kind != "skill" || !seen.insert(reference.logical_path.clone()) {
                    continue;
                }
                let Some(bytes) = self.blobs.get(&reference.blake3) else {
                    continue;
                };
                let Ok(text) = std::str::from_utf8(bytes) else {
                    continue;
                };
                out.push((reference.logical_path.clone(), text.to_owned()));
            }
        }
        out
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

    /// Die aktuell geltenden Rechte (Manifest, ggf. verengt über
    /// [`Self::with_rights`]).
    ///
    /// Direkt nach [`Self::from_bundle`] ist das die Manifest-Obergrenze,
    /// also `full_access = true` (siehe [`EffectiveRights`]); effektiv wird
    /// es erst durch `with_rights(from_manifest(..).narrowed_by(&flags))`.
    #[must_use]
    pub fn rights(&self) -> &EffectiveRights {
        &self.rights
    }

    /// Verengt die geltenden Rechte; auch direkte SDK-Aufrufer können das
    /// Manifest oder eine zuvor angewendete Einschränkung nicht erweitern.
    #[must_use]
    pub fn with_rights(mut self, rights: EffectiveRights) -> Self {
        self.rights = self.rights.intersect(&rights);
        self
    }

    /// Selects a bundled agent as the root while retaining the parent's
    /// current permission ceiling.
    ///
    /// # Errors
    /// Returns a configuration error when the agent is not bundled.
    pub fn for_agent(&self, id: &str) -> RuntimeResult<Self> {
        let ir = self.agent_ir(id).ok_or_else(|| RuntimeError::Config {
            detail: format!("agent '{id}' is not present in the compiled bundle"),
        })?;
        let mut selected = self.clone();
        selected.bundle.header.root = id.to_owned();
        selected.rights = EffectiveRights::from_manifest(&ir.permissions).intersect(&self.rights);
        Ok(selected)
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
/// `EffectiveRights::from_manifest` liest ausschließlich das Manifest und
/// liefert die **Obergrenze** (Ceiling) eines Laufs;
/// [`Self::narrowed_by`] wendet Laufzeit-Flags an und kann Rechte nur
/// **wegnehmen**, nie hinzufügen — auch `--full-access`
/// ([`RightsFlags::full_access`]) erweitert keine der Mengen unten, es
/// ändert nur, ob ein Aufruf innerhalb dieser Mengen automatisch freigegeben
/// wird.
///
/// # Monotonie-Regel
/// Jede Kombination zweier Rechte ist streng monoton: [`Self::intersect`]
/// schneidet jede Menge, verUNDet **jeden** Wahrheitswert (auch
/// [`Self::full_access`]) und nimmt pro Budgetgrenze die striktere;
/// [`Self::narrowed_by`] verUNDet `full_access` mit dem Flag und setzt alle
/// übrigen Wahrheitswerte höchstens auf `false`. Kein Schritt kann also
/// einen `false`-Wert wieder auf `true` setzen — auch nicht
/// [`EmbeddedAgent::with_rights`] oder [`EmbeddedAgent::for_agent`].
///
/// # `full_access`: Obergrenze und Flag
/// Das Manifest ([`Permissions`]) hat bewusst keinen Schlüssel für
/// automatische Freigabe: sie gewährt keine Fähigkeit, sondern überspringt
/// nur Rückfragen innerhalb dessen, was das Manifest ohnehin admittiert.
/// Die Manifest-Obergrenze ist daher immer `full_access = true` („darf
/// eingeschaltet werden“); effektiv wird sie erst durch
/// `narrowed_by(&RightsFlags { full_access: true, .. })` — ohne
/// `--full-access` ergibt `narrowed_by` `false`. Effektiv gilt also
/// `full_access = Obergrenze ∧ Flag`, und jede spätere Verengung (z. B.
/// eine Elternbeschränkung mit `full_access = false`) schaltet es
/// endgültig ab.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EffectiveRights {
    /// Effektive Werkzeugnamen (`Permissions::tools`: admittiert minus
    /// verboten).
    pub tools: BTreeSet<String>,
    /// Erreichbare Hosts (`Permissions::network::hosts`); bedeutungslos ohne
    /// [`Self::network_open`].
    pub network_hosts: BTreeSet<String>,
    /// Ob Netzzugriff überhaupt zugelassen ist (`NetworkMode::Allowlist`).
    ///
    /// Trotz des Namens **nicht** „uneingeschränkt“: auch mit `true` ist
    /// nur [`Self::network_hosts`] erreichbar (die Sandbox baut ihren
    /// `NetworkScope` ausschließlich aus dieser Liste); `false` sperrt das
    /// Netz ganz. Der Name bleibt aus Kompatibilität mit dem Kindprotokoll
    /// (`ChildRights::network_open`) bestehen.
    pub network_open: bool,
    /// Ob ein schreibendes Workspace-Werkzeug admittiert ist.
    pub write: bool,
    /// Ob ein Shell-/Prozessstart-Werkzeug admittiert ist.
    pub shell: bool,
    /// Ob ein Host-Werkzeug (außerhalb der Sandbox) admittiert ist.
    pub host: bool,
    /// Ob Werkzeugaufrufe innerhalb der obigen Mengen automatisch freigegeben
    /// werden (`--full-access`). Erweitert nie eine der Mengen selbst. In
    /// [`Self::from_manifest`] die Obergrenze (`true`), effektiv erst nach
    /// [`Self::narrowed_by`] (Obergrenze ∧ Flag); siehe Typdoku.
    pub full_access: bool,
    /// Das Ressourcenbudget des Manifests, falls vorhanden.
    pub budget: Option<Budget>,
}

impl EffectiveRights {
    /// Intersects capabilities and every resource limit.
    ///
    /// Strictly monotonic: every set is intersected and **every** boolean is
    /// AND'd, `full_access` included, so no argument can switch a `false`
    /// right (or automatic approval) back on. Budget limits take the
    /// stricter value per field.
    #[must_use]
    pub fn intersect(mut self, ceiling: &Self) -> Self {
        self.tools.retain(|tool| ceiling.tools.contains(tool));
        self.network_hosts
            .retain(|host| ceiling.network_hosts.contains(host));
        self.network_open &= ceiling.network_open;
        self.write &= ceiling.write;
        self.shell &= ceiling.shell;
        self.host &= ceiling.host;
        self.full_access &= ceiling.full_access;
        self.budget = match (self.budget, ceiling.budget.as_ref()) {
            (Some(mut budget), Some(cap)) => {
                budget.max_tokens = intersect_limit(budget.max_tokens, cap.max_tokens);
                budget.max_tool_calls = intersect_limit(budget.max_tool_calls, cap.max_tool_calls);
                budget.max_wall_secs = intersect_limit(budget.max_wall_secs, cap.max_wall_secs);
                budget.effort_cap = match (budget.effort_cap, cap.effort_cap) {
                    (Some(a), Some(b)) => Some(if (a as u8) <= (b as u8) { a } else { b }),
                    (a, b) => a.or(b),
                };
                Some(budget)
            }
            (budget, cap) => budget.or_else(|| cap.cloned()),
        };
        self
    }

    /// Leitet die unverengten Rechte (die Obergrenze) eines Manifests ab.
    ///
    /// `full_access` ist hier `true`: das Manifest verbietet automatische
    /// Freigabe nie (es kennt keinen Schlüssel dafür), effektiv wird sie
    /// erst durch [`Self::narrowed_by`] mit gesetztem
    /// [`RightsFlags::full_access`]. Ein Aufrufer, der effektive Rechte
    /// braucht, wendet deshalb immer `narrowed_by` an (auch mit
    /// `RightsFlags::default()`).
    #[must_use]
    pub fn from_manifest(permissions: &Permissions) -> Self {
        Self {
            tools: permissions.tools.iter().cloned().collect(),
            network_hosts: permissions.network.hosts.iter().cloned().collect(),
            network_open: permissions.network.mode == NetworkMode::Allowlist,
            write: permissions.filesystem.write,
            shell: permissions.shell,
            host: permissions.host,
            full_access: true,
            budget: permissions.budget.clone(),
        }
    }

    /// Wendet `flags` an. Verengt ausschließlich: jede Menge wird höchstens
    /// kleiner, nie größer, und kein Wahrheitswert wechselt von `false` auf
    /// `true`, unabhängig davon, was `flags` verlangt. `full_access` wird mit
    /// [`RightsFlags::full_access`] verUNDet (siehe Typdoku).
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
        // zulässt. Auch hier nur verUNDet: effektiv ist es genau dann, wenn
        // Obergrenze und Flag es erlauben; ohne Flag bleibt es aus.
        self.full_access &= flags.full_access;
        self.budget = narrow_budget(self.budget, flags.max_tokens);
        self
    }
}

fn intersect_limit<T: Ord>(a: Option<T>, b: Option<T>) -> Option<T> {
    match (a, b) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, b) => a.or(b),
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
    /// Werkzeugaufrufe innerhalb des Manifests automatisch freigeben; wirkt
    /// nur, solange die Obergrenze ([`EffectiveRights::full_access`]) es
    /// erlaubt.
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
        compile_agent(&target, &sources, OffsetDateTime::UNIX_EPOCH).map_err(
            |diagnostics: Diagnostics| -> Box<dyn std::error::Error> {
                diagnostics.to_string().into()
            },
        )
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
    fn selected_child_keeps_its_identity_and_parent_restrictions() -> TestResult {
        let (bundle, artifact) = build_bundle()?;
        let parent = EmbeddedAgent::from_bundle(bundle, &artifact)?.with_rights(
            EffectiveRights::from_manifest(
                &compile(ROOT_DEF, "acme.agent.embedded-root@1")?.permissions,
            )
            .narrowed_by(&RightsFlags {
                deny_tools: vec!["fs.read".to_owned()],
                max_tokens: Some(7),
                ..RightsFlags::default()
            }),
        );
        let child = parent.for_agent("child")?;
        assert_eq!(child.root_id(), "child");
        assert_eq!(child.root_ir().specialization, "embedded-child");
        assert!(child.rights().tools.is_empty());
        assert!(!child.rights().write);
        assert_eq!(
            child.rights().budget.as_ref().and_then(|b| b.max_tokens),
            Some(7)
        );
        assert!(parent.for_agent("missing").is_err());
        Ok(())
    }

    #[test]
    fn replacing_rights_cannot_restore_denied_capabilities_or_budgets() -> TestResult {
        let (bundle, artifact) = build_bundle()?;
        let agent = EmbeddedAgent::from_bundle(bundle, &artifact)?;
        let original = agent.rights().clone();
        let restricted = original.clone().narrowed_by(&RightsFlags {
            deny_tools: vec!["fs.write".to_owned()],
            no_network: true,
            read_only: true,
            max_tokens: Some(3),
            ..RightsFlags::default()
        });
        let agent = agent.with_rights(restricted).with_rights(original);
        assert!(!agent.rights().tools.contains("fs.write"));
        assert!(!agent.rights().network_open);
        assert!(agent.rights().network_hosts.is_empty());
        assert!(!agent.rights().write);
        assert!(!agent.rights().shell);
        assert_eq!(
            agent.rights().budget.as_ref().and_then(|b| b.max_tokens),
            Some(3)
        );
        Ok(())
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

        let mut ids: Vec<&str> = agent.agent_ids().collect();
        ids.sort_unstable();
        assert_eq!(ids, ["child", "root"]);

        let bundle = agent.skill_bundle();
        assert_eq!(bundle.len(), 1, "the shared skill is deduplicated");
        assert_eq!(
            bundle[0],
            (
                "skills/review/instructions.md".to_owned(),
                "# review skill".to_owned()
            )
        );
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
        assert!(
            matches!(result, Err(RuntimeError::Config { .. })),
            "{result:?}"
        );
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
        // Die Manifest-Obergrenze erlaubt automatische Freigabe; effektiv
        // wird sie erst über `narrowed_by` mit dem Flag.
        assert!(rights.full_access);
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

    /// Rechte, deren fünf Wahrheitswerte aus den unteren Bits von `bits`
    /// stammen (Bit 0 `network_open`, 1 `write`, 2 `shell`, 3 `host`, 4
    /// `full_access`).
    fn rights_from_bits(bits: u8) -> EffectiveRights {
        EffectiveRights {
            tools: ["fs.read", "fs.write"]
                .iter()
                .map(|tool| (*tool).to_owned())
                .collect(),
            network_hosts: std::iter::once("example.com".to_owned()).collect(),
            network_open: (bits & 1) != 0,
            write: (bits & 2) != 0,
            shell: (bits & 4) != 0,
            host: (bits & 8) != 0,
            full_access: (bits & 16) != 0,
            budget: None,
        }
    }

    fn booleans(rights: &EffectiveRights) -> [bool; 5] {
        [
            rights.network_open,
            rights.write,
            rights.shell,
            rights.host,
            rights.full_access,
        ]
    }

    #[test]
    fn intersect_never_turns_a_false_boolean_true() {
        // Erschöpfend: alle 2^5 × 2^5 Kombinationen beider Seiten.
        for left in 0..32_u8 {
            for right in 0..32_u8 {
                let a = rights_from_bits(left);
                let b = rights_from_bits(right);
                let result = booleans(&a.clone().intersect(&b));
                let expected = booleans(&a)
                    .into_iter()
                    .zip(booleans(&b))
                    .map(|(x, y)| x && y)
                    .collect::<Vec<_>>();
                assert_eq!(result.to_vec(), expected, "{left:05b} ∩ {right:05b}");
            }
        }
    }

    #[test]
    fn intersect_intersects_sets_and_takes_the_stricter_budget() {
        let mut left = rights_from_bits(31);
        left.tools.insert("shell.exec".to_owned());
        left.network_hosts.insert("left.example".to_owned());
        left.budget = Some(Budget {
            max_tokens: Some(10),
            max_tool_calls: Some(50),
            max_wall_secs: None,
            ..Budget::default()
        });
        let mut right = rights_from_bits(31);
        right.tools.remove("fs.write");
        right.network_hosts.insert("right.example".to_owned());
        right.budget = Some(Budget {
            max_tokens: Some(20),
            max_tool_calls: Some(5),
            max_wall_secs: Some(9),
            ..Budget::default()
        });
        let result = left.clone().intersect(&right);
        assert_eq!(
            result.tools,
            std::iter::once("fs.read".to_owned()).collect::<BTreeSet<_>>()
        );
        assert_eq!(
            result.network_hosts,
            std::iter::once("example.com".to_owned()).collect::<BTreeSet<_>>()
        );
        let budget = result.budget.clone().unwrap_or_default();
        assert_eq!(budget.max_tokens, Some(10));
        assert_eq!(budget.max_tool_calls, Some(5));
        assert_eq!(budget.max_wall_secs, Some(9));
        // Symmetrisch: die Reihenfolge ändert nichts an der Schnittmenge.
        assert_eq!(right.intersect(&left), result);
    }

    #[test]
    fn narrowed_by_never_widens_any_set_or_boolean() {
        let deny_variants: [Vec<String>; 3] = [
            Vec::new(),
            vec!["fs.write".to_owned()],
            vec!["not.admitted".to_owned()],
        ];
        for base_bits in 0..32_u8 {
            let mut base = rights_from_bits(base_bits);
            base.budget = Some(Budget {
                max_tokens: Some(100),
                ..Budget::default()
            });
            for flag_bits in 0..8_u8 {
                for deny_tools in &deny_variants {
                    for max_tokens in [None, Some(1), Some(1_000)] {
                        let flags = RightsFlags {
                            deny_tools: deny_tools.clone(),
                            no_network: (flag_bits & 1) != 0,
                            read_only: (flag_bits & 2) != 0,
                            full_access: (flag_bits & 4) != 0,
                            max_tokens,
                        };
                        let narrowed = base.clone().narrowed_by(&flags);
                        assert!(narrowed.tools.is_subset(&base.tools));
                        assert!(narrowed.network_hosts.is_subset(&base.network_hosts));
                        for (after, before) in booleans(&narrowed).into_iter().zip(booleans(&base))
                        {
                            assert!(!after || before, "{flags:?} widened {base_bits:05b}");
                        }
                        let after = narrowed.budget.and_then(|budget| budget.max_tokens);
                        assert!(after.is_some_and(|tokens| tokens <= 100), "{after:?}");
                    }
                }
            }
        }
    }

    #[test]
    fn full_access_is_effective_only_with_ceiling_and_flag() {
        for ceiling in [false, true] {
            for flag in [false, true] {
                let mut base = rights_from_bits(0);
                base.full_access = ceiling;
                let narrowed = base.narrowed_by(&RightsFlags {
                    full_access: flag,
                    ..RightsFlags::default()
                });
                assert_eq!(narrowed.full_access, ceiling && flag, "{ceiling} ∧ {flag}");
            }
        }
    }

    #[test]
    fn from_manifest_without_the_flag_has_no_full_access() -> TestResult {
        let root_ir = compile(ROOT_DEF, "acme.agent.embedded-root@1")?;
        let ceiling = EffectiveRights::from_manifest(&root_ir.permissions);
        assert!(ceiling.full_access, "the manifest ceiling permits it");
        let effective = ceiling.clone().narrowed_by(&RightsFlags::default());
        assert!(!effective.full_access);

        // Über `with_rights` bleibt es aus, auch wenn danach erneut die
        // unverengte Obergrenze übergeben wird.
        let (bundle, artifact) = build_bundle()?;
        let agent = EmbeddedAgent::from_bundle(bundle, &artifact)?
            .with_rights(effective)
            .with_rights(ceiling);
        assert!(!agent.rights().full_access);
        assert!(!agent.for_agent("child")?.rights().full_access);
        Ok(())
    }
}
