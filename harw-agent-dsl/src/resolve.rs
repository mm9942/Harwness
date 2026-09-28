//! Schichten-Resolver für Harwness Agent Definitions (§17 DSL-Spec).
//!
//! Dieses Modul implementiert [`resolve_definition`], den Kern des Compiler-Pipelines:
//! Schichten sortieren → Basis laden → Mixins anwenden → Patches anwenden →
//! Authority-Monotonie prüfen → Trace erzeugen.
//!
//! # Schlüsseltypen
//! Keine eigenen — nutzt Typen aus `ids`, `layers`, `raw`, `resolved`, `authority`, `error`.
//!
//! # Invarianten (§17, §22)
//! - Basis muss in den Schichten auffindbar sein, sonst `DslError::MissingBase`.
//! - Mixins müssen auffindbar sein, sonst `DslError::MissingMixin`.
//! - Authority-Patches dürfen nur reduzieren, sonst `DslError::AuthorityElevation`.
//! - Eine höhere Schicht (Overlay) darf die Authority-Obergrenze der niedrigeren
//!   Schichten nicht erweitern — auch nicht über ein eigenes `extends`, das die
//!   Basiskette erneut anwendet. Sonst `DslError::AuthorityElevation`.
//! - Mixins dürfen die Rolle nicht ändern.
//!
//! # Nebenläufigkeit
//! `resolve_definition` ist zustandslos und thread-sicher.

use time::OffsetDateTime;

use crate::authority::AuthorityCeiling;
use crate::error::{DiagLocation, DslError, DslResult};
use crate::ids::DefinitionId;
use crate::layers::DefinitionLayer;
use crate::merge::{MergeOp, PATCH_OPERATOR_KEYS, apply_merge_op};
use crate::raw::RawAgentDefinition;
use crate::resolved::{ResolutionStep, ResolutionTrace, ResolvedAgentDefinition};
use crate::skills::{SKILLS_CONFIG_KEY, union_skills, validate_skill_list, validate_skill_name};

/// Schema-Label einer Agentendefinition.
pub const AGENT_SCHEMA: &str = "harwness.agent/v1";

/// Schema-Label eines Mixins.
pub const MIXIN_SCHEMA: &str = "harwness.mixin/v1";

/// Zulässige Schemata für Ziel- und `extends`-Definitionen.
const AGENT_SCHEMAS: &[&str] = &[AGENT_SCHEMA];

/// Zulässige Schemata für Mixins (ein Mixin darf auch als Agent deklariert sein).
const MIXIN_SCHEMAS: &[&str] = &[MIXIN_SCHEMA, AGENT_SCHEMA];

/// Prüft das `schema`-Feld einer Definition.
///
/// # Errors
/// [`DslError::SchemaMismatch`], wenn `definition.schema` nicht in `accepted` liegt.
fn check_schema(
    definition: &RawAgentDefinition,
    accepted: &'static [&'static str],
) -> DslResult<()> {
    if accepted.contains(&definition.schema.as_str()) {
        return Ok(());
    }
    Err(DslError::SchemaMismatch {
        of: Box::new(definition.id.clone()),
        expected: accepted,
        found: definition.schema.clone(),
        location: DiagLocation::field("schema"),
    })
}

/// Löst eine geschichtete Definitions-Menge in eine [`ResolvedAgentDefinition`] auf.
///
/// # Beschreibung
/// Führt die vollständige Compiler-Pipeline für einen Definitions-Schichtstapel aus:
///
/// 1. Sortiert `layers` nach [`DefinitionLayer`] (aufsteigend).
/// 2. Sucht die Ziel-Definition (`target_id`) in den sortierten Schichten.
/// 3. Lädt rekursiv die Basis (`extends`), falls vorhanden.
///    Fehlt sie: `DslError::MissingBase`.
/// 4. Wendet Mixins in der deklarierten Reihenfolge an.
///    Fehlt eines: `DslError::MissingMixin`.
/// 5. Wendet Patches der Ziel-Definition an.
/// 6. Prüft Authority-Monotonie (child.authority ⊆ parent.authority) und je
///    Overlay-Schicht (Ergebnis der Schicht ⊆ Obergrenze der niedrigeren
///    Schichten, auch wenn die Schicht ein eigenes `extends` trägt).
///    Verletzung: `DslError::AuthorityElevation`.
/// 7. Erzeugt den [`ResolutionTrace`].
///
/// # Argumente
/// - `target_id` (`&DefinitionId`): ID der aufzulösenden Definition.
/// - `layers` (`&[(DefinitionLayer, RawAgentDefinition)]`): verfügbare Schichten.
/// - `now` (`OffsetDateTime`): Zeitstempel für Trace-Einträge.
///
/// # Rückgabe
/// `Ok(ResolvedAgentDefinition)` bei erfolgreicher Auflösung.
///
/// # Fehler
/// - [`DslError::MissingBase`]: wenn `extends` auf eine nicht vorhandene Definition zeigt.
/// - [`DslError::MissingMixin`]: wenn ein `mixin` nicht in den Schichten gefunden wird.
/// - [`DslError::AuthorityElevation`]: wenn ein Patch Authority unzulässig erhöht
///   oder eine höhere Schicht (auch über ihr `extends`) die Obergrenze der
///   niedrigeren Schichten erweitert.
/// - [`DslError::SchemaMismatch`]: wenn eine Ziel- oder Basisdefinition nicht
///   `harwness.agent/v1` bzw. ein Mixin weder `harwness.mixin/v1` noch
///   `harwness.agent/v1` deklariert.
/// - `DslError::Patch*`: wenn ein Patch-Pfad oder -Operator ungültig ist (§7).
///
/// # Nebenläufigkeit
/// Zustandslos; thread-sicher.
///
/// # Beispiele
/// ```rust
/// use harw_agent_dsl::resolve::resolve_definition;
/// use harw_agent_dsl::ids::DefinitionId;
/// use harw_agent_dsl::layers::DefinitionLayer;
/// use harw_agent_dsl::parse::parse_toml;
///
/// let src = r#"
/// schema = "harwness.agent/v1"
/// id = "harwness.agent.worker@1"
/// version = "1.0.0"
/// role = "worker"
/// specialization = "test"
/// "#;
/// let raw = parse_toml(src).unwrap();
/// let id = DefinitionId::parse("harwness.agent.worker@1").unwrap();
/// let layers = vec![(DefinitionLayer::BuiltIn, raw)];
/// let resolved = resolve_definition(&id, &layers, time::OffsetDateTime::now_utc()).unwrap();
/// assert_eq!(resolved.specialization, "test");
/// ```
pub fn resolve_definition(
    target_id: &DefinitionId,
    layers: &[(DefinitionLayer, RawAgentDefinition)],
    now: OffsetDateTime,
) -> DslResult<ResolvedAgentDefinition> {
    // 1. Schichten nach Priorität sortieren (BuiltIn=0 … RunLocal=5)
    let mut sorted: Vec<&(DefinitionLayer, RawAgentDefinition)> = layers.iter().collect();
    sorted.sort_by_key(|(layer, _)| *layer);

    // 2. Alle Zieldefinitionen von der niedrigsten zur höchsten Schicht sammeln.
    let target_layers: Vec<&RawAgentDefinition> = sorted
        .iter()
        .filter(|(_, definition)| &definition.id == target_id)
        .map(|(_, definition)| definition)
        .collect();
    let base_target = target_layers
        .first()
        .copied()
        .ok_or_else(|| DslError::MissingBase {
            of: Box::new(target_id.clone()),
            referenced: Box::new(crate::ids::DefinitionRef {
                id: target_id.clone(),
                version: None,
            }),
            location: DiagLocation::none(),
        })?;
    // `target_layers` wurde oben bereits als nicht-leer bestätigt (siehe
    // `base_target`); dieselbe Fehlervariante dient hier nur als defensiver
    // Fallback statt einer Panik (Bible R087).
    let target = target_layers
        .last()
        .copied()
        .ok_or_else(|| DslError::MissingBase {
            of: Box::new(target_id.clone()),
            referenced: Box::new(crate::ids::DefinitionRef {
                id: target_id.clone(),
                version: None,
            }),
            location: DiagLocation::none(),
        })?;
    let authoritative_role = base_target.role;
    for definition in &target_layers {
        check_schema(definition, AGENT_SCHEMAS)?;
    }

    let mut ctx = ResolveCtx {
        target_id,
        sorted: &sorted,
        now,
        steps: Vec::new(),
        config: toml::Table::new(),
        authority: AuthorityCeiling::default(),
        reasoning_effort: None,
        skills: Vec::new(),
    };
    // 4–5. Ziel-Layer aufsteigend komponieren. Höhere Layer sind Overlays.
    // Ein Overlay mit eigenem `extends` wendet seine Basiskette erneut an,
    // deren Wurzel die Obergrenze ungeprüft ersetzt. Darum wird das Ergebnis
    // jeder Overlay-Schicht gegen die Obergrenze davor geprüft (6.).
    for (layer_index, definition) in target_layers.iter().enumerate() {
        let ceiling_below = (layer_index > 0).then(|| ctx.authority.clone());
        let mut active_extends = vec![(definition.id.clone(), definition.version.clone())];
        resolve_extends(definition, &mut ctx, &mut active_extends)?;
        apply_mixins(definition, authoritative_role, &mut ctx)?;
        apply_definition_content(
            definition,
            &mut ctx,
            layer_index > 0 || base_target.extends.is_some(),
            layer_index > 0,
        )?;
        if let Some(ceiling_below) = ceiling_below {
            check_overlay_ceiling(definition, &ctx.authority, &ceiling_below)?;
        }
    }

    // Skills liegen unter einem reservierten Config-Schlüssel (siehe
    // `crate::skills`); nur eingetragen, wenn überhaupt eine Ebene Skills
    // führt — Definitionen ohne Skills behalten eine unveränderte Config.
    let mut config = ctx.config;
    if !ctx.skills.is_empty() {
        config.insert(
            SKILLS_CONFIG_KEY.to_owned(),
            toml::Value::Array(ctx.skills.into_iter().map(toml::Value::String).collect()),
        );
    }

    Ok(ResolvedAgentDefinition {
        id: target_id.clone(),
        version: target.version.clone(),
        role: authoritative_role,
        specialization: target.specialization.clone(),
        name: target.name.clone(),
        description: target.description.clone(),
        authority: ctx.authority,
        reasoning_effort: ctx.reasoning_effort,
        trace: ResolutionTrace { steps: ctx.steps },
        config,
    })
}

