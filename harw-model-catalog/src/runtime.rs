//! Layer 3 — `ModelRuntimeProfile`: Runtime-Betriebsregeln des Harness pro Modell.
//!
//! # Verantwortung
//! Dieses Modul beschreibt **wie** das Harness ein konkretes Modell betreiben soll,
//! unabhängig von den Fähigkeiten, die der Provider behauptet (Layer 2).
//! Die hier kodierten Policies sind Runtime-Gesetz; Provider-Metadaten sind nur Hinweise.
//!
//! # Schlüsseltypen
//! - [`TaskShape`] — bevorzugte Aufgabengranularität
//! - [`ContextPolicy`] — Kontextfensterstrategie
//! - [`CompactionPolicy`] — Wann das Harness kompaktiert
//! - [`DelegationPolicy`] — Erlaubter Delegationsgrad
//! - [`RetryPolicy`] — Wiederholungsparameter
//! - [`ModelRuntimeProfile`] — Zusammenfassung aller Policies für ein Modell
//! - [`DEFAULT_PROFILE`] — Konservativer Fallback-Wert
//! - [`profile_for`] — Lookup-Funktion
//!
//! # Grundlage
//! Gemäß `philosophy.md §12`: Untrusted Provider-Metadaten dürfen Authority niemals erweitern.
//! Gemäß `philosophy.md §2`: Modellheterogenität ist eine Ressource, die das Harness
//! gezielt ausnutzen muss — unterschiedliche Modelle bekommen unterschiedliche Profiles.
//!
//! # Nebenläufigkeit
//! Alle Typen sind `Copy` und `Send + Sync`. Kein innerer Zustand, kein Locking.
//!
//! # Fehler
//! Keine eigenen Fehlertypen; `profile_for` gibt immer einen validen Wert zurück.
//!
//! # Beispiele
//! ```rust,no_run
//! use harw_model_catalog::runtime::{profile_for, DEFAULT_PROFILE, DelegationPolicy};
//!
//! let profile = profile_for("deepseek-reasoner");
//! assert_eq!(profile.delegation_policy, DelegationPolicy::Forbidden);
//!
//! let fallback = profile_for("unknown-model");
//! assert_eq!(fallback, DEFAULT_PROFILE);
//! ```
//!
//! Quelle: `docs/design/model-catalog-v2.md §4`.

pub use crate::error::RuntimeProfileValidationError;
use serde::{Deserialize, Deserializer, Serialize};

/// Obergrenze für Wiederholungsversuche in einem Runtime-Profil.
pub const MAX_RETRIES: u8 = 10;
/// Obergrenze für lineares Retry-Backoff in Millisekunden.
pub const MAX_BACKOFF_MS: u32 = 60_000;
/// Obergrenze für gleichzeitig aufrufbare Tools in einem Turn.
pub const MAX_PARALLEL_TOOLS: u8 = 16;
/// Obergrenze für direkt gespawnte Child-Agenten.
pub const MAX_CHILD_FANOUT: u8 = 16;

/// Bevorzugte Aufgabengröße / Granularität für ein Modell.
///
/// # Beschreibung
/// Steuert, welche Arbeitsmenge das Harness einem Modell zuweist.
/// Kleine Modelle erhalten enge Micro/Small-Slices; große Modelle können
/// repository-weite Aufgaben übernehmen.
///
/// Gemäß `philosophy.md §2`: Unterschiedliche Modelle unterscheiden sich in bevorzugter
/// Aufgabengranularität — das Harness muss diese Ressource gezielt nutzen.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskShape {
    /// Sehr kleine, eng begrenzte Aufgaben (< 1 Datei, < 50 LOC).
    Micro,
    /// Kleine, fokussierte Aufgaben (1-2 Dateien, begrenzte Logik).
    Small,
    /// Mittlere Aufgaben (3-5 Dateien, moderate Komplexität).
    Medium,
    /// Große Aufgaben (mehrere Module, hohe Komplexität).
    Large,
    /// Repository-weite Aufgaben (viele Dateien, Langzeitkontext nötig).
    RepositoryScale,
}

/// Kontextverwaltungsstrategie des Harness für ein Modell.
///
/// # Beschreibung
/// Bestimmt, wie das Harness das Kontextfenster befüllt und verwaltet.
/// Gemäß `philosophy.md §2`: Modelle mit großem Kontextfenster erhalten `BroadContext`;
/// Modelle, die bei langem Kontext Fokus verlieren, erhalten `TightSelect`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextPolicy {
    /// Aggressive Selektion, häufige Kompaktierung, kleine Turn-Pakete.
    /// Für Modelle, die bei langen Verläufen den Fokus verlieren.
    TightSelect,
    /// Ausgewogenes Fenster mit thematischem Bündeln.
    /// Für allgemeine Modelle ohne besondere Kontextstärke oder -schwäche.
    Balanced,
    /// Große kohärente Fenster, seltene Kompaktierung.
    /// Für Modelle mit nachgewiesener Long-Context-Stärke (z. B. 1M-Token-Fenster).
    BroadContext,
}

/// Wann das Harness den Kontext kompaktiert.
///
/// # Beschreibung
/// Steuert die Kompaktierungsstrategie. `Never` ist ausschließlich für
/// Reasoning-Modelle gedacht, die keinen kompaktierten Kontext verarbeiten können.
/// Gemäß `philosophy.md §2`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CompactionPolicy {
    /// Niemals kompaktieren (nur für Reasoning-Modelle wie deepseek-reasoner).
    Never,
    /// Kompaktieren wenn Kontextdruck entsteht (Standard).
    OnPressure,
    /// Periodisch kompaktieren, unabhängig von Kontextdruck.
    Periodic,
    /// Aggressiv und häufig kompaktieren (für Modelle mit geringer Kompaktierungstoleranz).
    Aggressive,
}

