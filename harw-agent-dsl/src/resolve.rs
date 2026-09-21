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
//! - Mixins dürfen die Rolle nicht ändern.
//!
//! # Nebenläufigkeit
//! `resolve_definition` ist zustandslos und thread-sicher.

use time::OffsetDateTime;

use crate::authority::AuthorityCeiling;
use crate::error::{DiagLocation, DslError, DslResult};
use crate::ids::DefinitionId;
use crate::layers::DefinitionLayer;
use crate::merge::{apply_merge_op, MergeOp};
use crate::raw::RawAgentDefinition;
use crate::resolved::{ResolutionStep, ResolutionTrace, ResolvedAgentDefinition};

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
/// 6. Prüft Authority-Monotonie (child.authority ⊆ parent.authority).
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
/// - [`DslError::AuthorityElevation`]: wenn ein Patch Authority unzulässig erhöht.
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
    let target = target_layers
        .last()
        .copied()
        .expect("target_layers was checked to be non-empty");
    let authoritative_role = base_target.role;

    let mut ctx = ResolveCtx {
        target_id,
        sorted: &sorted,
        now,
        steps: Vec::new(),
        config: toml::Table::new(),
        authority: AuthorityCeiling::default(),
        reasoning_effort: None,
    };
    // 4–5. Ziel-Layer aufsteigend komponieren. Höhere Layer sind Overlays.
    for (layer_index, definition) in target_layers.iter().enumerate() {
        let mut active_extends = vec![(definition.id.clone(), definition.version.clone())];
        resolve_extends(definition, &mut ctx, &mut active_extends)?;
        apply_mixins(definition, authoritative_role, &mut ctx)?;
        apply_definition_content(
            definition,
            &mut ctx,
            layer_index > 0 || base_target.extends.is_some(),
            layer_index > 0,
        )?;
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
        config: ctx.config,
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

/// Resolves every ancestor of `definition`, from its root base to its direct base.
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
        let mixin = find_reference(ctx.sorted, mixin_ref).ok_or_else(|| DslError::MissingMixin {
            of: Box::new(ctx.target_id.clone()),
            referenced: Box::new(mixin_ref.clone()),
            location: DiagLocation::field(format!("mixins[{mixin_idx}]")),
        })?;
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
fn apply_patches(
    config: &mut toml::Table,
    authority: &mut AuthorityCeiling,
    patch: &toml::Table,
    target_id: &DefinitionId,
    parent_authority: &AuthorityCeiling,
) -> DslResult<()> {
    for (field, patch_value) in patch {
        if field == "authority" {
            // Authority-Patch: nur Intersect erlaubt
            let patch_table = match patch_value.as_table() {
                Some(t) => t,
                None => continue,
            };
            if let Some(caps_patch) = patch_table.get("capabilities") {
                let patch_caps_table = caps_patch.as_table();
                if let Some(pt) = patch_caps_table {
                    if let Some(intersect_val) = pt.get("intersect") {
                        // Intersect-Operation auf authority.capabilities
                        let intersect_caps: Vec<String> = intersect_val
                            .as_array()
                            .unwrap_or(&vec![])
                            .iter()
                            .filter_map(|v| v.as_str().map(str::to_owned))
                            .collect();

                        let intersect_ceiling = AuthorityCeiling {
                            capabilities: intersect_caps,
                        };

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
                    } else if let Some(append_val) = pt.get("append") {
                        // Append auf authority.capabilities ist verboten (§7)
                        let added: Vec<String> = append_val
                            .as_array()
                            .unwrap_or(&vec![])
                            .iter()
                            .filter_map(|v| v.as_str().map(str::to_owned))
                            .collect();
                        return Err(DslError::AuthorityElevation {
                            of: Box::new(target_id.clone()),
                            added_capabilities: added,
                            location: DiagLocation::field("authority.capabilities"),
                        });
                    }
                }
            }
        } else {
            // Normaler Patch auf config-Tabelle
            let current = config
                .entry(field.clone())
                .or_insert_with(|| toml::Value::Table(toml::Table::new()));

            // Patch-Value als Tabelle mit op-Schlüssel interpretieren
            if let Some(patch_table) = patch_value.as_table() {
                try_apply_table_op(current, patch_table)?;
            }
        }
    }
    Ok(())
}

/// Versucht, eine Tabellen-basierte Merge-Op anzuwenden (z. B. `{ append = [...] }`).
fn try_apply_table_op(current: &mut toml::Value, patch_table: &toml::Table) -> DslResult<()> {
    if let Some(val) = patch_table.get("replace") {
        let op = MergeOp::Replace { value: val.clone() };
        apply_merge_op(current, op)?;
    } else if let Some(vals) = patch_table.get("append") {
        let values = vals.as_array().cloned().unwrap_or_default();
        apply_merge_op(current, MergeOp::Append { values })?;
    } else if let Some(vals) = patch_table.get("prepend") {
        let values = vals.as_array().cloned().unwrap_or_default();
        apply_merge_op(current, MergeOp::Prepend { values })?;
    } else if let Some(vals) = patch_table.get("remove") {
        let values = vals.as_array().cloned().unwrap_or_default();
        apply_merge_op(current, MergeOp::Remove { values })?;
    } else if let Some(vals) = patch_table.get("intersect") {
        let values = vals.as_array().cloned().unwrap_or_default();
        apply_merge_op(current, MergeOp::Intersect { values })?;
    }
    Ok(())
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
    fn test_simple_no_extends() {
        let raw = parse_toml(MINIMAL_WORKER).unwrap();
        let id = DefinitionId::parse("harwness.agent.worker@1").unwrap();
        let layers = vec![(DefinitionLayer::BuiltIn, raw)];
        let resolved = resolve_definition(&id, &layers, now()).unwrap();
        assert_eq!(resolved.specialization, "test-worker");
        assert_eq!(resolved.role, crate::roles::AgentRoleId::Worker);
    }

    #[test]
    fn test_missing_base_errors() {
        let src = r#"
schema = "harwness.agent/v1"
id = "harwness.agent.my-worker@1"
version = "1.0.0"
role = "worker"
specialization = "test"
extends = { id = "harwness.agent.nonexistent-base@1" }
"#;
        let raw = parse_toml(src).unwrap();
        let id = DefinitionId::parse("harwness.agent.my-worker@1").unwrap();
        let layers = vec![(DefinitionLayer::BuiltIn, raw)];
        let result = resolve_definition(&id, &layers, now());
        assert!(matches!(result, Err(DslError::MissingBase { .. })));
    }

    #[test]
    fn test_authority_elevation_rejected() {
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
        let raw = parse_toml(src).unwrap();
        let id = DefinitionId::parse("harwness.agent.evil-worker@1").unwrap();
        let layers = vec![(DefinitionLayer::BuiltIn, raw)];
        let result = resolve_definition(&id, &layers, now());
        assert!(
            matches!(result, Err(DslError::AuthorityElevation { .. })),
            "Erwartet AuthorityElevation, erhalten: {:?}",
            result
        );
    }

    #[test]
    fn test_authority_elevation_display_contains_field_path() {
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
        let raw = parse_toml(src).unwrap();
        let id = DefinitionId::parse("harwness.agent.evil-worker@2").unwrap();
        let layers = vec![(DefinitionLayer::BuiltIn, raw)];
        let err = resolve_definition(&id, &layers, now()).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("authority.capabilities"),
            "Display should contain 'authority.capabilities', got: {msg}"
        );
    }

    #[test]
    fn test_resolve_trace_contains_base_and_mixin_steps() {
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
        let base = parse_toml(base_src).unwrap();
        let mixin = parse_toml(mixin_src).unwrap();
        let target = parse_toml(target_src).unwrap();
        let id = DefinitionId::parse("harwness.agent.rust-worker@1").unwrap();
        let layers = vec![
            (DefinitionLayer::BuiltIn, base),
            (DefinitionLayer::BuiltIn, mixin),
            (DefinitionLayer::BuiltIn, target),
        ];
        let resolved = resolve_definition(&id, &layers, now()).unwrap();
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
    }

    #[test]
    fn test_resolve_with_layer_priority() {
        // Höhere Schicht (Project) überschreibt BuiltIn
        let builtin = parse_toml(MINIMAL_WORKER).unwrap();
        let project_src = r#"
schema = "harwness.agent/v1"
id = "harwness.agent.worker@1"
version = "2.0.0"
role = "worker"
specialization = "project-override"
"#;
        let project = parse_toml(project_src).unwrap();
        let id = DefinitionId::parse("harwness.agent.worker@1").unwrap();
        let layers = vec![
            (DefinitionLayer::BuiltIn, builtin),
            (DefinitionLayer::Project, project),
        ];
        let resolved = resolve_definition(&id, &layers, now()).unwrap();
        assert_eq!(resolved.specialization, "project-override");
        assert_eq!(resolved.version.0.major, 2);
    }

    #[test]
    fn test_resolve_recursively_applies_all_extends() {
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
        )
        .unwrap();
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
        )
        .unwrap();
        let target = parse_toml(
            r#"
schema = "harwness.agent/v1"
id = "harwness.agent.recursive-target@1"
version = "1.0.0"
role = "worker"
specialization = "target"
extends = { id = "harwness.agent.middle-base@1" }
"#,
        )
        .unwrap();
        let id = DefinitionId::parse("harwness.agent.recursive-target@1").unwrap();
        let resolved = resolve_definition(
            &id,
            &[
                (DefinitionLayer::BuiltIn, root),
                (DefinitionLayer::BuiltIn, middle),
                (DefinitionLayer::BuiltIn, target),
            ],
            now(),
        )
        .unwrap();

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
    }

    #[test]
    fn test_resolve_recursively_applies_extends_from_higher_target_layer() {
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
        )
        .unwrap();
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
        )
        .unwrap();
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
        )
        .unwrap();
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
        )
        .unwrap();
        let id = DefinitionId::parse("harwness.agent.overlay-target@1").unwrap();

        let resolved = resolve_definition(
            &id,
            &[
                (DefinitionLayer::BuiltIn, root),
                (DefinitionLayer::BuiltIn, base),
                (DefinitionLayer::BuiltIn, lower_target),
                (DefinitionLayer::Project, higher_target),
            ],
            now(),
        )
        .unwrap();

        for table in ["root_only", "base_only", "lower_only", "higher_only"] {
            assert!(resolved.config.contains_key(table), "missing {table}");
        }
    }

    #[test]
    fn test_resolve_rejects_cycle_declared_by_higher_target_layer() {
        let lower_target = parse_toml(
            r#"
schema = "harwness.agent/v1"
id = "harwness.agent.overlay-cycle@1"
version = "1.0.0"
role = "worker"
specialization = "lower"
"#,
        )
        .unwrap();
        let higher_target = parse_toml(
            r#"
schema = "harwness.agent/v1"
id = "harwness.agent.overlay-cycle@1"
version = "1.1.0"
role = "worker"
specialization = "higher"
extends = { id = "harwness.agent.overlay-cycle@1" }
"#,
        )
        .unwrap();
        let id = DefinitionId::parse("harwness.agent.overlay-cycle@1").unwrap();

        let error = resolve_definition(
            &id,
            &[
                (DefinitionLayer::BuiltIn, lower_target),
                (DefinitionLayer::Project, higher_target),
            ],
            now(),
        )
        .unwrap_err();

        assert!(matches!(error, DslError::InheritanceCycle { .. }));
    }

    #[test]
    fn test_resolve_honors_explicit_reference_version() {
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
        )
        .unwrap();
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
        )
        .unwrap();
        let target = parse_toml(
            r#"
schema = "harwness.agent/v1"
id = "harwness.agent.versioned-target@1"
version = "1.0.0"
role = "worker"
specialization = "target"
extends = { id = "harwness.agent.versioned-base@1", version = "1.0.0" }
"#,
        )
        .unwrap();
        let id = DefinitionId::parse("harwness.agent.versioned-target@1").unwrap();
        let resolved = resolve_definition(
            &id,
            &[
                (DefinitionLayer::BuiltIn, base_v1),
                (DefinitionLayer::BuiltIn, base_v2),
                (DefinitionLayer::BuiltIn, target),
            ],
            now(),
        )
        .unwrap();

        assert_eq!(
            resolved.config["selected"]["version"].as_str(),
            Some("1.0.0")
        );
    }

    #[test]
    fn test_resolve_uses_newest_unpinned_reference_version_in_winning_layer() {
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
        )
        .unwrap();
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
        )
        .unwrap();
        let target = parse_toml(
            r#"
schema = "harwness.agent/v1"
id = "harwness.agent.unpinned-target@1"
version = "1.0.0"
role = "worker"
specialization = "target"
extends = { id = "harwness.agent.unpinned-base@1" }
"#,
        )
        .unwrap();
        let id = DefinitionId::parse("harwness.agent.unpinned-target@1").unwrap();

        let resolved = resolve_definition(
            &id,
            &[
                (DefinitionLayer::BuiltIn, newest_base),
                (DefinitionLayer::BuiltIn, older_base),
                (DefinitionLayer::BuiltIn, target),
            ],
            now(),
        )
        .unwrap();

        assert_eq!(
            resolved.config["selected"]["version"].as_str(),
            Some("1.2.0")
        );
    }

    #[test]
    fn test_resolve_rejects_direct_inheritance_cycle() {
        let definition = parse_toml(
            r#"
schema = "harwness.agent/v1"
id = "harwness.agent.self-cycle@1"
version = "1.0.0"
role = "worker"
specialization = "cycle"
extends = { id = "harwness.agent.self-cycle@1" }
"#,
        )
        .unwrap();
        let id = DefinitionId::parse("harwness.agent.self-cycle@1").unwrap();

        let error =
            resolve_definition(&id, &[(DefinitionLayer::BuiltIn, definition)], now()).unwrap_err();

        let DslError::InheritanceCycle { cycle, location } = error else {
            panic!("expected InheritanceCycle");
        };
        assert_eq!(cycle, vec![id.clone(), id]);
        assert_eq!(location.field_path.as_deref(), Some("extends"));
    }

    #[test]
    fn test_resolve_rejects_indirect_inheritance_cycle_with_closed_path() {
        let first = parse_toml(
            r#"
schema = "harwness.agent/v1"
id = "harwness.agent.cycle-a@1"
version = "1.0.0"
role = "worker"
specialization = "a"
extends = { id = "harwness.agent.cycle-b@1" }
"#,
        )
        .unwrap();
        let second = parse_toml(
            r#"
schema = "harwness.agent/v1"
id = "harwness.agent.cycle-b@1"
version = "1.0.0"
role = "worker"
specialization = "b"
extends = { id = "harwness.agent.cycle-c@1" }
"#,
        )
        .unwrap();
        let third = parse_toml(
            r#"
schema = "harwness.agent/v1"
id = "harwness.agent.cycle-c@1"
version = "1.0.0"
role = "worker"
specialization = "c"
extends = { id = "harwness.agent.cycle-b@1" }
"#,
        )
        .unwrap();
        let id = DefinitionId::parse("harwness.agent.cycle-a@1").unwrap();

        let error = resolve_definition(
            &id,
            &[
                (DefinitionLayer::BuiltIn, first),
                (DefinitionLayer::BuiltIn, second),
                (DefinitionLayer::BuiltIn, third),
            ],
            now(),
        )
        .unwrap_err();

        let DslError::InheritanceCycle { cycle, location } = error else {
            panic!("expected InheritanceCycle");
        };
        assert_eq!(
            cycle,
            vec![
                DefinitionId::parse("harwness.agent.cycle-b@1").unwrap(),
                DefinitionId::parse("harwness.agent.cycle-c@1").unwrap(),
                DefinitionId::parse("harwness.agent.cycle-b@1").unwrap(),
            ]
        );
        assert_eq!(location.field_path.as_deref(), Some("extends"));
    }

    #[test]
    fn test_resolve_includes_target_direct_authority() {
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
        )
        .unwrap();
        let id = DefinitionId::parse("harwness.agent.authority-target@1").unwrap();

        let resolved =
            resolve_definition(&id, &[(DefinitionLayer::BuiltIn, definition)], now()).unwrap();

        assert_eq!(
            resolved.authority.capabilities,
            vec!["filesystem.read", "process.spawn.sandboxed"]
        );
    }

    #[test]
    fn test_resolve_rejects_direct_authority_elevation_over_base() {
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
        )
        .unwrap();
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
        )
        .unwrap();
        let id = DefinitionId::parse("harwness.agent.authority-child@1").unwrap();

        let error = resolve_definition(
            &id,
            &[
                (DefinitionLayer::BuiltIn, base),
                (DefinitionLayer::BuiltIn, target),
            ],
            now(),
        )
        .unwrap_err();

        let DslError::AuthorityElevation {
            of,
            added_capabilities,
            location,
        } = error
        else {
            panic!("expected AuthorityElevation");
        };
        assert_eq!(*of, id);
        assert_eq!(added_capabilities, vec!["filesystem.write"]);
        assert_eq!(
            location.field_path.as_deref(),
            Some("authority.capabilities")
        );
    }

    #[test]
    fn test_resolve_validates_role_of_mixin_declared_by_base() {
        let mixin = parse_toml(
            r#"
schema = "harwness.mixin/v1"
id = "harwness.mixin.incompatible-base-mixin@1"
version = "1.0.0"
role = "root-orchestrator"
specialization = "mixin"
"#,
        )
        .unwrap();
        let base = parse_toml(
            r#"
schema = "harwness.agent/v1"
id = "harwness.agent.mixin-role-base@1"
version = "1.0.0"
role = "worker"
specialization = "base"
mixins = [{ id = "harwness.mixin.incompatible-base-mixin@1" }]
"#,
        )
        .unwrap();
        let target = parse_toml(
            r#"
schema = "harwness.agent/v1"
id = "harwness.agent.mixin-role-target@1"
version = "1.0.0"
role = "worker"
specialization = "target"
extends = { id = "harwness.agent.mixin-role-base@1" }
"#,
        )
        .unwrap();
        let id = DefinitionId::parse("harwness.agent.mixin-role-target@1").unwrap();

        let error = resolve_definition(
            &id,
            &[
                (DefinitionLayer::BuiltIn, mixin),
                (DefinitionLayer::BuiltIn, base),
                (DefinitionLayer::BuiltIn, target),
            ],
            now(),
        )
        .unwrap_err();

        assert!(matches!(error, DslError::IllegalRoleForMixin { .. }));
    }

    #[test]
    fn test_resolve_composes_lower_layer_tables_with_highest_scalar_overrides() {
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
        )
        .unwrap();
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
        )
        .unwrap();
        let id = DefinitionId::parse("harwness.agent.layered@1").unwrap();

        let resolved = resolve_definition(
            &id,
            &[
                (DefinitionLayer::Project, higher),
                (DefinitionLayer::BuiltIn, lower),
            ],
            now(),
        )
        .unwrap();

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
    }

    #[test]
    fn test_resolve_rejects_authority_elevation_in_higher_layer() {
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
        )
        .unwrap();
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
        )
        .unwrap();
        let id = DefinitionId::parse("harwness.agent.layered-authority@1").unwrap();

        let error = resolve_definition(
            &id,
            &[
                (DefinitionLayer::BuiltIn, lower),
                (DefinitionLayer::Project, higher),
            ],
            now(),
        )
        .unwrap_err();

        let DslError::AuthorityElevation {
            added_capabilities,
            location,
            ..
        } = error
        else {
            panic!("expected AuthorityElevation");
        };
        assert_eq!(added_capabilities, vec!["filesystem.write"]);
        assert_eq!(
            location.field_path.as_deref(),
            Some("authority.capabilities")
        );
    }

    // -----------------------------------------------------------------------
    // reasoning_effort: Vererbung über extends/mixins/Schichten
    // (Rollen-Konflikt-Rangfolge: Provider > Modell > Agent > Rolle — dieser
    // Knoten liefert nur die Agent-Ebene; siehe guard_wiring::resolve_default_reasoning_effort)
    // -----------------------------------------------------------------------

    #[test]
    fn test_reasoning_effort_absent_everywhere_resolves_to_none() {
        let raw = parse_toml(MINIMAL_WORKER).unwrap();
        let id = DefinitionId::parse("harwness.agent.worker@1").unwrap();
        let layers = vec![(DefinitionLayer::BuiltIn, raw)];
        let resolved = resolve_definition(&id, &layers, now()).unwrap();
        assert!(resolved.reasoning_effort.is_none());
    }

    #[test]
    fn test_reasoning_effort_set_only_on_target_is_used() {
        let src = r#"
schema = "harwness.agent/v1"
id = "harwness.agent.effort-target@1"
version = "1.0.0"
role = "worker"
specialization = "effort-target"
reasoning_effort = "high"
"#;
        let raw = parse_toml(src).unwrap();
        let id = DefinitionId::parse("harwness.agent.effort-target@1").unwrap();
        let layers = vec![(DefinitionLayer::BuiltIn, raw)];
        let resolved = resolve_definition(&id, &layers, now()).unwrap();
        assert_eq!(resolved.reasoning_effort.as_deref(), Some("high"));
    }

    #[test]
    fn test_reasoning_effort_inherited_from_extends_base_when_target_silent() {
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
        )
        .unwrap();
        let target = parse_toml(
            r#"
schema = "harwness.agent/v1"
id = "harwness.agent.effort-child@1"
version = "1.0.0"
role = "worker"
specialization = "child"
extends = { id = "harwness.agent.effort-base@1" }
"#,
        )
        .unwrap();
        let id = DefinitionId::parse("harwness.agent.effort-child@1").unwrap();
        let layers = vec![
            (DefinitionLayer::BuiltIn, base),
            (DefinitionLayer::BuiltIn, target),
        ];
        let resolved = resolve_definition(&id, &layers, now()).unwrap();
        assert_eq!(resolved.reasoning_effort.as_deref(), Some("low"));
    }

    #[test]
    fn test_reasoning_effort_target_overrides_extends_base() {
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
        )
        .unwrap();
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
        )
        .unwrap();
        let id = DefinitionId::parse("harwness.agent.effort-child2@1").unwrap();
        let layers = vec![
            (DefinitionLayer::BuiltIn, base),
            (DefinitionLayer::BuiltIn, target),
        ];
        let resolved = resolve_definition(&id, &layers, now()).unwrap();
        assert_eq!(resolved.reasoning_effort.as_deref(), Some("high"));
    }

    #[test]
    fn test_reasoning_effort_inherited_from_mixin_when_target_silent() {
        let mixin = parse_toml(
            r#"
schema = "harwness.mixin/v1"
id = "harwness.mixin.effort-mixin@1"
version = "1.0.0"
role = "worker"
specialization = "mixin"
reasoning_effort = "medium"
"#,
        )
        .unwrap();
        let target = parse_toml(
            r#"
schema = "harwness.agent/v1"
id = "harwness.agent.effort-mixin-user@1"
version = "1.0.0"
role = "worker"
specialization = "target"
mixins = [{ id = "harwness.mixin.effort-mixin@1" }]
"#,
        )
        .unwrap();
        let id = DefinitionId::parse("harwness.agent.effort-mixin-user@1").unwrap();
        let layers = vec![
            (DefinitionLayer::BuiltIn, mixin),
            (DefinitionLayer::BuiltIn, target),
        ];
        let resolved = resolve_definition(&id, &layers, now()).unwrap();
        assert_eq!(resolved.reasoning_effort.as_deref(), Some("medium"));
    }

    #[test]
    fn test_reasoning_effort_target_overrides_mixin() {
        let mixin = parse_toml(
            r#"
schema = "harwness.mixin/v1"
id = "harwness.mixin.effort-mixin2@1"
version = "1.0.0"
role = "worker"
specialization = "mixin"
reasoning_effort = "medium"
"#,
        )
        .unwrap();
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
        )
        .unwrap();
        let id = DefinitionId::parse("harwness.agent.effort-mixin-user2@1").unwrap();
        let layers = vec![
            (DefinitionLayer::BuiltIn, mixin),
            (DefinitionLayer::BuiltIn, target),
        ];
        let resolved = resolve_definition(&id, &layers, now()).unwrap();
        assert_eq!(resolved.reasoning_effort.as_deref(), Some("high"));
    }

    #[test]
    fn test_reasoning_effort_higher_layer_overrides_lower_layer() {
        let builtin = parse_toml(
            r#"
schema = "harwness.agent/v1"
id = "harwness.agent.effort-layered@1"
version = "1.0.0"
role = "worker"
specialization = "builtin"
reasoning_effort = "low"
"#,
        )
        .unwrap();
        let project = parse_toml(
            r#"
schema = "harwness.agent/v1"
id = "harwness.agent.effort-layered@1"
version = "1.1.0"
role = "worker"
specialization = "project"
reasoning_effort = "high"
"#,
        )
        .unwrap();
        let id = DefinitionId::parse("harwness.agent.effort-layered@1").unwrap();
        let layers = vec![
            (DefinitionLayer::BuiltIn, builtin),
            (DefinitionLayer::Project, project),
        ];
        let resolved = resolve_definition(&id, &layers, now()).unwrap();
        assert_eq!(resolved.reasoning_effort.as_deref(), Some("high"));
    }

    #[test]
    fn test_reasoning_effort_higher_layer_silent_keeps_lower_layer_value() {
        let builtin = parse_toml(
            r#"
schema = "harwness.agent/v1"
id = "harwness.agent.effort-layered2@1"
version = "1.0.0"
role = "worker"
specialization = "builtin"
reasoning_effort = "low"
"#,
        )
        .unwrap();
        let project = parse_toml(
            r#"
schema = "harwness.agent/v1"
id = "harwness.agent.effort-layered2@1"
version = "1.1.0"
role = "worker"
specialization = "project"
"#,
        )
        .unwrap();
        let id = DefinitionId::parse("harwness.agent.effort-layered2@1").unwrap();
        let layers = vec![
            (DefinitionLayer::BuiltIn, builtin),
            (DefinitionLayer::Project, project),
        ];
        let resolved = resolve_definition(&id, &layers, now()).unwrap();
        assert_eq!(resolved.reasoning_effort.as_deref(), Some("low"));
    }
}