/// Gebündelter, veränderlicher Auflösungskontext für die interne Compiler-Pipeline.
///
/// Bündelt die über die rekursive Extends-/Mixin-/Patch-Auflösung hinweg
/// gemeinsam durchgereichten Daten (Ziel-ID, sortierte Schichten, Zeitstempel,
/// Trace-Schritte, akkumulierte Config und Authority-Ceiling) in einem Struct,
/// um `clippy::too_many_arguments` an den internen Hilfsfunktionen
/// [`resolve_extends`], [`apply_mixins`] und [`apply_definition_content`] zu
/// vermeiden. Rein crate-intern (nicht `pub`) — keine Änderung der
/// öffentlichen API von `resolve_definition`.
struct ResolveCtx<'a> {
    /// ID der aufzulösenden Ziel-Definition.
    target_id: &'a DefinitionId,
    /// Alle Schichten, aufsteigend nach [`DefinitionLayer`] sortiert.
    sorted: &'a [&'a (DefinitionLayer, RawAgentDefinition)],
    /// Zeitstempel für neu erzeugte Trace-Einträge.
    now: OffsetDateTime,
    /// Bisher erzeugte Auflösungsschritte (Trace).
    steps: Vec<ResolutionStep>,
    /// Akkumulierte, zusammengeführte Konfigurationstabelle.
    config: toml::Table,
    /// Aktuell gültige Authority-Obergrenze.
    authority: AuthorityCeiling,
    /// Aktuell gültiger Standard-Reasoning-Effort. Wird von jeder Ebene
    /// (Basis vor Mixin vor eigenem Inhalt, ältere Vorfahren vor jüngeren,
    /// niedrigere Schicht vor höherer) überschrieben, sofern diese Ebene
    /// `reasoning_effort` selbst setzt (`Some`); eine Ebene ohne Aussage
    /// (`None`) lässt den zuvor akkumulierten Wert unverändert. So gewinnt
    /// stets die spezifischste Definition, die tatsächlich eine Aussage
    /// trifft — analog zur Vererbungsregel von `BudgetSpec::effort_cap`.
    reasoning_effort: Option<String>,
    /// Akkumulierte Skill-Liste: Vereinigung mit Duplikat-Entfernung in
    /// Anwendungsreihenfolge (Basis → Mixins → eigene Liste → höhere
    /// Schicht); nur `[patch.skills]` kann geerbte Einträge entfernen.
    skills: Vec<String>,
}

/// Wendet die "spezifischere Definition überschreibt"-Regel für
/// `reasoning_effort` an: setzt `ctx.reasoning_effort` nur, wenn `definition`
/// selbst eine Aussage trifft (`Some`); lässt den akkumulierten Wert bei
/// `None` unverändert, statt ihn zu löschen.
fn apply_reasoning_effort_override(ctx: &mut ResolveCtx<'_>, definition: &RawAgentDefinition) {
    if let Some(effort) = &definition.reasoning_effort {
        ctx.reasoning_effort = Some(effort.clone());
    }
}

/// Prüft, dass eine Overlay-Schicht die Obergrenze der niedrigeren Schichten
/// nicht erweitert.
///
/// # Beschreibung
/// Ohne `extends` fängt schon [`apply_definition_content`] jede Erweiterung
/// ab. Mit `extends` wendet [`resolve_extends`] die Basiskette erneut an, und
/// deren Wurzel ersetzt die Obergrenze ungeprüft — eine Verengung aus einer
/// niedrigeren Schicht ginge verloren, und das eigene `[authority]` des
/// Overlays würde nur gegen die zurückgesetzte Obergrenze geprüft. Darum wird
/// hier das Ergebnis der ganzen Schicht mit `ceiling_below` verglichen.
///
/// # Errors
/// [`DslError::AuthorityElevation`], wenn `resolved` Capabilities trägt, die
/// `ceiling_below` nicht hat. Feldpfad ist `authority.capabilities`, wenn das
/// Overlay selbst `[authority]` setzt, sonst `extends`.
fn check_overlay_ceiling(
    definition: &RawAgentDefinition,
    resolved: &AuthorityCeiling,
    ceiling_below: &AuthorityCeiling,
) -> DslResult<()> {
    let added = resolved.added_relative_to(ceiling_below);
    if added.is_empty() {
        return Ok(());
    }
    let field = if definition.extends.is_some()
        && extract_authority_from_table(&definition.tables).is_none()
    {
        "extends"
    } else {
        "authority.capabilities"
    };
    Err(DslError::AuthorityElevation {
        of: Box::new(definition.id.clone()),
        added_capabilities: added,
        location: DiagLocation::field(field),
    })
}

/// Resolves every ancestor of `definition`, from its root base to its direct base.
///
/// Die Wurzel der Kette setzt die Obergrenze ungeprüft; für Overlay-Schichten
/// prüft [`check_overlay_ceiling`] das Ergebnis gegen die Obergrenze davor.
///
/// Nimmt den geteilten Pipeline-Zustand gebündelt über `ctx` entgegen
/// (siehe [`ResolveCtx`]), um `clippy::too_many_arguments` zu vermeiden.
fn resolve_extends(
    definition: &RawAgentDefinition,
    ctx: &mut ResolveCtx<'_>,
    active_extends: &mut Vec<(DefinitionId, crate::ids::Version)>,
) -> DslResult<()> {
    let Some(base_ref) = &definition.extends else {
        return Ok(());
    };

    let base = find_reference(ctx.sorted, base_ref).ok_or_else(|| DslError::MissingBase {
        of: Box::new(ctx.target_id.clone()),
        referenced: Box::new(base_ref.clone()),
        location: DiagLocation::field("extends"),
    })?;

    if let Some(cycle_start) = active_extends
        .iter()
        .position(|(id, version)| id == &base.id && version == &base.version)
    {
        let mut cycle: Vec<DefinitionId> = active_extends[cycle_start..]
            .iter()
            .map(|(id, _)| id.clone())
            .collect();
        cycle.push(base.id.clone());
        return Err(DslError::InheritanceCycle {
            cycle,
            location: DiagLocation::field("extends"),
        });
    }

    check_schema(base, AGENT_SCHEMAS)?;
    active_extends.push((base.id.clone(), base.version.clone()));
    resolve_extends(base, ctx, active_extends)?;
    active_extends.pop();
    apply_mixins(base, base.role, ctx)?;
    apply_definition_content(base, ctx, base.extends.is_some(), false)?;
    ctx.steps.push(ResolutionStep {
        source: base.id.to_string(),
        kind: "base".to_owned(),
        applied_at: ctx.now,
    });
    Ok(())
}

/// Applies the mixins declared by one definition in declaration order.
///
/// Nimmt den geteilten Pipeline-Zustand gebündelt über `ctx` entgegen
/// (siehe [`ResolveCtx`]).
fn apply_mixins(
    definition: &RawAgentDefinition,
    authoritative_role: crate::roles::AgentRoleId,
    ctx: &mut ResolveCtx<'_>,
) -> DslResult<()> {
    for (mixin_idx, mixin_ref) in definition.mixins.iter().enumerate() {
        let mixin =
            find_reference(ctx.sorted, mixin_ref).ok_or_else(|| DslError::MissingMixin {
                of: Box::new(ctx.target_id.clone()),
                referenced: Box::new(mixin_ref.clone()),
                location: DiagLocation::field(format!("mixins[{mixin_idx}]")),
            })?;
        check_schema(mixin, MIXIN_SCHEMAS)?;
        // Mixins dürfen die Rolle nicht ändern (§6)
        if mixin.role != authoritative_role {
            return Err(DslError::IllegalRoleForMixin {
                mixin: Box::new(mixin_ref.id.clone()),
                role: mixin.role,
                location: DiagLocation::field(format!("mixins[{mixin_idx}].role")),
            });
        }
        merge_tables(&mut ctx.config, &mixin.tables);
        apply_reasoning_effort_override(ctx, mixin);
        validate_skill_list(&mixin.id, &mixin.skills, "skills")?;
        union_skills(&mut ctx.skills, &mixin.skills);
        ctx.steps.push(ResolutionStep {
            source: mixin_ref.id.to_string(),
            kind: "mixin".to_owned(),
            applied_at: ctx.now,
        });
    }
    Ok(())
}

/// Applies the patch and free-form tables belonging to one definition.
///
/// Nimmt den geteilten Pipeline-Zustand gebündelt über `ctx` entgegen
/// (siehe [`ResolveCtx`]).
fn apply_definition_content(
    definition: &RawAgentDefinition,
    ctx: &mut ResolveCtx<'_>,
    has_inherited_authority: bool,
    is_layer_overlay: bool,
) -> DslResult<()> {
    if let Some(definition_authority) = extract_authority_from_table(&definition.tables) {
        if has_inherited_authority {
            let added = definition_authority.added_relative_to(&ctx.authority);
            if !added.is_empty() {
                return Err(DslError::AuthorityElevation {
                    of: Box::new(definition.id.clone()),
                    added_capabilities: added,
                    location: DiagLocation::field("authority.capabilities"),
                });
            }
        }
        ctx.authority = definition_authority;
    }

    // `[patch.skills]` wirkt auf die geerbte Liste, bevor die eigene Liste
    // vereinigt wird — der einzige Weg, geerbte Skills zu entfernen.
    if let Some(skills_patch) = definition.patch.get(SKILLS_CONFIG_KEY) {
        apply_skills_patch(&mut ctx.skills, skills_patch, &definition.id)?;
    }

    let authority_before_patch = ctx.authority.clone();
    apply_patches(
        &mut ctx.config,
        &mut ctx.authority,
        &definition.patch,
        ctx.target_id,
        &authority_before_patch,
    )?;

    if is_layer_overlay {
        merge_overlay_tables(&mut ctx.config, &definition.tables);
    } else {
        merge_tables(&mut ctx.config, &definition.tables);
    }

    // Zuletzt anwenden: die eigene Aussage dieser Definition ist innerhalb
    // ihrer Ebene die spezifischste und überschreibt, was Basis/Mixins zuvor
    // gesetzt haben (siehe [`apply_reasoning_effort_override`]).
    apply_reasoning_effort_override(ctx, definition);

    validate_skill_list(&definition.id, &definition.skills, "skills")?;
    union_skills(&mut ctx.skills, &definition.skills);

    ctx.steps.push(ResolutionStep {
        source: definition.id.to_string(),
        kind: "patch".to_owned(),
        applied_at: ctx.now,
    });
    Ok(())
}

/// Selects a reference target. An explicit full version must match exactly;
/// otherwise the newest version in the highest-priority matching layer is selected.
fn find_reference<'a>(
    sorted: &[&'a (DefinitionLayer, RawAgentDefinition)],
    reference: &crate::ids::DefinitionRef,
) -> Option<&'a RawAgentDefinition> {
    if let Some(version) = &reference.version {
        return sorted
            .iter()
            .rev()
            .find(|(_, definition)| definition.id == reference.id && definition.version == *version)
            .map(|(_, definition)| definition);
    }

    sorted
        .iter()
        .filter(|(_, definition)| definition.id == reference.id)
        .max_by(|(left_layer, left), (right_layer, right)| {
            left_layer
                .cmp(right_layer)
                .then_with(|| left.version.0.cmp(&right.version.0))
        })
        .map(|(_, definition)| definition)
}