/// Erlaubter Grad der Aufgabendelegation an Child-Agenten.
///
/// # Beschreibung
/// Steuert, ob und wie stark ein Modell Unteraufgaben delegieren darf.
/// `Forbidden` für Modelle in Reasoning-Mode, `Bold` für nachweislich agentische Modelle.
/// Gemäß `philosophy.md §12`: Authority darf durch Delegation niemals wachsen.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DelegationPolicy {
    /// Delegation vollständig verboten (z. B. Reasoning-Modelle).
    Forbidden,
    /// Sehr sparsame Delegation, nur wenn unbedingt nötig.
    Cautious,
    /// Standarddelegation für allgemeine Modelle.
    Standard,
    /// Agentische Delegation erlaubt; Modell kann breiter orchestrieren.
    Bold,
}

/// Retry-Konfiguration für ein Modell.
///
/// # Beschreibung
/// Legt fest, wie oft das Harness einen fehlgeschlagenen Aufruf wiederholt,
/// wie lange es zwischen Versuchen wartet, und ob Tool-Fehler ebenfalls
/// Retry-Kandidaten sind.
///
/// Gemäß `philosophy.md §2`: Modelle mit unterschiedlicher Zuverlässigkeit
/// erhalten unterschiedliche Retry-Policies.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct RetryPolicy {
    /// Maximale Anzahl von Wiederholungsversuchen nach dem Erstversuch.
    pub max_retries: u8,
    /// Wartezeit in Millisekunden zwischen Versuchen (lineares Backoff).
    pub backoff_ms: u32,
    /// Wenn `true`, wird auch bei Tool-Fehlern (nicht nur API-Fehlern) wiederholt.
    pub retry_on_tool_error: bool,
}

impl RetryPolicy {
    /// Erstellt eine validierte Retry-Konfiguration.
    pub const fn try_new(
        max_retries: u8,
        backoff_ms: u32,
        retry_on_tool_error: bool,
    ) -> Result<Self, RuntimeProfileValidationError> {
        let policy = Self {
            max_retries,
            backoff_ms,
            retry_on_tool_error,
        };
        match policy.validate() {
            Ok(()) => {}
            Err(error) => return Err(error),
        }
        Ok(policy)
    }

    /// Prüft die numerischen Retry-Grenzen.
    pub const fn validate(&self) -> Result<(), RuntimeProfileValidationError> {
        if self.max_retries == 0 {
            return Err(RuntimeProfileValidationError::ZeroRetryLimit);
        }
        if self.max_retries > MAX_RETRIES {
            return Err(RuntimeProfileValidationError::RetryLimitExceedsMaximum {
                actual: self.max_retries,
                maximum: MAX_RETRIES,
            });
        }
        if self.backoff_ms == 0 {
            return Err(RuntimeProfileValidationError::ZeroRetryBackoff);
        }
        if self.backoff_ms > MAX_BACKOFF_MS {
            return Err(RuntimeProfileValidationError::RetryBackoffExceedsMaximum {
                actual: self.backoff_ms,
                maximum: MAX_BACKOFF_MS,
            });
        }
        Ok(())
    }
}

impl<'de> Deserialize<'de> for RetryPolicy {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct RawRetryPolicy {
            max_retries: u8,
            backoff_ms: u32,
            retry_on_tool_error: bool,
        }

        let raw = RawRetryPolicy::deserialize(deserializer)?;
        Self::try_new(raw.max_retries, raw.backoff_ms, raw.retry_on_tool_error)
            .map_err(serde::de::Error::custom)
    }
}

/// Vollständiges Runtime-Profil für ein Modell.
///
/// # Beschreibung
/// Fasst alle Harness-seitigen Betriebsentscheidungen für ein Modell zusammen.
/// Wird von `profile_for` zurückgegeben und steuert den gesamten Lebenszyklus
/// eines Modellaufrufs: Kontext, Kompaktierung, Delegation, Retries, Parallelität.
///
/// # Nebenläufigkeit
/// `Copy + Send + Sync` — kann ohne Overhead zwischen Threads geteilt werden.
///
/// # Beispiele
/// ```rust,no_run
/// use harw_model_catalog::runtime::profile_for;
///
/// let p = profile_for("claude-opus-4-8");
/// println!("max parallel tools: {}", p.max_parallel_tools);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct ModelRuntimeProfile {
    /// Strategie zur Kontextfensterverwaltung.
    pub context_policy: ContextPolicy,
    /// Wann das Harness kompaktiert.
    pub compaction_policy: CompactionPolicy,
    /// Erlaubter Grad der Aufgabendelegation.
    pub delegation_policy: DelegationPolicy,
    /// Retry-Konfiguration für dieses Modell.
    pub retry_policy: RetryPolicy,
    /// Bevorzugte Aufgabengröße.
    pub preferred_task_shape: TaskShape,
    /// Maximale Anzahl parallel aufrufbarer Tools in einem Turn.
    pub max_parallel_tools: u8,
    /// Maximale Anzahl direkt gespawnter Child-Agenten.
    pub max_child_fanout: u8,
}