/// Merged `source`-Tabelle in `target`: existierende Keys bleiben erhalten,
/// fehlende werden ergänzt. Arrays werden per Append zusammengeführt.
fn merge_tables(target: &mut toml::Table, source: &toml::Table) {
    for (key, value) in source {
        // Interne Steuerfelder überspringen
        if matches!(
            key.as_str(),
            "schema"
                | "id"
                | "version"
                | "extends"
                | "mixins"
                | "role"
                | "specialization"
                | "name"
                | "description"
                | "patch"
        ) {
            continue;
        }
        if !target.contains_key(key) {
            target.insert(key.clone(), value.clone());
        } else if let toml::Value::Array(src_arr) = value {
            // `contains_key` war true, also ist der Entry vorhanden; sicher binden.
            let Some(toml::Value::Array(tgt_arr)) = target.get_mut(key) else {
                continue;
            };
            // Arrays werden zusammengeführt (Append-Semantik für Mixin-Anwendung)
            for item in src_arr {
                if !tgt_arr.contains(item) {
                    tgt_arr.push(item.clone());
                }
            }
        }
    }
}

/// Composes a higher-priority layer over accumulated lower-layer tables.
/// Scalars replace lower values, nested tables merge recursively, and arrays
/// append new values in declaration order. Authority is replaced as a whole so
/// the config view cannot retain capabilities removed by the resolved ceiling.
fn merge_overlay_tables(target: &mut toml::Table, source: &toml::Table) {
    for (key, source_value) in source {
        if key == "authority" {
            target.insert(key.clone(), source_value.clone());
            continue;
        }

        let Some(target_value) = target.get_mut(key) else {
            target.insert(key.clone(), source_value.clone());
            continue;
        };

        match (target_value, source_value) {
            (toml::Value::Table(target_table), toml::Value::Table(source_table)) => {
                merge_overlay_tables(target_table, source_table);
            }
            (toml::Value::Array(target_array), toml::Value::Array(source_array)) => {
                for item in source_array {
                    if !target_array.contains(item) {
                        target_array.push(item.clone());
                    }
                }
            }
            (target_value, source_value) => {
                *target_value = source_value.clone();
            }
        }
    }
}

/// Wendet alle Patch-Operationen an und prüft Authority-Monotonie.
///
/// # Beschreibung
/// Jeder Schlüssel unter `[patch]` ist ein Pfad in die akkumulierte Config.
/// Eine Patch-Tabelle führt entweder Operatoren (`replace`, `append`, …,
/// siehe [`PATCH_OPERATOR_KEYS`]) für genau diesen Pfad oder verschachtelte
/// Schlüssel für tiefere Pfade — `[patch.context.must_include] append = […]`
/// wirkt also auf `context.must_include` (§7). Beides zugleich ist
/// mehrdeutig und ein Fehler. `[patch.skills]` hat einen eigenen Pfad
/// ([`apply_skills_patch`]), `[patch.authority]` ebenfalls
/// ([`apply_authority_patch`]).
fn apply_patches(
    config: &mut toml::Table,
    authority: &mut AuthorityCeiling,
    patch: &toml::Table,
    target_id: &DefinitionId,
    parent_authority: &AuthorityCeiling,
) -> DslResult<()> {
    for (field, patch_value) in patch {
        if field == SKILLS_CONFIG_KEY {
            // Eigener Pfad: `apply_skills_patch` (typisierte Liste statt Config).
            continue;
        }
        if field == "authority" {
            apply_authority_patch(authority, patch_value, target_id, parent_authority)?;
            continue;
        }
        apply_patch_path(config, field, patch_value, field)?;
    }
    Ok(())
}

/// `true`, wenn `key` ein Merge-Operator einer Patch-Tabelle ist.
fn is_operator_key(key: &str) -> bool {
    PATCH_OPERATOR_KEYS.contains(&key)
}

/// Wendet den Patch `patch_value` auf `parent[key]` an; `path` ist der
/// punktierte Pfad relativ zur Config (für Fehlermeldungen).
///
/// # Errors
/// - [`DslError::UnknownMergeOp`]: `patch_value` ist keine Tabelle.
/// - [`DslError::PatchAmbiguous`]: Operatoren und verschachtelte Schlüssel gemischt.
/// - [`DslError::PatchPathMissing`]: der Pfad existiert nicht und der Patch
///   ist kein `replace`.
/// - [`DslError::PatchTypeMismatch`] / [`DslError::PatchExceedsParent`]: aus
///   dem Operator selbst.
fn apply_patch_path(
    parent: &mut toml::Table,
    key: &str,
    patch_value: &toml::Value,
    path: &str,
) -> DslResult<()> {
    let location_path = format!("patch.{path}");
    let Some(patch_table) = patch_value.as_table() else {
        return Err(DslError::UnknownMergeOp {
            name: format!(
                "patch value at '{location_path}' must be a table of operators or nested keys, found {}",
                patch_value.type_str()
            ),
        });
    };
    let has_operators = patch_table.keys().any(|k| is_operator_key(k));
    let has_nested = patch_table.keys().any(|k| !is_operator_key(k));
    if has_operators && has_nested {
        return Err(DslError::PatchAmbiguous {
            path: path.to_owned(),
            location: DiagLocation::field(location_path),
        });
    }
    if has_operators {
        if !parent.contains_key(key) {
            if !patch_table.contains_key("replace") {
                return Err(DslError::PatchPathMissing {
                    path: path.to_owned(),
                    location: DiagLocation::field(location_path),
                });
            }
            parent.insert(key.to_owned(), toml::Value::Table(toml::Table::new()));
        }
        let Some(slot) = parent.get_mut(key) else {
            return Err(DslError::PatchPathMissing {
                path: path.to_owned(),
                location: DiagLocation::field(location_path),
            });
        };
        return try_apply_table_op(slot, patch_table)
            .map_err(|error| error.with_field_path(&location_path));
    }
    if patch_table.is_empty() {
        return Ok(());
    }
    let Some(slot) = parent.get_mut(key) else {
        return Err(DslError::PatchPathMissing {
            path: path.to_owned(),
            location: DiagLocation::field(location_path),
        });
    };
    let found = slot.type_str();
    let Some(slot_table) = slot.as_table_mut() else {
        return Err(DslError::PatchTypeMismatch {
            op: "nested patch path".to_owned(),
            expected: "table",
            found,
            location: DiagLocation::field(location_path),
        });
    };
    for (sub_key, sub_value) in patch_table {
        apply_patch_path(slot_table, sub_key, sub_value, &format!("{path}.{sub_key}"))?;
    }
    Ok(())
}

/// Wendet `[patch.authority]` an: nur `capabilities` mit `intersect` oder
/// `remove` (§7). `append` ist eine Elevation, jeder andere Operator ist
/// verboten.
fn apply_authority_patch(
    authority: &mut AuthorityCeiling,
    patch_value: &toml::Value,
    target_id: &DefinitionId,
    parent_authority: &AuthorityCeiling,
) -> DslResult<()> {
    let Some(patch_table) = patch_value.as_table() else {
        return Err(DslError::PatchTypeMismatch {
            op: "patch".to_owned(),
            expected: "table",
            found: patch_value.type_str(),
            location: DiagLocation::field("patch.authority"),
        });
    };
    for (key, caps_patch) in patch_table {
        if key != "capabilities" {
            return Err(DslError::PatchPathMissing {
                path: format!("authority.{key}"),
                location: DiagLocation::field(format!("patch.authority.{key}")),
            });
        }
        let Some(operators) = caps_patch.as_table() else {
            return Err(DslError::PatchTypeMismatch {
                op: "patch".to_owned(),
                expected: "table",
                found: caps_patch.type_str(),
                location: DiagLocation::field("patch.authority.capabilities"),
            });
        };
        for (op, values) in operators {
            let location = DiagLocation::field(format!("patch.authority.capabilities.{op}"));
            let Some(array) = values.as_array() else {
                if !is_operator_key(op) {
                    return Err(DslError::UnknownMergeOp { name: op.clone() });
                }
                return Err(DslError::PatchTypeMismatch {
                    op: op.clone(),
                    expected: "array",
                    found: values.type_str(),
                    location,
                });
            };
            let caps: Vec<String> = array
                .iter()
                .filter_map(|v| v.as_str().map(str::to_owned))
                .collect();
            match op.as_str() {
                "intersect" => {
                    let intersect_ceiling = AuthorityCeiling { capabilities: caps };
                    *authority = authority.intersect(&intersect_ceiling);
                    // Prüfe, dass das Ergebnis keine neuen Caps gegenüber parent enthält
                    let added = authority.added_relative_to(parent_authority);
                    if !added.is_empty() && !parent_authority.capabilities.is_empty() {
                        return Err(DslError::AuthorityElevation {
                            of: Box::new(target_id.clone()),
                            added_capabilities: added,
                            location: DiagLocation::field("authority.capabilities"),
                        });
                    }
                }
                "remove" => {
                    authority
                        .capabilities
                        .retain(|capability| !caps.contains(capability));
                }
                "append" | "prepend" => {
                    // Hinzufügen zu authority.capabilities ist verboten (§7)
                    return Err(DslError::AuthorityElevation {
                        of: Box::new(target_id.clone()),
                        added_capabilities: caps,
                        location: DiagLocation::field("authority.capabilities"),
                    });
                }
                other if is_operator_key(other) => {
                    return Err(DslError::AuthorityOpForbidden {
                        of: Box::new(target_id.clone()),
                        op: other.to_owned(),
                        location,
                    });
                }
                other => {
                    return Err(DslError::UnknownMergeOp {
                        name: other.to_owned(),
                    });
                }
            }
        }
    }
    Ok(())
}

/// Wendet eine `[patch.skills]`-Operation auf die akkumulierte Skill-Liste an.
///
/// Das Ergebnis wird streng geprüft (nur Strings, gültige Namen) und danach
/// dedupliziert — `append`/`prepend` eines bereits geerbten Namens ist also
/// kein Fehler, sondern ein No-op an der ersten Position.
fn apply_skills_patch(
    skills: &mut Vec<String>,
    patch_value: &toml::Value,
    of: &DefinitionId,
) -> DslResult<()> {
    let Some(patch_table) = patch_value.as_table() else {
        return Err(DslError::PatchTypeMismatch {
            op: "patch".to_owned(),
            expected: "table",
            found: patch_value.type_str(),
            location: DiagLocation::field("patch.skills"),
        });
    };
    let mut current = toml::Value::Array(skills.drain(..).map(toml::Value::String).collect());
    try_apply_table_op(&mut current, patch_table)
        .map_err(|error| error.with_field_path("patch.skills"))?;
    let Some(values) = current.as_array() else {
        return Err(DslError::InvalidSkill {
            of: Box::new(of.clone()),
            skill: current.to_string(),
            reason: "'patch.skills' muss eine Liste von Strings ergeben",
            location: DiagLocation::field("patch.skills"),
        });
    };
    let mut patched = Vec::with_capacity(values.len());
    for (index, entry) in values.iter().enumerate() {
        let Some(name) = entry.as_str() else {
            return Err(DslError::InvalidSkill {
                of: Box::new(of.clone()),
                skill: entry.to_string(),
                reason: "Skill-Eintrag ist kein String",
                location: DiagLocation::field(format!("patch.skills[{index}]")),
            });
        };
        patched.push(name.to_owned());
    }
    for (index, name) in patched.iter().enumerate() {
        if let Err(reason) = validate_skill_name(name) {
            return Err(DslError::InvalidSkill {
                of: Box::new(of.clone()),
                skill: name.clone(),
                reason,
                location: DiagLocation::field(format!("patch.skills[{index}]")),
            });
        }
    }
    union_skills(skills, &patched);
    Ok(())
}

/// Wendet eine Tabelle von Merge-Operatoren an (z. B. `{ append = [...] }`).
///
/// # Beschreibung
/// Alle vorhandenen Operatoren laufen in der Reihenfolge von
/// [`PATCH_OPERATOR_KEYS`] (`replace`, `prepend`, `append`, `remove`,
/// `intersect`, `min`, `max-within-parent`). Ein unbekannter Schlüssel ist
/// ein Fehler, bevor irgendein Operator läuft; ein Array-Operator mit einem
/// Nicht-Array-Wert ebenfalls (früher wurde er still zu „keine Elemente“).
fn try_apply_table_op(current: &mut toml::Value, patch_table: &toml::Table) -> DslResult<()> {
    if let Some(unknown) = patch_table.keys().find(|key| !is_operator_key(key)) {
        return Err(DslError::UnknownMergeOp {
            name: unknown.clone(),
        });
    }
    for key in PATCH_OPERATOR_KEYS {
        let Some(val) = patch_table.get(*key) else {
            continue;
        };
        let op = match *key {
            "replace" => MergeOp::Replace { value: val.clone() },
            "prepend" => MergeOp::Prepend {
                values: operator_values(key, val)?,
            },
            "append" => MergeOp::Append {
                values: operator_values(key, val)?,
            },
            "remove" => MergeOp::Remove {
                values: operator_values(key, val)?,
            },
            "intersect" => MergeOp::Intersect {
                values: operator_values(key, val)?,
            },
            "min" => MergeOp::Min { value: val.clone() },
            _ => MergeOp::MaxWithinParent { value: val.clone() },
        };
        apply_merge_op(current, op)?;
    }
    Ok(())
}

/// Der Array-Wert eines Array-Operators.
///
/// # Errors
/// [`DslError::PatchTypeMismatch`], wenn `val` kein Array ist.
fn operator_values(op: &str, val: &toml::Value) -> DslResult<Vec<toml::Value>> {
    val.as_array()
        .cloned()
        .ok_or_else(|| DslError::PatchTypeMismatch {
            op: op.to_owned(),
            expected: "array",
            found: val.type_str(),
            location: DiagLocation::none(),
        })
}