impl ModelRuntimeProfile {
    /// Erstellt ein Runtime-Profil und erzwingt die Runtime-Grenzen.
    pub const fn try_new(
        context_policy: ContextPolicy,
        compaction_policy: CompactionPolicy,
        delegation_policy: DelegationPolicy,
        retry_policy: RetryPolicy,
        preferred_task_shape: TaskShape,
        max_parallel_tools: u8,
        max_child_fanout: u8,
    ) -> Result<Self, RuntimeProfileValidationError> {
        let profile = Self {
            context_policy,
            compaction_policy,
            delegation_policy,
            retry_policy,
            preferred_task_shape,
            max_parallel_tools,
            max_child_fanout,
        };
        match profile.validate() {
            Ok(()) => {}
            Err(error) => return Err(error),
        }
        Ok(profile)
    }

    /// Prüft alle Grenzen und Policy-Kombinationen des Profils.
    pub const fn validate(&self) -> Result<(), RuntimeProfileValidationError> {
        match self.retry_policy.validate() {
            Ok(()) => {}
            Err(error) => return Err(error),
        }

        if self.max_parallel_tools == 0 {
            return Err(RuntimeProfileValidationError::ZeroParallelToolLimit);
        }
        if self.max_parallel_tools > MAX_PARALLEL_TOOLS {
            return Err(
                RuntimeProfileValidationError::ParallelToolLimitExceedsMaximum {
                    actual: self.max_parallel_tools,
                    maximum: MAX_PARALLEL_TOOLS,
                },
            );
        }
        if self.max_child_fanout > MAX_CHILD_FANOUT {
            return Err(RuntimeProfileValidationError::ChildFanoutExceedsMaximum {
                actual: self.max_child_fanout,
                maximum: MAX_CHILD_FANOUT,
            });
        }
        match self.delegation_policy {
            DelegationPolicy::Forbidden if self.max_child_fanout != 0 => Err(
                RuntimeProfileValidationError::ForbiddenDelegationAllowsChildren {
                    actual: self.max_child_fanout,
                },
            ),
            DelegationPolicy::Forbidden => Ok(()),
            policy if self.max_child_fanout == 0 => {
                Err(RuntimeProfileValidationError::DelegationPolicyHasNoChildSlots { policy })
            }
            _ => Ok(()),
        }
    }
}

impl<'de> Deserialize<'de> for ModelRuntimeProfile {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct RawModelRuntimeProfile {
            context_policy: ContextPolicy,
            compaction_policy: CompactionPolicy,
            delegation_policy: DelegationPolicy,
            retry_policy: RetryPolicy,
            preferred_task_shape: TaskShape,
            max_parallel_tools: u8,
            max_child_fanout: u8,
        }

        let raw = RawModelRuntimeProfile::deserialize(deserializer)?;
        Self::try_new(
            raw.context_policy,
            raw.compaction_policy,
            raw.delegation_policy,
            raw.retry_policy,
            raw.preferred_task_shape,
            raw.max_parallel_tools,
            raw.max_child_fanout,
        )
        .map_err(serde::de::Error::custom)
    }
}

/// Konservativer Standard-Profile, wenn kein spezifisches Profil bekannt ist.
///
/// # Beschreibung
/// Wird von `profile_for` zurückgegeben, wenn die Modell-ID nicht in der
/// kuratierten Liste ist. Bewusst restriktiv: kleines Fenster, wenig Delegation,
/// wenige Retries — gemäß `philosophy.md §12` (Authority niemals erweitern durch
/// unbekannte Inputs).
///
/// # Werte
/// - `TightSelect`: unbekannte Modelle können Fokus verlieren → eng halten.
/// - `OnPressure`: Standard-Kompaktierung, sicher für alle Modelle.
/// - `Cautious`: unbekannte Modelle dürfen nur sparsam delegieren.
/// - Retry 2/500ms/false: konservativ, kein aggressives Retry-Storm-Risiko.
/// - `Small`: unbekannte Granularität → kleine Slice-Größe.
/// - max_parallel_tools 2: enge Parallelität für unbekannte Modelle.
/// - max_child_fanout 2: minimale Delegation für unbekannte Modelle.
pub const DEFAULT_PROFILE: ModelRuntimeProfile = ModelRuntimeProfile {
    context_policy: ContextPolicy::TightSelect,
    compaction_policy: CompactionPolicy::OnPressure,
    delegation_policy: DelegationPolicy::Cautious,
    retry_policy: RetryPolicy {
        max_retries: 2,
        backoff_ms: 500,
        retry_on_tool_error: false,
    },
    preferred_task_shape: TaskShape::Small,
    max_parallel_tools: 2,
    max_child_fanout: 2,
};

/// Gibt das kuratierte Runtime-Profil für die angegebene Modell-ID zurück.
///
/// # Beschreibung
/// Lookup-Funktion über eine hardcodierte Liste von 15 kuratierten Modellen
/// (dieselbe Menge wie `bootstrap_descriptors` in `descriptor.rs`).
/// Alle Profil-Werte sind durch `philosophy.md §2` begründet:
/// unterschiedliche Modelle erhalten unterschiedliche Runtime-Konfigurationen,
/// die ihre tatsächlichen Stärken und Schwächen widerspiegeln.
///
/// # Arguments
/// - `model` (`&str`): Modell-ID wie in `providers.toml` verwendet.
///
/// # Returns
/// Ein `ModelRuntimeProfile`. Wenn `model` nicht in der kuratierten Liste ist,
/// wird [`DEFAULT_PROFILE`] zurückgegeben.
///
/// # Panics
/// Keine. Diese Funktion ist immer sicher aufzurufen.
///
/// # Concurrency
/// Rein funktional, kein Zustand. Thread-safe ohne Einschränkungen.
///
/// # Examples
/// ```rust,no_run
/// use harw_model_catalog::runtime::{profile_for, DelegationPolicy};
///
/// assert_eq!(
///     profile_for("deepseek-reasoner").delegation_policy,
///     DelegationPolicy::Forbidden
/// );
/// assert_eq!(profile_for("does-not-exist"), harw_model_catalog::runtime::DEFAULT_PROFILE);
/// ```
pub fn profile_for(model: &str) -> ModelRuntimeProfile {
    match model {
        // Anthropic — Claude Opus 5.5 (Standardmodell), Fable 5.1, Opus 4.8
        // Balanced: Flagship-Orchestratoren; Opus 5.5 und Fable 5.1 erben das
        // Opus-4.8-Profil, bis Beobachtungsdaten (Layer 4) ein eigenes rechtfertigen.
        // Standard delegation: vertrauenswürdiges Modell für Orchestration (philosophy.md §2).
        // retry 3/500ms/false: stabil, kein aggressives Tool-Retry nötig.
        "claude-opus-5-5" | "claude-fable-5-1" | "claude-opus-4-8" => ModelRuntimeProfile {
            context_policy: ContextPolicy::Balanced,
            compaction_policy: CompactionPolicy::OnPressure,
            delegation_policy: DelegationPolicy::Standard,
            retry_policy: RetryPolicy {
                max_retries: 3,
                backoff_ms: 500,
                retry_on_tool_error: false,
            },
            preferred_task_shape: TaskShape::Medium,
            max_parallel_tools: 4,
            max_child_fanout: 4,
        },

        // Anthropic — Claude Sonnet 5
        // BroadContext: starkes Long-Context-Modell, verarbeitet große Slices stabil.
        // Standard delegation: zuverlässig für Worker-Rollen (philosophy.md §2).
        // retry 3/500ms/false: analoger Backoff zu Opus.
        "claude-sonnet-5" => ModelRuntimeProfile {
            context_policy: ContextPolicy::BroadContext,
            compaction_policy: CompactionPolicy::OnPressure,
            delegation_policy: DelegationPolicy::Standard,
            retry_policy: RetryPolicy {
                max_retries: 3,
                backoff_ms: 500,
                retry_on_tool_error: false,
            },
            preferred_task_shape: TaskShape::Large,
            max_parallel_tools: 4,
            max_child_fanout: 4,
        },

        // Anthropic — Claude Haiku 4.5
        // TightSelect: schnelles, leichtgewichtiges Modell, kein langer Kontext geeignet.
        // Aggressive compaction: Haiku verliert Fokus bei langen Verläufen (philosophy.md §2).
        // Cautious: nicht für Orchestration — einfache, fokussierte Aufgaben.
        // retry 2/300ms/true: aggressives Tool-Retry, da Haiku häufiger Tool-Fehler macht.
        "claude-haiku-4-5-20251001" => ModelRuntimeProfile {
            context_policy: ContextPolicy::TightSelect,
            compaction_policy: CompactionPolicy::Aggressive,
            delegation_policy: DelegationPolicy::Cautious,
            retry_policy: RetryPolicy {
                max_retries: 2,
                backoff_ms: 300,
                retry_on_tool_error: true,
            },
            preferred_task_shape: TaskShape::Small,
            max_parallel_tools: 3,
            max_child_fanout: 3,
        },

        // OpenAI — GPT-5
        // BroadContext: GPT-5 hat starkes Long-Context und Reasoning (philosophy.md §2).
        // Standard: zuverlässig für Orchestration-Rollen.
        // retry 3/500ms/false: analoger Backoff zu Claude Opus.
        "gpt-5" => ModelRuntimeProfile {
            context_policy: ContextPolicy::BroadContext,
            compaction_policy: CompactionPolicy::OnPressure,
            delegation_policy: DelegationPolicy::Standard,
            retry_policy: RetryPolicy {
                max_retries: 3,
                backoff_ms: 500,
                retry_on_tool_error: false,
            },
            preferred_task_shape: TaskShape::Large,
            max_parallel_tools: 4,
            max_child_fanout: 4,
        },

        // OpenAI — GPT-4o
        // Balanced: bewährtes Allround-Modell, stabiler Kontext ohne 1M-Fenster.
        // Standard delegation: bekannte Zuverlässigkeit (philosophy.md §2).
        // max_child_fanout 3: leicht eingeschränkt gegenüber GPT-5.
        "gpt-4o" => ModelRuntimeProfile {
            context_policy: ContextPolicy::Balanced,
            compaction_policy: CompactionPolicy::OnPressure,
            delegation_policy: DelegationPolicy::Standard,
            retry_policy: RetryPolicy {
                max_retries: 3,
                backoff_ms: 500,
                retry_on_tool_error: false,
            },
            preferred_task_shape: TaskShape::Medium,
            max_parallel_tools: 4,
            max_child_fanout: 3,
        },

        // OpenAI — o4-mini
        // Balanced: kompaktes Reasoning-Modell; funktioniert gut mit mittlerem Kontext.
        // Periodic: o4-mini bevorzugt regelmäßige Kompaktierung für Reasoning-Phasen.
        // max_parallel_tools 3: etwas eingeschränkt bei parallelem Tool-Calling.
        "o4-mini" => ModelRuntimeProfile {
            context_policy: ContextPolicy::Balanced,
            compaction_policy: CompactionPolicy::Periodic,
            delegation_policy: DelegationPolicy::Standard,
            retry_policy: RetryPolicy {
                max_retries: 3,
                backoff_ms: 600,
                retry_on_tool_error: false,
            },
            preferred_task_shape: TaskShape::Medium,
            max_parallel_tools: 3,
            max_child_fanout: 3,
        },

        // Z.AI — GLM-4.6
        // BroadContext: 1M-Token-Kontextfenster, explizit für Long-Horizon-Aufgaben (philosophy.md §2).
        // Periodic: großes Fenster, periodische Kompaktierung sinnvoll.
        // RepositoryScale: kann repository-weite Aufgaben ohne Fensterdruck bearbeiten.
        "glm-4.6" => ModelRuntimeProfile {
            context_policy: ContextPolicy::BroadContext,
            compaction_policy: CompactionPolicy::Periodic,
            delegation_policy: DelegationPolicy::Standard,
            retry_policy: RetryPolicy {
                max_retries: 2,
                backoff_ms: 500,
                retry_on_tool_error: false,
            },
            preferred_task_shape: TaskShape::RepositoryScale,
            max_parallel_tools: 3,
            max_child_fanout: 3,
        },

        // Moonshot — Kimi K2 Turbo Preview
        // Balanced: agentisches Modell, starkes Tool-Calling (philosophy.md §2).
        // Bold: offiziell für agentische Szenarien positioniert — breite Delegation erlaubt.
        // retry 3/400ms/true: Tool-Fehler-Retry sinnvoll für agentische Modelle.
        // max_parallel_tools 6: hohes paralleles Tool-Calling nach philosophy.md §2.
        "kimi-k2-turbo-preview" => ModelRuntimeProfile {
            context_policy: ContextPolicy::Balanced,
            compaction_policy: CompactionPolicy::OnPressure,
            delegation_policy: DelegationPolicy::Bold,
            retry_policy: RetryPolicy {
                max_retries: 3,
                backoff_ms: 400,
                retry_on_tool_error: true,
            },
            preferred_task_shape: TaskShape::Medium,
            max_parallel_tools: 6,
            max_child_fanout: 4,
        },

        // Alibaba DashScope — Qwen3-Max
        // Balanced: starkes Allround-Modell, mittlerer Kontext.
        // Standard delegation: bewährt in allgemeinen Aufgaben.
        // max_child_fanout 3: etwas eingeschränkt bei unbekannter Agentik-Reife.
        "qwen3-max" => ModelRuntimeProfile {
            context_policy: ContextPolicy::Balanced,
            compaction_policy: CompactionPolicy::OnPressure,
            delegation_policy: DelegationPolicy::Standard,
            retry_policy: RetryPolicy {
                max_retries: 3,
                backoff_ms: 500,
                retry_on_tool_error: false,
            },
            preferred_task_shape: TaskShape::Medium,
            max_parallel_tools: 4,
            max_child_fanout: 3,
        },

        // Alibaba DashScope — Qwen3-Coder-Plus
        // TightSelect: spezialisiertes Coder-Modell, enger Fokus auf Code-Slices.
        // Cautious: kein Orchestrator — reiner Coding-Worker (philosophy.md §2).
        // retry 2/400ms/true: Coder-Modelle profitieren von Tool-Retry bei Compile-Fehlern.
        "qwen3-coder-plus" => ModelRuntimeProfile {
            context_policy: ContextPolicy::TightSelect,
            compaction_policy: CompactionPolicy::OnPressure,
            delegation_policy: DelegationPolicy::Cautious,
            retry_policy: RetryPolicy {
                max_retries: 2,
                backoff_ms: 400,
                retry_on_tool_error: true,
            },
            preferred_task_shape: TaskShape::Small,
            max_parallel_tools: 4,
            max_child_fanout: 2,
        },

        // Mistral — Mistral Large 2411
        // Balanced: starkes europäisches Modell, gute Tool-Nutzung.
        // Standard delegation: zuverlässig für mittlere Orchestration.
        // max_parallel_tools/fanout 3: etwas konservativer als GPT/Claude-Flagships.
        "mistral-large-2411" => ModelRuntimeProfile {
            context_policy: ContextPolicy::Balanced,
            compaction_policy: CompactionPolicy::OnPressure,
            delegation_policy: DelegationPolicy::Standard,
            retry_policy: RetryPolicy {
                max_retries: 2,
                backoff_ms: 500,
                retry_on_tool_error: false,
            },
            preferred_task_shape: TaskShape::Medium,
            max_parallel_tools: 3,
            max_child_fanout: 3,
        },

        // xAI — Grok 4.5
        // BroadContext: 500k-Token-Fenster, agentisches Tool-Calling (philosophy.md §2).
        // Bold: agentisch positioniert, konfigurierbare Reasoning-Tiefe.
        // retry 3/500ms/false: analoger Backoff zu anderen Flagship-Modellen.
        "grok-4.5" => ModelRuntimeProfile {
            context_policy: ContextPolicy::BroadContext,
            compaction_policy: CompactionPolicy::OnPressure,
            delegation_policy: DelegationPolicy::Bold,
            retry_policy: RetryPolicy {
                max_retries: 3,
                backoff_ms: 500,
                retry_on_tool_error: false,
            },
            preferred_task_shape: TaskShape::Large,
            max_parallel_tools: 4,
            max_child_fanout: 4,
        },

        // DeepSeek — DeepSeek Chat (V3)
        // Balanced: gutes Allround-Modell, moderate Kontextstärke.
        // Cautious: keine gesicherte Agentik-Reife — konservative Delegation.
        // Small preferred: optimale Nutzung bei fokussierten Slices.
        "deepseek-chat" => ModelRuntimeProfile {
            context_policy: ContextPolicy::Balanced,
            compaction_policy: CompactionPolicy::OnPressure,
            delegation_policy: DelegationPolicy::Cautious,
            retry_policy: RetryPolicy {
                max_retries: 2,
                backoff_ms: 500,
                retry_on_tool_error: false,
            },
            preferred_task_shape: TaskShape::Small,
            max_parallel_tools: 2,
            max_child_fanout: 2,
        },

        // DeepSeek — DeepSeek Reasoner (R1)
        // TightSelect: Reasoning-Modus benötigt engen, unkompaktierten Kontext.
        // Never: Reasoning-Traces dürfen nicht kompaktiert werden (philosophy.md §2).
        // Forbidden: Reasoning-Modelle sollen keine Child-Agenten spawnen (philosophy.md §12).
        // retry 1/1000ms/false: Reasoning-Läufe sind teuer — minimale Wiederholung.
        // max_parallel_tools 1, max_child_fanout 0: vollständig sequentielle Reasoning-Pipeline.
        "deepseek-reasoner" => ModelRuntimeProfile {
            context_policy: ContextPolicy::TightSelect,
            compaction_policy: CompactionPolicy::Never,
            delegation_policy: DelegationPolicy::Forbidden,
            retry_policy: RetryPolicy {
                max_retries: 1,
                backoff_ms: 1000,
                retry_on_tool_error: false,
            },
            preferred_task_shape: TaskShape::Small,
            max_parallel_tools: 1,
            max_child_fanout: 0,
        },

        // Groq — Llama 3.3 70B Versatile
        // TightSelect: schnelles Open-Source-Modell, Fokus auf enge Slices.
        // Cautious: kein Orchestrator — Worker-Rolle mit bekannten Grenzen.
        // retry 2/400ms/true: Llama-Modelle profitieren von Tool-Retry (philosophy.md §2).
        "llama-3.3-70b-versatile" => ModelRuntimeProfile {
            context_policy: ContextPolicy::TightSelect,
            compaction_policy: CompactionPolicy::OnPressure,
            delegation_policy: DelegationPolicy::Cautious,
            retry_policy: RetryPolicy {
                max_retries: 2,
                backoff_ms: 400,
                retry_on_tool_error: true,
            },
            preferred_task_shape: TaskShape::Small,
            max_parallel_tools: 3,
            max_child_fanout: 2,
        },

        // Fallback für alle unbekannten Modelle.
        // Gemäß philosophy.md §12: Unbekannte Inputs dürfen Authority niemals erweitern.
        // Konservative Defaults schützen vor unerwarteten Modellverhalten.
        _ => DEFAULT_PROFILE,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult, ctx};

    /// Test 1: Unbekannte Modell-ID liefert DEFAULT_PROFILE.
    #[test]
    fn test_unknown_model_returns_default() {
        assert_eq!(profile_for("does-not-exist"), DEFAULT_PROFILE);
        assert_eq!(profile_for(""), DEFAULT_PROFILE);
        assert_eq!(profile_for("CLAUDE-OPUS-4-8"), DEFAULT_PROFILE); // case-sensitive
    }

    /// Test 2: Alle 15 kuratierten Modelle liefern Profile != DEFAULT_PROFILE.
    #[test]
    fn test_known_models_return_non_default() {
        let known_models = [
            "claude-opus-4-8",
            "claude-sonnet-5",
            "claude-haiku-4-5-20251001",
            "gpt-5",
            "gpt-4o",
            "o4-mini",
            "glm-4.6",
            "kimi-k2-turbo-preview",
            "qwen3-max",
            "qwen3-coder-plus",
            "mistral-large-2411",
            "grok-4.5",
            "deepseek-chat",
            "deepseek-reasoner",
            "llama-3.3-70b-versatile",
        ];
        for model in &known_models {
            let profile = profile_for(model);
            assert_ne!(
                profile, DEFAULT_PROFILE,
                "Model '{}' should not return DEFAULT_PROFILE",
                model
            );
            assert_eq!(
                profile.validate(),
                Ok(()),
                "Model '{}' must return a valid runtime profile",
                model
            );
        }
    }

    /// Test 2b: Das Anthropic-Standardmodell und Fable 5.1 teilen das
    /// Opus-Profil und fallen nicht auf DEFAULT_PROFILE zurück.
    #[test]
    fn test_current_anthropic_flagships_share_opus_profile() {
        let opus = profile_for("claude-opus-4-8");
        for model in ["claude-opus-5-5", "claude-fable-5-1"] {
            assert_ne!(profile_for(model), DEFAULT_PROFILE, "{model}");
            assert_eq!(profile_for(model), opus, "{model}");
        }
    }

    /// Test 3: DEFAULT_PROFILE entspricht exakt dem im Design-Doc angegebenen Wert.
    #[test]
    fn test_default_profile_is_conservative() {
        assert_eq!(DEFAULT_PROFILE.context_policy, ContextPolicy::TightSelect);
        assert_eq!(
            DEFAULT_PROFILE.compaction_policy,
            CompactionPolicy::OnPressure
        );
        assert_eq!(
            DEFAULT_PROFILE.delegation_policy,
            DelegationPolicy::Cautious
        );
        assert_eq!(DEFAULT_PROFILE.retry_policy.max_retries, 2);
        assert_eq!(DEFAULT_PROFILE.retry_policy.backoff_ms, 500);
        const { assert!(!DEFAULT_PROFILE.retry_policy.retry_on_tool_error) };
        assert_eq!(DEFAULT_PROFILE.preferred_task_shape, TaskShape::Small);
        assert_eq!(DEFAULT_PROFILE.max_parallel_tools, 2);
        assert_eq!(DEFAULT_PROFILE.max_child_fanout, 2);
    }

    /// Test 4: ContextPolicy::BroadContext serialisiert als "broad_context".
    #[test]
    fn test_enum_serde_snake_case() -> TestResult {
        let json = serde_json::to_string(&ContextPolicy::BroadContext)?;
        assert_eq!(json, "\"broad_context\"");

        let json = serde_json::to_string(&CompactionPolicy::OnPressure)?;
        assert_eq!(json, "\"on_pressure\"");

        let json = serde_json::to_string(&DelegationPolicy::Forbidden)?;
        assert_eq!(json, "\"forbidden\"");

        let json = serde_json::to_string(&TaskShape::RepositoryScale)?;
        assert_eq!(json, "\"repository_scale\"");
        Ok(())
    }

    /// Test 5: Serde JSON Roundtrip für ModelRuntimeProfile.
    #[test]
    fn test_profile_roundtrip() -> TestResult {
        let original = profile_for("claude-opus-4-8");
        let json = serde_json::to_string(&original)?;
        let restored: ModelRuntimeProfile = serde_json::from_str(&json)?;
        assert_eq!(original, restored);

        // Auch DEFAULT_PROFILE roundtrip testen.
        let json = serde_json::to_string(&DEFAULT_PROFILE)?;
        let restored: ModelRuntimeProfile = serde_json::from_str(&json)?;
        assert_eq!(DEFAULT_PROFILE, restored);
        Ok(())
    }

    /// Test 6: Fallible constructors reject zero and excessive runtime limits.
    #[test]
    fn test_fallible_construction_rejects_invalid_limits() -> TestResult {
        assert_eq!(
            RetryPolicy::try_new(0, 500, false),
            Err(RuntimeProfileValidationError::ZeroRetryLimit)
        );
        assert_eq!(
            RetryPolicy::try_new(MAX_RETRIES + 1, 500, false),
            Err(RuntimeProfileValidationError::RetryLimitExceedsMaximum {
                actual: MAX_RETRIES + 1,
                maximum: MAX_RETRIES,
            })
        );
        assert_eq!(
            RetryPolicy::try_new(1, 0, false),
            Err(RuntimeProfileValidationError::ZeroRetryBackoff)
        );

        let retry = RetryPolicy::try_new(1, 500, false).map_err(ctx(
            "RetryPolicy::try_new(1, 500, false) sollte gültig sein",
        ))?;
        assert_eq!(
            ModelRuntimeProfile::try_new(
                ContextPolicy::Balanced,
                CompactionPolicy::OnPressure,
                DelegationPolicy::Cautious,
                retry,
                TaskShape::Small,
                0,
                1,
            ),
            Err(RuntimeProfileValidationError::ZeroParallelToolLimit)
        );
        assert_eq!(
            ModelRuntimeProfile::try_new(
                ContextPolicy::TightSelect,
                CompactionPolicy::Never,
                DelegationPolicy::Forbidden,
                retry,
                TaskShape::Small,
                1,
                1,
            ),
            Err(RuntimeProfileValidationError::ForbiddenDelegationAllowsChildren { actual: 1 })
        );
        Ok(())
    }

    /// Test 7: Fallible constructors remain usable in stable const contexts.
    #[test]
    fn test_fallible_construction_is_const_compatible() {
        const RETRY_POLICY: Result<RetryPolicy, RuntimeProfileValidationError> =
            RetryPolicy::try_new(1, 500, false);
        const PROFILE: Result<ModelRuntimeProfile, RuntimeProfileValidationError> =
            ModelRuntimeProfile::try_new(
                ContextPolicy::Balanced,
                CompactionPolicy::OnPressure,
                DelegationPolicy::Cautious,
                RetryPolicy {
                    max_retries: 1,
                    backoff_ms: 500,
                    retry_on_tool_error: false,
                },
                TaskShape::Small,
                1,
                1,
            );

        assert_eq!(
            RETRY_POLICY,
            Ok(RetryPolicy {
                max_retries: 1,
                backoff_ms: 500,
                retry_on_tool_error: false,
            })
        );
        assert_eq!(
            PROFILE,
            Ok(ModelRuntimeProfile {
                context_policy: ContextPolicy::Balanced,
                compaction_policy: CompactionPolicy::OnPressure,
                delegation_policy: DelegationPolicy::Cautious,
                retry_policy: RetryPolicy {
                    max_retries: 1,
                    backoff_ms: 500,
                    retry_on_tool_error: false,
                },
                preferred_task_shape: TaskShape::Small,
                max_parallel_tools: 1,
                max_child_fanout: 1,
            })
        );
    }

    /// Test 8: Deserialisierung kann keine unvalidierten Runtime-Profile erzeugen.
    #[test]
    fn test_deserialization_rejects_every_invalid_runtime_profile_field() -> TestResult {
        let mut invalid = serde_json::to_value(DEFAULT_PROFILE)?;
        invalid["retry_policy"]["max_retries"] = serde_json::json!(0);
        let Err(error) = serde_json::from_value::<ModelRuntimeProfile>(invalid) else {
            return Err(TestError::Unexpected("Err erwartet (max_retries=0)".into()));
        };
        assert!(
            error
                .to_string()
                .contains("max_retries must be greater than zero")
        );

        let mut invalid = serde_json::to_value(DEFAULT_PROFILE)?;
        invalid["retry_policy"]["max_retries"] = serde_json::json!(MAX_RETRIES + 1);
        let Err(error) = serde_json::from_value::<ModelRuntimeProfile>(invalid) else {
            return Err(TestError::Unexpected(
                "Err erwartet (max_retries > MAX_RETRIES)".into(),
            ));
        };
        assert!(
            error
                .to_string()
                .contains("max_retries 11 exceeds maximum 10")
        );

        let mut invalid = serde_json::to_value(DEFAULT_PROFILE)?;
        invalid["retry_policy"]["backoff_ms"] = serde_json::json!(0);
        let Err(error) = serde_json::from_value::<ModelRuntimeProfile>(invalid) else {
            return Err(TestError::Unexpected("Err erwartet (backoff_ms=0)".into()));
        };
        assert!(
            error
                .to_string()
                .contains("backoff_ms must be greater than zero")
        );

        let mut invalid = serde_json::to_value(DEFAULT_PROFILE)?;
        invalid["retry_policy"]["backoff_ms"] = serde_json::json!(MAX_BACKOFF_MS + 1);
        let Err(error) = serde_json::from_value::<ModelRuntimeProfile>(invalid) else {
            return Err(TestError::Unexpected(
                "Err erwartet (backoff_ms > MAX_BACKOFF_MS)".into(),
            ));
        };
        assert!(
            error
                .to_string()
                .contains("backoff_ms 60001 exceeds maximum 60000")
        );

        let mut invalid = serde_json::to_value(DEFAULT_PROFILE)?;
        invalid["max_parallel_tools"] = serde_json::json!(0);
        let Err(error) = serde_json::from_value::<ModelRuntimeProfile>(invalid) else {
            return Err(TestError::Unexpected(
                "Err erwartet (max_parallel_tools=0)".into(),
            ));
        };
        assert!(
            error
                .to_string()
                .contains("max_parallel_tools must be greater than zero")
        );

        let mut invalid = serde_json::to_value(DEFAULT_PROFILE)?;
        invalid["max_parallel_tools"] = serde_json::json!(MAX_PARALLEL_TOOLS + 1);
        let Err(error) = serde_json::from_value::<ModelRuntimeProfile>(invalid) else {
            return Err(TestError::Unexpected(
                "Err erwartet (max_parallel_tools > MAX_PARALLEL_TOOLS)".into(),
            ));
        };
        assert!(
            error
                .to_string()
                .contains("max_parallel_tools 17 exceeds maximum 16")
        );

        let mut invalid = serde_json::to_value(DEFAULT_PROFILE)?;
        invalid["max_child_fanout"] = serde_json::json!(0);
        let Err(error) = serde_json::from_value::<ModelRuntimeProfile>(invalid) else {
            return Err(TestError::Unexpected(
                "Err erwartet (max_child_fanout=0)".into(),
            ));
        };
        assert!(
            error
                .to_string()
                .contains("delegation_policy Cautious requires max_child_fanout greater than zero")
        );

        let mut invalid = serde_json::to_value(DEFAULT_PROFILE)?;
        invalid["max_child_fanout"] = serde_json::json!(MAX_CHILD_FANOUT + 1);
        let Err(error) = serde_json::from_value::<ModelRuntimeProfile>(invalid) else {
            return Err(TestError::Unexpected(
                "Err erwartet (max_child_fanout > MAX_CHILD_FANOUT)".into(),
            ));
        };
        assert!(
            error
                .to_string()
                .contains("max_child_fanout 17 exceeds maximum 16")
        );

        let mut invalid = serde_json::to_value(DEFAULT_PROFILE)?;
        invalid["delegation_policy"] = serde_json::json!("forbidden");
        let Err(error) = serde_json::from_value::<ModelRuntimeProfile>(invalid) else {
            return Err(TestError::Unexpected(
                "Err erwartet (delegation_policy=forbidden, max_child_fanout=2)".into(),
            ));
        };
        assert!(
            error
                .to_string()
                .contains("delegation_policy forbidden requires max_child_fanout 0, got 2")
        );
        Ok(())
    }

    /// Test 9: RetryPolicy-Bounds für alle 15 kuratierten Modelle.
    #[test]
    fn test_retry_policy_bounds() {
        let known_models = [
            "claude-opus-4-8",
            "claude-sonnet-5",
            "claude-haiku-4-5-20251001",
            "gpt-5",
            "gpt-4o",
            "o4-mini",
            "glm-4.6",
            "kimi-k2-turbo-preview",
            "qwen3-max",
            "qwen3-coder-plus",
            "mistral-large-2411",
            "grok-4.5",
            "deepseek-chat",
            "deepseek-reasoner",
            "llama-3.3-70b-versatile",
        ];
        for model in &known_models {
            let profile = profile_for(model);
            assert!(
                profile.retry_policy.max_retries <= 10,
                "Model '{}' has max_retries {} > 10",
                model,
                profile.retry_policy.max_retries
            );
            assert!(
                profile.retry_policy.backoff_ms <= 60_000,
                "Model '{}' has backoff_ms {} > 60000",
                model,
                profile.retry_policy.backoff_ms
            );
        }
    }

    /// Test 10: deepseek-reasoner hat DelegationPolicy::Forbidden.
    #[test]
    fn test_deepseek_reasoner_forbids_delegation() {
        let profile = profile_for("deepseek-reasoner");
        assert_eq!(profile.delegation_policy, DelegationPolicy::Forbidden);
        assert_eq!(profile.compaction_policy, CompactionPolicy::Never);
        assert_eq!(profile.max_child_fanout, 0);
    }
}