/// Extrahiert eine `AuthorityCeiling` aus einer TOML-Tabelle, falls vorhanden.
fn extract_authority_from_table(tables: &toml::Table) -> Option<AuthorityCeiling> {
    let auth = tables.get("authority")?.as_table()?;
    let caps = auth.get("capabilities")?.as_array()?;
    let capabilities: Vec<String> = caps
        .iter()
        .filter_map(|v| v.as_str().map(str::to_owned))
        .collect();
    Some(AuthorityCeiling { capabilities })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layers::DefinitionLayer;
    use crate::parse::parse_toml;
    use crate::test_support::{TestError, TestResult};

    fn now() -> OffsetDateTime {
        OffsetDateTime::now_utc()
    }

    const MINIMAL_WORKER: &str = r#"
schema = "harwness.agent/v1"
id = "harwness.agent.worker@1"
version = "1.0.0"
role = "worker"
specialization = "test-worker"
"#;

    #[test]
    fn test_simple_no_extends() -> TestResult {
        let raw = parse_toml(MINIMAL_WORKER)?;
        let id = DefinitionId::parse("harwness.agent.worker@1")?;
        let layers = vec![(DefinitionLayer::BuiltIn, raw)];
        let resolved = resolve_definition(&id, &layers, now())?;
        assert_eq!(resolved.specialization, "test-worker");
        assert_eq!(resolved.role, crate::roles::AgentRoleId::Worker);
        Ok(())
    }

    #[test]
    fn test_missing_base_errors() -> TestResult {
        let src = r#"
schema = "harwness.agent/v1"
id = "harwness.agent.my-worker@1"
version = "1.0.0"
role = "worker"
specialization = "test"
extends = { id = "harwness.agent.nonexistent-base@1" }
"#;
        let raw = parse_toml(src)?;
        let id = DefinitionId::parse("harwness.agent.my-worker@1")?;
        let layers = vec![(DefinitionLayer::BuiltIn, raw)];
        let result = resolve_definition(&id, &layers, now());
        assert!(matches!(result, Err(DslError::MissingBase { .. })));
        Ok(())
    }

    #[test]
    fn test_authority_elevation_rejected() -> TestResult {
        // Ein Patch, der versucht, Capabilities per `append` hinzuzufügen
        let src = r#"
schema = "harwness.agent/v1"
id = "harwness.agent.evil-worker@1"
version = "1.0.0"
role = "worker"
specialization = "evil"

[authority]
capabilities = ["filesystem.read"]

[patch.authority.capabilities]
append = ["agent.spawn.child-orchestrator"]
"#;
        let raw = parse_toml(src)?;
        let id = DefinitionId::parse("harwness.agent.evil-worker@1")?;
        let layers = vec![(DefinitionLayer::BuiltIn, raw)];
        let result = resolve_definition(&id, &layers, now());
        assert!(
            matches!(result, Err(DslError::AuthorityElevation { .. })),
            "Erwartet AuthorityElevation, erhalten: {:?}",
            result
        );
        Ok(())
    }

    #[test]
    fn test_authority_elevation_display_contains_field_path() -> TestResult {
        // Verifies that the resolver passes field_path "authority.capabilities" into AuthorityElevation.
        let src = r#"
schema = "harwness.agent/v1"
id = "harwness.agent.evil-worker@2"
version = "1.0.0"
role = "worker"
specialization = "evil"

[authority]
capabilities = ["filesystem.read"]

[patch.authority.capabilities]
append = ["agent.spawn.child-orchestrator"]
"#;
        let raw = parse_toml(src)?;
        let id = DefinitionId::parse("harwness.agent.evil-worker@2")?;
        let layers = vec![(DefinitionLayer::BuiltIn, raw)];
        let Err(err) = resolve_definition(&id, &layers, now()) else {
            return Err(TestError::Unexpected("Err erwartet".into()));
        };
        let msg = err.to_string();
        assert!(
            msg.contains("authority.capabilities"),
            "Display should contain 'authority.capabilities', got: {msg}"
        );
        Ok(())
    }

    #[test]
    fn test_resolve_trace_contains_base_and_mixin_steps() -> TestResult {
        let base_src = r#"
schema = "harwness.agent/v1"
id = "harwness.agent.worker-base@1"
version = "1.0.0"
role = "worker"
specialization = "base"
"#;
        let mixin_src = r#"
schema = "harwness.mixin/v1"
id = "harwness.mixin.rust-coding@1"
version = "1.0.0"
role = "worker"
specialization = "mixin"
"#;
        let target_src = r#"
schema = "harwness.agent/v1"
id = "harwness.agent.rust-worker@1"
version = "1.0.0"
role = "worker"
specialization = "rust-worker"
extends = { id = "harwness.agent.worker-base@1" }
mixins = [{ id = "harwness.mixin.rust-coding@1" }]
"#;
        let base = parse_toml(base_src)?;
        let mixin = parse_toml(mixin_src)?;
        let target = parse_toml(target_src)?;
        let id = DefinitionId::parse("harwness.agent.rust-worker@1")?;
        let layers = vec![
            (DefinitionLayer::BuiltIn, base),
            (DefinitionLayer::BuiltIn, mixin),
            (DefinitionLayer::BuiltIn, target),
        ];
        let resolved = resolve_definition(&id, &layers, now())?;
        // Trace muss: base-Step, mixin-Step, patch-Step (target) enthalten = ≥ 2 Steps
        assert!(
            resolved.trace.steps.len() >= 2,
            "Trace sollte mindestens 2 Schritte enthalten, hat {}",
            resolved.trace.steps.len()
        );
        let kinds: Vec<&str> = resolved
            .trace
            .steps
            .iter()
            .map(|s| s.kind.as_str())
            .collect();
        assert!(
            kinds.contains(&"base"),
            "Trace muss 'base'-Schritt enthalten"
        );
        assert!(
            kinds.contains(&"mixin"),
            "Trace muss 'mixin'-Schritt enthalten"
        );
        Ok(())
    }

    #[test]
    fn test_resolve_with_layer_priority() -> TestResult {
        // Höhere Schicht (Project) überschreibt BuiltIn
        let builtin = parse_toml(MINIMAL_WORKER)?;
        let project_src = r#"
schema = "harwness.agent/v1"
id = "harwness.agent.worker@1"
version = "2.0.0"
role = "worker"
specialization = "project-override"
"#;
        let project = parse_toml(project_src)?;
        let id = DefinitionId::parse("harwness.agent.worker@1")?;
        let layers = vec![
            (DefinitionLayer::BuiltIn, builtin),
            (DefinitionLayer::Project, project),
        ];
        let resolved = resolve_definition(&id, &layers, now())?;
        assert_eq!(resolved.specialization, "project-override");
        assert_eq!(resolved.version.0.major, 2);
        Ok(())
    }

    #[test]
    fn test_resolve_recursively_applies_all_extends() -> TestResult {
        let root = parse_toml(
            r#"
schema = "harwness.agent/v1"
id = "harwness.agent.root-base@1"
version = "1.0.0"
role = "worker"
specialization = "root"

[root_only]
enabled = true
"#,
        )?;
        let middle = parse_toml(
            r#"
schema = "harwness.agent/v1"
id = "harwness.agent.middle-base@1"
version = "1.0.0"
role = "worker"
specialization = "middle"
extends = { id = "harwness.agent.root-base@1" }

[middle_only]
enabled = true
"#,
        )?;
        let target = parse_toml(
            r#"
schema = "harwness.agent/v1"
id = "harwness.agent.recursive-target@1"
version = "1.0.0"
role = "worker"
specialization = "target"
extends = { id = "harwness.agent.middle-base@1" }
"#,
        )?;
        let id = DefinitionId::parse("harwness.agent.recursive-target@1")?;
        let resolved = resolve_definition(
            &id,
            &[
                (DefinitionLayer::BuiltIn, root),
                (DefinitionLayer::BuiltIn, middle),
                (DefinitionLayer::BuiltIn, target),
            ],
            now(),
        )?;

        assert!(resolved.config.contains_key("root_only"));
        assert!(resolved.config.contains_key("middle_only"));
        let base_sources: Vec<&str> = resolved
            .trace
            .steps
            .iter()
            .filter(|step| step.kind == "base")
            .map(|step| step.source.as_str())
            .collect();
        assert_eq!(
            base_sources,
            vec!["harwness.agent.root-base@1", "harwness.agent.middle-base@1"]
        );
        Ok(())
    }

    #[test]
    fn test_resolve_recursively_applies_extends_from_higher_target_layer() -> TestResult {
        let root = parse_toml(
            r#"
schema = "harwness.agent/v1"
id = "harwness.agent.overlay-root@1"
version = "1.0.0"
role = "worker"
specialization = "root"

[root_only]
enabled = true
"#,
        )?;
        let base = parse_toml(
            r#"
schema = "harwness.agent/v1"
id = "harwness.agent.overlay-base@1"
version = "1.0.0"
role = "worker"
specialization = "base"
extends = { id = "harwness.agent.overlay-root@1" }

[base_only]
enabled = true
"#,
        )?;
        let lower_target = parse_toml(
            r#"
schema = "harwness.agent/v1"
id = "harwness.agent.overlay-target@1"
version = "1.0.0"
role = "worker"
specialization = "lower"

[lower_only]
enabled = true
"#,
        )?;
        let higher_target = parse_toml(
            r#"
schema = "harwness.agent/v1"
id = "harwness.agent.overlay-target@1"
version = "1.1.0"
role = "worker"
specialization = "higher"
extends = { id = "harwness.agent.overlay-base@1" }

[higher_only]
enabled = true
"#,
        )?;
        let id = DefinitionId::parse("harwness.agent.overlay-target@1")?;

        let resolved = resolve_definition(
            &id,
            &[
                (DefinitionLayer::BuiltIn, root),
                (DefinitionLayer::BuiltIn, base),
                (DefinitionLayer::BuiltIn, lower_target),
                (DefinitionLayer::Project, higher_target),
            ],
            now(),
        )?;

        for table in ["root_only", "base_only", "lower_only", "higher_only"] {
            assert!(resolved.config.contains_key(table), "missing {table}");
        }
        Ok(())
    }

    #[test]
    fn test_resolve_rejects_cycle_declared_by_higher_target_layer() -> TestResult {
        let lower_target = parse_toml(
            r#"
schema = "harwness.agent/v1"
id = "harwness.agent.overlay-cycle@1"
version = "1.0.0"
role = "worker"
specialization = "lower"
"#,
        )?;
        let higher_target = parse_toml(
            r#"
schema = "harwness.agent/v1"
id = "harwness.agent.overlay-cycle@1"
version = "1.1.0"
role = "worker"
specialization = "higher"
extends = { id = "harwness.agent.overlay-cycle@1" }
"#,
        )?;
        let id = DefinitionId::parse("harwness.agent.overlay-cycle@1")?;

        let result = resolve_definition(
            &id,
            &[
                (DefinitionLayer::BuiltIn, lower_target),
                (DefinitionLayer::Project, higher_target),
            ],
            now(),
        );
        let Err(error) = result else {
            return Err(TestError::Unexpected("Err erwartet".into()));
        };

        assert!(matches!(error, DslError::InheritanceCycle { .. }));
        Ok(())
    }

    #[test]
    fn test_resolve_honors_explicit_reference_version() -> TestResult {
        let base_v1 = parse_toml(
            r#"
schema = "harwness.agent/v1"
id = "harwness.agent.versioned-base@1"
version = "1.0.0"
role = "worker"
specialization = "v1"

[selected]
version = "1.0.0"
"#,
        )?;
        let base_v2 = parse_toml(
            r#"
schema = "harwness.agent/v1"
id = "harwness.agent.versioned-base@1"
version = "1.2.0"
role = "worker"
specialization = "v2"

[selected]
version = "1.2.0"
"#,
        )?;
        let target = parse_toml(
            r#"
schema = "harwness.agent/v1"
id = "harwness.agent.versioned-target@1"
version = "1.0.0"
role = "worker"
specialization = "target"
extends = { id = "harwness.agent.versioned-base@1", version = "1.0.0" }
"#,
        )?;
        let id = DefinitionId::parse("harwness.agent.versioned-target@1")?;
        let resolved = resolve_definition(
            &id,
            &[
                (DefinitionLayer::BuiltIn, base_v1),
                (DefinitionLayer::BuiltIn, base_v2),
                (DefinitionLayer::BuiltIn, target),
            ],
            now(),
        )?;

        assert_eq!(
            resolved.config["selected"]["version"].as_str(),
            Some("1.0.0")
        );
        Ok(())
    }

    #[test]
    fn test_resolve_uses_newest_unpinned_reference_version_in_winning_layer() -> TestResult {
        let newest_base = parse_toml(
            r#"
schema = "harwness.agent/v1"
id = "harwness.agent.unpinned-base@1"
version = "1.2.0"
role = "worker"
specialization = "newest"

[selected]
version = "1.2.0"
"#,
        )?;
        let older_base = parse_toml(
            r#"
schema = "harwness.agent/v1"
id = "harwness.agent.unpinned-base@1"
version = "1.0.0"
role = "worker"
specialization = "older"

[selected]
version = "1.0.0"
"#,
        )?;
        let target = parse_toml(
            r#"
schema = "harwness.agent/v1"
id = "harwness.agent.unpinned-target@1"
version = "1.0.0"
role = "worker"
specialization = "target"
extends = { id = "harwness.agent.unpinned-base@1" }
"#,
        )?;
        let id = DefinitionId::parse("harwness.agent.unpinned-target@1")?;

        let resolved = resolve_definition(
            &id,
            &[
                (DefinitionLayer::BuiltIn, newest_base),
                (DefinitionLayer::BuiltIn, older_base),
                (DefinitionLayer::BuiltIn, target),
            ],
            now(),
        )?;

        assert_eq!(
            resolved.config["selected"]["version"].as_str(),
            Some("1.2.0")
        );
        Ok(())
    }

    #[test]
    fn test_resolve_rejects_direct_inheritance_cycle() -> TestResult {
        let definition = parse_toml(
            r#"
schema = "harwness.agent/v1"
id = "harwness.agent.self-cycle@1"
version = "1.0.0"
role = "worker"
specialization = "cycle"
extends = { id = "harwness.agent.self-cycle@1" }
"#,
        )?;
        let id = DefinitionId::parse("harwness.agent.self-cycle@1")?;

        let result = resolve_definition(&id, &[(DefinitionLayer::BuiltIn, definition)], now());
        let Err(error) = result else {
            return Err(TestError::Unexpected("Err erwartet".into()));
        };

        let DslError::InheritanceCycle { cycle, location } = error else {
            return Err(TestError::Unexpected("expected InheritanceCycle".into()));
        };
        assert_eq!(cycle, vec![id.clone(), id]);
        assert_eq!(location.field_path.as_deref(), Some("extends"));
        Ok(())
    }

    #[test]
    fn test_resolve_rejects_indirect_inheritance_cycle_with_closed_path() -> TestResult {
        let first = parse_toml(
            r#"
schema = "harwness.agent/v1"
id = "harwness.agent.cycle-a@1"
version = "1.0.0"
role = "worker"
specialization = "a"
extends = { id = "harwness.agent.cycle-b@1" }
"#,
        )?;
        let second = parse_toml(
            r#"
schema = "harwness.agent/v1"
id = "harwness.agent.cycle-b@1"
version = "1.0.0"
role = "worker"
specialization = "b"
extends = { id = "harwness.agent.cycle-c@1" }
"#,
        )?;
        let third = parse_toml(
            r#"
schema = "harwness.agent/v1"
id = "harwness.agent.cycle-c@1"
version = "1.0.0"
role = "worker"
specialization = "c"
extends = { id = "harwness.agent.cycle-b@1" }
"#,
        )?;
        let id = DefinitionId::parse("harwness.agent.cycle-a@1")?;

        let result = resolve_definition(
            &id,
            &[
                (DefinitionLayer::BuiltIn, first),
                (DefinitionLayer::BuiltIn, second),
                (DefinitionLayer::BuiltIn, third),
            ],
            now(),
        );
        let Err(error) = result else {
            return Err(TestError::Unexpected("Err erwartet".into()));
        };

        let DslError::InheritanceCycle { cycle, location } = error else {
            return Err(TestError::Unexpected("expected InheritanceCycle".into()));
        };
        assert_eq!(
            cycle,
            vec![
                DefinitionId::parse("harwness.agent.cycle-b@1")?,
                DefinitionId::parse("harwness.agent.cycle-c@1")?,
                DefinitionId::parse("harwness.agent.cycle-b@1")?,
            ]
        );
        assert_eq!(location.field_path.as_deref(), Some("extends"));
        Ok(())
    }

    #[test]
    fn test_resolve_includes_target_direct_authority() -> TestResult {
        let definition = parse_toml(
            r#"
schema = "harwness.agent/v1"
id = "harwness.agent.authority-target@1"
version = "1.0.0"
role = "worker"
specialization = "authority"

[authority]
capabilities = ["filesystem.read", "process.spawn.sandboxed"]
"#,
        )?;
        let id = DefinitionId::parse("harwness.agent.authority-target@1")?;

        let resolved = resolve_definition(&id, &[(DefinitionLayer::BuiltIn, definition)], now())?;

        assert_eq!(
            resolved.authority.capabilities,
            vec!["filesystem.read", "process.spawn.sandboxed"]
        );
        Ok(())
    }

    #[test]
    fn test_resolve_rejects_direct_authority_elevation_over_base() -> TestResult {
        let base = parse_toml(
            r#"
schema = "harwness.agent/v1"
id = "harwness.agent.authority-base@1"
version = "1.0.0"
role = "worker"
specialization = "base"

[authority]
capabilities = ["filesystem.read"]
"#,
        )?;
        let target = parse_toml(
            r#"
schema = "harwness.agent/v1"
id = "harwness.agent.authority-child@1"
version = "1.0.0"
role = "worker"
specialization = "child"
extends = { id = "harwness.agent.authority-base@1" }

[authority]
capabilities = ["filesystem.read", "filesystem.write"]
"#,
        )?;
        let id = DefinitionId::parse("harwness.agent.authority-child@1")?;

        let result = resolve_definition(
            &id,
            &[
                (DefinitionLayer::BuiltIn, base),
                (DefinitionLayer::BuiltIn, target),
            ],
            now(),
        );
        let Err(error) = result else {
            return Err(TestError::Unexpected("Err erwartet".into()));
        };

        let DslError::AuthorityElevation {
            of,
            added_capabilities,
            location,
        } = error
        else {
            return Err(TestError::Unexpected("expected AuthorityElevation".into()));
        };
        assert_eq!(*of, id);
        assert_eq!(added_capabilities, vec!["filesystem.write"]);
        assert_eq!(
            location.field_path.as_deref(),
            Some("authority.capabilities")
        );
        Ok(())
    }

    #[test]
    fn test_resolve_validates_role_of_mixin_declared_by_base() -> TestResult {
        let mixin = parse_toml(
            r#"
schema = "harwness.mixin/v1"
id = "harwness.mixin.incompatible-base-mixin@1"
version = "1.0.0"
role = "root-orchestrator"
specialization = "mixin"
"#,
        )?;
        let base = parse_toml(
            r#"
schema = "harwness.agent/v1"
id = "harwness.agent.mixin-role-base@1"
version = "1.0.0"
role = "worker"
specialization = "base"
mixins = [{ id = "harwness.mixin.incompatible-base-mixin@1" }]
"#,
        )?;
        let target = parse_toml(
            r#"
schema = "harwness.agent/v1"
id = "harwness.agent.mixin-role-target@1"
version = "1.0.0"
role = "worker"
specialization = "target"
extends = { id = "harwness.agent.mixin-role-base@1" }
"#,
        )?;
        let id = DefinitionId::parse("harwness.agent.mixin-role-target@1")?;

        let result = resolve_definition(
            &id,
            &[
                (DefinitionLayer::BuiltIn, mixin),
                (DefinitionLayer::BuiltIn, base),
                (DefinitionLayer::BuiltIn, target),
            ],
            now(),
        );
        let Err(error) = result else {
            return Err(TestError::Unexpected("Err erwartet".into()));
        };

        assert!(matches!(error, DslError::IllegalRoleForMixin { .. }));
        Ok(())
    }

    #[test]
    fn test_resolve_composes_lower_layer_tables_with_highest_scalar_overrides() -> TestResult {
        let lower = parse_toml(
            r#"
schema = "harwness.agent/v1"
id = "harwness.agent.layered@1"
version = "1.0.0"
role = "worker"
specialization = "lower"
name = "Lower"

[work]
mode = "lower"
lower_only = true
labels = ["builtin"]
"#,
        )?;
        let higher = parse_toml(
            r#"
schema = "harwness.agent/v1"
id = "harwness.agent.layered@1"
version = "1.2.0"
role = "root-orchestrator"
specialization = "higher"
name = "Higher"

[work]
mode = "higher"
higher_only = true
labels = ["project"]
"#,
        )?;
        let id = DefinitionId::parse("harwness.agent.layered@1")?;

        let resolved = resolve_definition(
            &id,
            &[
                (DefinitionLayer::Project, higher),
                (DefinitionLayer::BuiltIn, lower),
            ],
            now(),
        )?;

        assert_eq!(resolved.version.0, semver::Version::new(1, 2, 0));
        assert_eq!(resolved.specialization, "higher");
        assert_eq!(resolved.name.as_deref(), Some("Higher"));
        assert_eq!(resolved.role, crate::roles::AgentRoleId::Worker);
        assert_eq!(resolved.config["work"]["mode"].as_str(), Some("higher"));
        assert_eq!(resolved.config["work"]["lower_only"].as_bool(), Some(true));
        assert_eq!(resolved.config["work"]["higher_only"].as_bool(), Some(true));
        assert_eq!(
            resolved.config["work"]["labels"].as_array(),
            Some(&vec![
                toml::Value::String("builtin".to_owned()),
                toml::Value::String("project".to_owned()),
            ])
        );
        Ok(())
    }

    #[test]
    fn test_resolve_rejects_authority_elevation_in_higher_layer() -> TestResult {
        let lower = parse_toml(
            r#"
schema = "harwness.agent/v1"
id = "harwness.agent.layered-authority@1"
version = "1.0.0"
role = "worker"
specialization = "lower"

[authority]
capabilities = ["filesystem.read"]
"#,
        )?;
        let higher = parse_toml(
            r#"
schema = "harwness.agent/v1"
id = "harwness.agent.layered-authority@1"
version = "1.1.0"
role = "worker"
specialization = "higher"

[authority]
capabilities = ["filesystem.read", "filesystem.write"]
"#,
        )?;
        let id = DefinitionId::parse("harwness.agent.layered-authority@1")?;

        let result = resolve_definition(
            &id,
            &[
                (DefinitionLayer::BuiltIn, lower),
                (DefinitionLayer::Project, higher),
            ],
            now(),
        );
        let Err(error) = result else {
            return Err(TestError::Unexpected("Err erwartet".into()));
        };

        let DslError::AuthorityElevation {
            added_capabilities,
            location,
            ..
        } = error
        else {
            return Err(TestError::Unexpected("expected AuthorityElevation".into()));
        };
        assert_eq!(added_capabilities, vec!["filesystem.write"]);
        assert_eq!(
            location.field_path.as_deref(),
            Some("authority.capabilities")
        );
        Ok(())
    }

    /// Basis `[read, write]`; eine niedrigere Zielschicht verengt per `extends`
    /// auf `[read]`; darüber ein Project-Overlay, das dieselbe Basis erneut
    /// per `extends` einbindet und `overlay_extra` anhängt.
    fn overlay_extends_layers(
        overlay_extra: &str,
    ) -> TestResult<Vec<(DefinitionLayer, RawAgentDefinition)>> {
        let base = parse_toml(
            r#"
schema = "harwness.agent/v1"
id = "harwness.agent.ceiling-base@1"
version = "1.0.0"
role = "worker"
specialization = "base"

[authority]
capabilities = ["filesystem.read", "filesystem.write"]
"#,
        )?;
        let lower = parse_toml(
            r#"
schema = "harwness.agent/v1"
id = "harwness.agent.ceiling-target@1"
version = "1.0.0"
role = "worker"
specialization = "lower"
extends = { id = "harwness.agent.ceiling-base@1" }

[authority]
capabilities = ["filesystem.read"]
"#,
        )?;
        let overlay = parse_toml(&format!(
            r#"
schema = "harwness.agent/v1"
id = "harwness.agent.ceiling-target@1"
version = "1.1.0"
role = "worker"
specialization = "overlay"
extends = {{ id = "harwness.agent.ceiling-base@1" }}
{overlay_extra}
"#
        ))?;
        Ok(vec![
            (DefinitionLayer::BuiltIn, base),
            (DefinitionLayer::UserGlobal, lower),
            (DefinitionLayer::Project, overlay),
        ])
    }

    #[test]
    fn test_resolve_rejects_overlay_extends_that_resets_lower_ceiling() -> TestResult {
        let id = DefinitionId::parse("harwness.agent.ceiling-target@1")?;
        match resolve_definition(&id, &overlay_extends_layers("")?, now()) {
            Err(DslError::AuthorityElevation {
                of,
                added_capabilities,
                location,
            }) => {
                assert_eq!(*of, id);
                assert_eq!(added_capabilities, vec!["filesystem.write"]);
                assert_eq!(location.field_path.as_deref(), Some("extends"));
                Ok(())
            }
            other => Err(TestError::Unexpected(format!(
                "AuthorityElevation expected, got {other:?}"
            ))),
        }
    }

    #[test]
    fn test_resolve_rejects_overlay_extends_authority_above_lower_ceiling() -> TestResult {
        // Innerhalb der Basis, aber über der Verengung der niedrigeren Schicht.
        let id = DefinitionId::parse("harwness.agent.ceiling-target@1")?;
        let layers = overlay_extends_layers(
            r#"
[authority]
capabilities = ["filesystem.read", "filesystem.write"]
"#,
        )?;
        match resolve_definition(&id, &layers, now()) {
            Err(DslError::AuthorityElevation {
                added_capabilities,
                location,
                ..
            }) => {
                assert_eq!(added_capabilities, vec!["filesystem.write"]);
                assert_eq!(
                    location.field_path.as_deref(),
                    Some("authority.capabilities")
                );
                Ok(())
            }
            other => Err(TestError::Unexpected(format!(
                "AuthorityElevation expected, got {other:?}"
            ))),
        }
    }

    #[test]
    fn test_resolve_accepts_overlay_extends_that_keeps_lower_ceiling() -> TestResult {
        let id = DefinitionId::parse("harwness.agent.ceiling-target@1")?;
        let layers = overlay_extends_layers(
            r#"
[authority]
capabilities = ["filesystem.read"]
"#,
        )?;
        let resolved = resolve_definition(&id, &layers, now())?;
        assert_eq!(resolved.authority.capabilities, vec!["filesystem.read"]);
        assert_eq!(resolved.specialization, "overlay");
        Ok(())
    }

    // -----------------------------------------------------------------------
    // reasoning_effort: Vererbung über extends/mixins/Schichten
    // (Rollen-Konflikt-Rangfolge: Provider > Modell > Agent > Rolle — dieser
    // Knoten liefert nur die Agent-Ebene; siehe guard_wiring::resolve_default_reasoning_effort)
    // -----------------------------------------------------------------------

    #[test]
    fn test_reasoning_effort_absent_everywhere_resolves_to_none() -> TestResult {
        let raw = parse_toml(MINIMAL_WORKER)?;
        let id = DefinitionId::parse("harwness.agent.worker@1")?;
        let layers = vec![(DefinitionLayer::BuiltIn, raw)];
        let resolved = resolve_definition(&id, &layers, now())?;
        assert!(resolved.reasoning_effort.is_none());
        Ok(())
    }

    #[test]
    fn test_reasoning_effort_set_only_on_target_is_used() -> TestResult {
        let src = r#"
schema = "harwness.agent/v1"
id = "harwness.agent.effort-target@1"
version = "1.0.0"
role = "worker"
specialization = "effort-target"
reasoning_effort = "high"
"#;
        let raw = parse_toml(src)?;
        let id = DefinitionId::parse("harwness.agent.effort-target@1")?;
        let layers = vec![(DefinitionLayer::BuiltIn, raw)];
        let resolved = resolve_definition(&id, &layers, now())?;
        assert_eq!(resolved.reasoning_effort.as_deref(), Some("high"));
        Ok(())
    }

    #[test]
    fn test_reasoning_effort_inherited_from_extends_base_when_target_silent() -> TestResult {
        // Basis setzt reasoning_effort; das Ziel selbst schweigt dazu -> die
        // Basisaussage wird übernommen (spezifischste vorhandene Aussage gewinnt,
        // eine schweigende Ebene löscht nichts).
        let base = parse_toml(
            r#"
schema = "harwness.agent/v1"
id = "harwness.agent.effort-base@1"
version = "1.0.0"
role = "worker"
specialization = "base"
reasoning_effort = "low"
"#,
        )?;
        let target = parse_toml(
            r#"
schema = "harwness.agent/v1"
id = "harwness.agent.effort-child@1"
version = "1.0.0"
role = "worker"
specialization = "child"
extends = { id = "harwness.agent.effort-base@1" }
"#,
        )?;
        let id = DefinitionId::parse("harwness.agent.effort-child@1")?;
        let layers = vec![
            (DefinitionLayer::BuiltIn, base),
            (DefinitionLayer::BuiltIn, target),
        ];
        let resolved = resolve_definition(&id, &layers, now())?;
        assert_eq!(resolved.reasoning_effort.as_deref(), Some("low"));
        Ok(())
    }

    #[test]
    fn test_reasoning_effort_target_overrides_extends_base() -> TestResult {
        // Spezifischere Definition (Ziel) überschreibt die Aussage der Basis —
        // identisch zur Vererbungsregel von BudgetSpec::effort_cap.
        let base = parse_toml(
            r#"
schema = "harwness.agent/v1"
id = "harwness.agent.effort-base2@1"
version = "1.0.0"
role = "worker"
specialization = "base"
reasoning_effort = "low"
"#,
        )?;
        let target = parse_toml(
            r#"
schema = "harwness.agent/v1"
id = "harwness.agent.effort-child2@1"
version = "1.0.0"
role = "worker"
specialization = "child"
extends = { id = "harwness.agent.effort-base2@1" }
reasoning_effort = "high"
"#,
        )?;
        let id = DefinitionId::parse("harwness.agent.effort-child2@1")?;
        let layers = vec![
            (DefinitionLayer::BuiltIn, base),
            (DefinitionLayer::BuiltIn, target),
        ];
        let resolved = resolve_definition(&id, &layers, now())?;
        assert_eq!(resolved.reasoning_effort.as_deref(), Some("high"));
        Ok(())
    }

    #[test]
    fn test_reasoning_effort_inherited_from_mixin_when_target_silent() -> TestResult {
        let mixin = parse_toml(
            r#"
schema = "harwness.mixin/v1"
id = "harwness.mixin.effort-mixin@1"
version = "1.0.0"
role = "worker"
specialization = "mixin"
reasoning_effort = "medium"
"#,
        )?;
        let target = parse_toml(
            r#"
schema = "harwness.agent/v1"
id = "harwness.agent.effort-mixin-user@1"
version = "1.0.0"
role = "worker"
specialization = "target"
mixins = [{ id = "harwness.mixin.effort-mixin@1" }]
"#,
        )?;
        let id = DefinitionId::parse("harwness.agent.effort-mixin-user@1")?;
        let layers = vec![
            (DefinitionLayer::BuiltIn, mixin),
            (DefinitionLayer::BuiltIn, target),
        ];
        let resolved = resolve_definition(&id, &layers, now())?;
        assert_eq!(resolved.reasoning_effort.as_deref(), Some("medium"));
        Ok(())
    }

    #[test]
    fn test_reasoning_effort_target_overrides_mixin() -> TestResult {
        let mixin = parse_toml(
            r#"
schema = "harwness.mixin/v1"
id = "harwness.mixin.effort-mixin2@1"
version = "1.0.0"
role = "worker"
specialization = "mixin"
reasoning_effort = "medium"
"#,
        )?;
        let target = parse_toml(
            r#"
schema = "harwness.agent/v1"
id = "harwness.agent.effort-mixin-user2@1"
version = "1.0.0"
role = "worker"
specialization = "target"
mixins = [{ id = "harwness.mixin.effort-mixin2@1" }]
reasoning_effort = "high"
"#,
        )?;
        let id = DefinitionId::parse("harwness.agent.effort-mixin-user2@1")?;
        let layers = vec![
            (DefinitionLayer::BuiltIn, mixin),
            (DefinitionLayer::BuiltIn, target),
        ];
        let resolved = resolve_definition(&id, &layers, now())?;
        assert_eq!(resolved.reasoning_effort.as_deref(), Some("high"));
        Ok(())
    }

    #[test]
    fn test_reasoning_effort_higher_layer_overrides_lower_layer() -> TestResult {
        let builtin = parse_toml(
            r#"
schema = "harwness.agent/v1"
id = "harwness.agent.effort-layered@1"
version = "1.0.0"
role = "worker"
specialization = "builtin"
reasoning_effort = "low"
"#,
        )?;
        let project = parse_toml(
            r#"
schema = "harwness.agent/v1"
id = "harwness.agent.effort-layered@1"
version = "1.1.0"
role = "worker"
specialization = "project"
reasoning_effort = "high"
"#,
        )?;
        let id = DefinitionId::parse("harwness.agent.effort-layered@1")?;
        let layers = vec![
            (DefinitionLayer::BuiltIn, builtin),
            (DefinitionLayer::Project, project),
        ];
        let resolved = resolve_definition(&id, &layers, now())?;
        assert_eq!(resolved.reasoning_effort.as_deref(), Some("high"));
        Ok(())
    }

    #[test]
    fn test_reasoning_effort_higher_layer_silent_keeps_lower_layer_value() -> TestResult {
        let builtin = parse_toml(
            r#"
schema = "harwness.agent/v1"
id = "harwness.agent.effort-layered2@1"
version = "1.0.0"
role = "worker"
specialization = "builtin"
reasoning_effort = "low"
"#,
        )?;
        let project = parse_toml(
            r#"
schema = "harwness.agent/v1"
id = "harwness.agent.effort-layered2@1"
version = "1.1.0"
role = "worker"
specialization = "project"
"#,
        )?;
        let id = DefinitionId::parse("harwness.agent.effort-layered2@1")?;
        let layers = vec![
            (DefinitionLayer::BuiltIn, builtin),
            (DefinitionLayer::Project, project),
        ];
        let resolved = resolve_definition(&id, &layers, now())?;
        assert_eq!(resolved.reasoning_effort.as_deref(), Some("low"));
        Ok(())
    }

    // -----------------------------------------------------------------
    // skills: erstklassiges Feld, Vereinigung über extends/Mixins/Schichten
    // -----------------------------------------------------------------

    fn skills_base() -> TestResult<RawAgentDefinition> {
        Ok(parse_toml(
            r#"
schema = "harwness.agent/v1"
id = "harwness.agent.skills-base@1"
version = "1.0.0"
role = "worker"
specialization = "base"
skills = ["review", "rust"]
"#,
        )?)
    }

    #[test]
    fn test_skills_absent_everywhere_leaves_config_untouched() -> TestResult {
        let raw = parse_toml(MINIMAL_WORKER)?;
        let id = DefinitionId::parse("harwness.agent.worker@1")?;
        let resolved = resolve_definition(&id, &[(DefinitionLayer::BuiltIn, raw)], now())?;
        assert!(resolved.skills().is_empty());
        assert!(!resolved.config.contains_key(SKILLS_CONFIG_KEY));
        Ok(())
    }

    #[test]
    fn test_skills_union_over_extends_and_mixin_dedupes() -> TestResult {
        let mixin = parse_toml(
            r#"
schema = "harwness.mixin/v1"
id = "harwness.mixin.skills-mixin@1"
version = "1.0.0"
role = "worker"
specialization = "mixin"
skills = ["docs", "review"]
"#,
        )?;
        let target = parse_toml(
            r#"
schema = "harwness.agent/v1"
id = "harwness.agent.skills-child@1"
version = "1.0.0"
role = "worker"
specialization = "child"
extends = { id = "harwness.agent.skills-base@1" }
mixins = [{ id = "harwness.mixin.skills-mixin@1" }]
skills = ["rust", "testing"]
"#,
        )?;
        let id = DefinitionId::parse("harwness.agent.skills-child@1")?;
        let layers = vec![
            (DefinitionLayer::BuiltIn, skills_base()?),
            (DefinitionLayer::BuiltIn, mixin),
            (DefinitionLayer::BuiltIn, target),
        ];
        let resolved = resolve_definition(&id, &layers, now())?;
        assert_eq!(resolved.skills(), ["review", "rust", "docs", "testing"]);
        Ok(())
    }

    #[test]
    fn test_skills_higher_layer_adds_without_dropping_lower() -> TestResult {
        let builtin = parse_toml(
            r#"
schema = "harwness.agent/v1"
id = "harwness.agent.skills-layered@1"
version = "1.0.0"
role = "worker"
specialization = "builtin"
skills = ["review"]
"#,
        )?;
        let project = parse_toml(
            r#"
schema = "harwness.agent/v1"
id = "harwness.agent.skills-layered@1"
version = "1.1.0"
role = "worker"
specialization = "project"
skills = ["docs"]
"#,
        )?;
        let id = DefinitionId::parse("harwness.agent.skills-layered@1")?;
        let layers = vec![
            (DefinitionLayer::BuiltIn, builtin),
            (DefinitionLayer::Project, project),
        ];
        let resolved = resolve_definition(&id, &layers, now())?;
        assert_eq!(resolved.skills(), ["review", "docs"]);
        Ok(())
    }

    #[test]
    fn test_skills_patch_remove_drops_inherited_skill() -> TestResult {
        let target = parse_toml(
            r#"
schema = "harwness.agent/v1"
id = "harwness.agent.skills-trim@1"
version = "1.0.0"
role = "worker"
specialization = "trim"
extends = { id = "harwness.agent.skills-base@1" }
skills = ["docs"]

[patch.skills]
remove = ["rust"]
"#,
        )?;
        let id = DefinitionId::parse("harwness.agent.skills-trim@1")?;
        let layers = vec![
            (DefinitionLayer::BuiltIn, skills_base()?),
            (DefinitionLayer::BuiltIn, target),
        ];
        let resolved = resolve_definition(&id, &layers, now())?;
        assert_eq!(resolved.skills(), ["review", "docs"]);
        Ok(())
    }

    #[test]
    fn test_skills_invalid_name_rejected_with_field_path() -> TestResult {
        let raw = parse_toml(
            r#"
schema = "harwness.agent/v1"
id = "harwness.agent.skills-bad@1"
version = "1.0.0"
role = "worker"
specialization = "bad"
skills = ["ok", "Not_Ok"]
"#,
        )?;
        let id = DefinitionId::parse("harwness.agent.skills-bad@1")?;
        let Err(error) = resolve_definition(&id, &[(DefinitionLayer::BuiltIn, raw)], now()) else {
            return Err(TestError::Unexpected("Err erwartet".into()));
        };
        assert!(matches!(error, DslError::InvalidSkill { .. }), "{error}");
        assert!(error.to_string().contains("skills[1]"), "{error}");
        Ok(())
    }

    #[test]
    fn test_skills_duplicate_in_one_definition_rejected() -> TestResult {
        let raw = parse_toml(
            r#"
schema = "harwness.agent/v1"
id = "harwness.agent.skills-dup@1"
version = "1.0.0"
role = "worker"
specialization = "dup"
skills = ["review", "review"]
"#,
        )?;
        let id = DefinitionId::parse("harwness.agent.skills-dup@1")?;
        let result = resolve_definition(&id, &[(DefinitionLayer::BuiltIn, raw)], now());
        assert!(matches!(result, Err(DslError::InvalidSkill { .. })));
        Ok(())
    }

    #[test]
    fn test_skills_invalid_name_in_mixin_rejected() -> TestResult {
        let mixin_src = format!(
            r#"
schema = "harwness.mixin/v1"
id = "harwness.mixin.skills-bad-mixin@1"
version = "1.0.0"
role = "worker"
specialization = "mixin"
skills = ["{}"]
"#,
            "x".repeat(65)
        );
        let mixin = parse_toml(&mixin_src)?;
        let target = parse_toml(
            r#"
schema = "harwness.agent/v1"
id = "harwness.agent.skills-mixin-user@1"
version = "1.0.0"
role = "worker"
specialization = "target"
mixins = [{ id = "harwness.mixin.skills-bad-mixin@1" }]
"#,
        )?;
        let id = DefinitionId::parse("harwness.agent.skills-mixin-user@1")?;
        let layers = vec![
            (DefinitionLayer::BuiltIn, mixin),
            (DefinitionLayer::BuiltIn, target),
        ];
        let result = resolve_definition(&id, &layers, now());
        assert!(matches!(result, Err(DslError::InvalidSkill { .. })));
        Ok(())
    }

    // -----------------------------------------------------------------
    // IR v2 wave 1: schema check, nested patch paths, min / max-within-parent
    // -----------------------------------------------------------------

    const PATCH_BASE: &str = r#"
schema = "harwness.agent/v1"
id = "harwness.agent.patch-base@1"
version = "1.0.0"
role = "worker"
specialization = "patch-base"

[context]
must_include = ["task.objective"]
exclude = ["secret.*"]

[limits]
max_tool_calls = 40
max_wall_time_seconds = 1800
"#;

    fn resolve_with_base(child: &str, id: &str) -> DslResult<ResolvedAgentDefinition> {
        let base = parse_toml(PATCH_BASE)?;
        let child = parse_toml(child)?;
        let id = DefinitionId::parse(id)?;
        resolve_definition(
            &id,
            &[
                (DefinitionLayer::BuiltIn, base),
                (DefinitionLayer::BuiltIn, child),
            ],
            now(),
        )
    }

    #[test]
    fn test_schema_mismatch_is_rejected() -> TestResult {
        let raw = parse_toml(
            r#"
schema = "harwness.agent/v9"
id = "harwness.agent.schema-bad@1"
version = "1.0.0"
role = "worker"
specialization = "bad"
"#,
        )?;
        let id = DefinitionId::parse("harwness.agent.schema-bad@1")?;
        match resolve_definition(&id, &[(DefinitionLayer::BuiltIn, raw)], now()) {
            Err(DslError::SchemaMismatch {
                found, location, ..
            }) => {
                assert_eq!(found, "harwness.agent/v9");
                assert_eq!(location.field_path.as_deref(), Some("schema"));
                Ok(())
            }
            other => Err(TestError::Unexpected(format!(
                "SchemaMismatch expected, got {other:?}"
            ))),
        }
    }

    #[test]
    fn test_schema_mismatch_in_base_is_rejected() -> TestResult {
        let base = parse_toml(
            r#"
schema = "harwness.mixin/v1"
id = "harwness.agent.schema-base@1"
version = "1.0.0"
role = "worker"
specialization = "base"
"#,
        )?;
        let child = parse_toml(
            r#"
schema = "harwness.agent/v1"
id = "harwness.agent.schema-child@1"
version = "1.0.0"
role = "worker"
specialization = "child"
extends = { id = "harwness.agent.schema-base@1" }
"#,
        )?;
        let id = DefinitionId::parse("harwness.agent.schema-child@1")?;
        let result = resolve_definition(
            &id,
            &[
                (DefinitionLayer::BuiltIn, base),
                (DefinitionLayer::BuiltIn, child),
            ],
            now(),
        );
        assert!(matches!(result, Err(DslError::SchemaMismatch { .. })));
        Ok(())
    }

    #[test]
    fn test_nested_patch_path_appends_to_nested_value() -> TestResult {
        let resolved = resolve_with_base(
            r#"
schema = "harwness.agent/v1"
id = "harwness.agent.patch-nested@1"
version = "1.0.0"
role = "worker"
specialization = "nested"
extends = { id = "harwness.agent.patch-base@1" }

[patch.context.must_include]
append = ["task.acceptance_criteria"]

[patch.context.exclude]
remove = ["secret.*"]
"#,
            "harwness.agent.patch-nested@1",
        )?;
        assert_eq!(
            resolved.config["context"]["must_include"].as_array(),
            Some(&vec![
                toml::Value::String("task.objective".to_owned()),
                toml::Value::String("task.acceptance_criteria".to_owned()),
            ])
        );
        assert_eq!(
            resolved.config["context"]["exclude"].as_array(),
            Some(&vec![])
        );
        Ok(())
    }

    #[test]
    fn test_patch_table_applies_every_operator_in_fixed_order() -> TestResult {
        let resolved = resolve_with_base(
            r#"
schema = "harwness.agent/v1"
id = "harwness.agent.patch-multi@1"
version = "1.0.0"
role = "worker"
specialization = "multi"
extends = { id = "harwness.agent.patch-base@1" }

[patch.context.must_include]
append = ["b"]
remove = ["task.objective"]
"#,
            "harwness.agent.patch-multi@1",
        )?;
        assert_eq!(
            resolved.config["context"]["must_include"].as_array(),
            Some(&vec![toml::Value::String("b".to_owned())])
        );
        Ok(())
    }

    #[test]
    fn test_patch_min_and_max_within_parent_on_nested_limits() -> TestResult {
        let resolved = resolve_with_base(
            r#"
schema = "harwness.agent/v1"
id = "harwness.agent.patch-limits@1"
version = "1.0.0"
role = "worker"
specialization = "limits"
extends = { id = "harwness.agent.patch-base@1" }

[patch.limits]
max_tool_calls = { min = 24 }
max_wall_time_seconds = { max-within-parent = 900 }
"#,
            "harwness.agent.patch-limits@1",
        )?;
        assert_eq!(
            resolved.config["limits"]["max_tool_calls"].as_integer(),
            Some(24)
        );
        assert_eq!(
            resolved.config["limits"]["max_wall_time_seconds"].as_integer(),
            Some(900)
        );
        Ok(())
    }

    #[test]
    fn test_patch_min_never_raises() -> TestResult {
        let resolved = resolve_with_base(
            r#"
schema = "harwness.agent/v1"
id = "harwness.agent.patch-min@1"
version = "1.0.0"
role = "worker"
specialization = "min"
extends = { id = "harwness.agent.patch-base@1" }

[patch.limits]
max_tool_calls = { min = 100 }
"#,
            "harwness.agent.patch-min@1",
        )?;
        assert_eq!(
            resolved.config["limits"]["max_tool_calls"].as_integer(),
            Some(40)
        );
        Ok(())
    }

    #[test]
    fn test_patch_max_within_parent_above_parent_is_an_error() -> TestResult {
        let result = resolve_with_base(
            r#"
schema = "harwness.agent/v1"
id = "harwness.agent.patch-max@1"
version = "1.0.0"
role = "worker"
specialization = "max"
extends = { id = "harwness.agent.patch-base@1" }

[patch.limits]
max_tool_calls = { max-within-parent = 41 }
"#,
            "harwness.agent.patch-max@1",
        );
        match result {
            Err(DslError::PatchExceedsParent { location, .. }) => {
                assert_eq!(
                    location.field_path.as_deref(),
                    Some("patch.limits.max_tool_calls")
                );
                Ok(())
            }
            other => Err(TestError::Unexpected(format!(
                "PatchExceedsParent expected, got {other:?}"
            ))),
        }
    }

    #[test]
    fn test_patch_nested_path_that_names_no_field_is_an_error() -> TestResult {
        let result = resolve_with_base(
            r#"
schema = "harwness.agent/v1"
id = "harwness.agent.patch-missing@1"
version = "1.0.0"
role = "worker"
specialization = "missing"
extends = { id = "harwness.agent.patch-base@1" }

[patch.context.nope]
append = ["x"]
"#,
            "harwness.agent.patch-missing@1",
        );
        assert!(
            matches!(result, Err(DslError::PatchPathMissing { ref path, .. }) if path == "context.nope"),
            "{result:?}"
        );
        Ok(())
    }

    #[test]
    fn test_patch_replace_may_introduce_a_nested_field() -> TestResult {
        let resolved = resolve_with_base(
            r#"
schema = "harwness.agent/v1"
id = "harwness.agent.patch-introduce@1"
version = "1.0.0"
role = "worker"
specialization = "introduce"
extends = { id = "harwness.agent.patch-base@1" }

[patch.context.policy]
replace = "harwness.context.custom@1"
"#,
            "harwness.agent.patch-introduce@1",
        )?;
        assert_eq!(
            resolved.config["context"]["policy"].as_str(),
            Some("harwness.context.custom@1")
        );
        Ok(())
    }

    #[test]
    fn test_patch_mixing_operators_and_nested_keys_is_an_error() -> TestResult {
        let result = resolve_with_base(
            r#"
schema = "harwness.agent/v1"
id = "harwness.agent.patch-mixed@1"
version = "1.0.0"
role = "worker"
specialization = "mixed"
extends = { id = "harwness.agent.patch-base@1" }

[patch.context]
replace = {}
must_include = { append = ["x"] }
"#,
            "harwness.agent.patch-mixed@1",
        );
        assert!(
            matches!(result, Err(DslError::PatchAmbiguous { .. })),
            "{result:?}"
        );
        Ok(())
    }

    #[test]
    fn test_patch_array_operator_with_non_array_value_is_an_error() -> TestResult {
        let result = resolve_with_base(
            r#"
schema = "harwness.agent/v1"
id = "harwness.agent.patch-scalar@1"
version = "1.0.0"
role = "worker"
specialization = "scalar"
extends = { id = "harwness.agent.patch-base@1" }

[patch.context.must_include]
append = "x"
"#,
            "harwness.agent.patch-scalar@1",
        );
        assert!(
            matches!(result, Err(DslError::PatchTypeMismatch { .. })),
            "{result:?}"
        );
        Ok(())
    }

    #[test]
    fn test_authority_remove_is_allowed_and_replace_is_forbidden() -> TestResult {
        let base = r#"
schema = "harwness.agent/v1"
id = "harwness.agent.auth-base@1"
version = "1.0.0"
role = "worker"
specialization = "base"

[authority]
capabilities = ["filesystem.read", "filesystem.write"]
"#;
        let removing = r#"
schema = "harwness.agent/v1"
id = "harwness.agent.auth-remove@1"
version = "1.0.0"
role = "worker"
specialization = "remove"
extends = { id = "harwness.agent.auth-base@1" }

[patch.authority.capabilities]
remove = ["filesystem.write"]
"#;
        let id = DefinitionId::parse("harwness.agent.auth-remove@1")?;
        let resolved = resolve_definition(
            &id,
            &[
                (DefinitionLayer::BuiltIn, parse_toml(base)?),
                (DefinitionLayer::BuiltIn, parse_toml(removing)?),
            ],
            now(),
        )?;
        assert_eq!(resolved.authority.capabilities, vec!["filesystem.read"]);

        let replacing = removing
            .replace("auth-remove", "auth-replace")
            .replace("remove = [", "replace = [");
        let id = DefinitionId::parse("harwness.agent.auth-replace@1")?;
        let result = resolve_definition(
            &id,
            &[
                (DefinitionLayer::BuiltIn, parse_toml(base)?),
                (DefinitionLayer::BuiltIn, parse_toml(&replacing)?),
            ],
            now(),
        );
        assert!(
            matches!(result, Err(DslError::AuthorityOpForbidden { .. })),
            "{result:?}"
        );
        Ok(())
    }
}
