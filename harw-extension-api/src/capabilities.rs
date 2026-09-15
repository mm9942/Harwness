//! Capabilities — was eine Extension DARF.

use harw_catalog::AgentSuggestions;
use harw_context::ContextCeiling;
use harw_sandbox::SandboxSpec;
use harw_types::{SessionId, ToolCallId};
use jiff::Timestamp;
use std::future::Future;
use std::pin::Pin;

pub type SpawnFuture<'a> =
    Pin<Box<dyn Future<Output = Result<SessionId, AgentSpawnError>> + Send + 'a>>;

/// Handoff: Sub-Agent starten.
pub trait AgentSpawner: Send + Sync {
    /// Spawn a governed child with immutable parent-derived context.
    ///
    /// There is intentionally no context-free fallback: a spawner that does
    /// not receive this authority cannot prove that the child has inherited
    /// its parent's workspace and permissions. Implementations must validate
    /// the role and apply the sandbox before allocating a child session.
    fn spawn_child<'a>(
        &'a self,
        role: &'a str,
        input: SpawnInput,
        sandbox: SandboxSpec,
        suggestions: Option<AgentSuggestions>,
    ) -> SpawnFuture<'a>;

    /// The core calls this only after it has correlated a child result to the
    /// parent handoff. Managed spawners use it to release concurrency slots.
    /// A no-op default keeps isolated test/dummy spawners lightweight; it does
    /// not grant any authority.
    fn child_finished(&self, _child: &SessionId) {}

    /// Notify the spawner that a terminal child result has been durably written
    /// to the parent, including the result's completion timestamp. The core
    /// invokes this only after that durable write succeeds. Implementations
    /// must surface failures so the child lease is preserved for recovery.
    ///
    /// The default keeps legacy spawners compatible by delegating to
    /// [`Self::child_finished`] and reporting success.
    fn child_completed(
        &self,
        child: &SessionId,
        _completed_at: Timestamp,
    ) -> Result<(), AgentSpawnError> {
        self.child_finished(child);
        Ok(())
    }

    /// Sichtbare Delegationsziele für `parent_session_id` (Addendum F+G,
    /// Nachtrag F — Delegationsprojektion).
    ///
    /// # Description
    /// Namen registrierter Rollen, an die `parent_session_id` laut
    /// derselben zwei Prädikaten delegieren dürfte, die auch die echte
    /// Admission durchsetzt (`can_delegate_to` /
    /// `harw_core::delegation_visibility::visible_delegation_targets`). Ein
    /// Aufrufer (`harw-core/src/turn_loop.rs`) hängt daraus — nur wenn die
    /// Liste nicht leer ist — einen deterministischen Kontextblock an die
    /// Modellanfrage an.
    ///
    /// Der Default liefert immer eine leere Liste, damit bestehende
    /// [`AgentSpawner`]-Implementierungen (Tests, Dummy-Spawner) ohne
    /// Änderung weiter kompilieren; sie zeigen dann schlicht keine
    /// Delegationsziele an.
    ///
    /// # Arguments
    /// - `parent_session_id` (`&SessionId`): der delegieren wollende Agent.
    ///
    /// # Returns
    /// Exakte, sortierte registrierte Rollennamen; leer, wenn nichts
    /// sichtbar ist oder die Implementierung diese Projektion nicht anbietet.
    fn delegation_target_names(&self, _parent_session_id: &SessionId) -> Vec<String> {
        Vec::new()
    }
}

#[derive(Debug, Clone)]
pub struct SpawnInput {
    /// Trusted parent correlation established by the orchestrator. This is
    /// not model-generated child context and must be validated by a managed
    /// spawner before it creates a session.
    pub parent_session_id: SessionId,
    /// Exact parent handoff call that owns this child. It is established by
    /// the core before the spawner runs and enables zombie reconciliation to
    /// deliver an error to the correct pending handoff.
    pub handoff_call_id: ToolCallId,
    pub instructions: Option<String>,
    pub context: serde_json::Value,
    /// The context ceiling the child declares wanting, carried directly
    /// alongside the rest of this handoff — not layered on in a later,
    /// separate step.
    ///
    /// # Warum hier, nicht später
    /// A managed spawner (`harw_core::child_controller::ManagedAgentSpawner::admit`)
    /// cuts this field against the parent's own ceiling in the very same
    /// admission call that derives the child's sandbox from the parent's
    /// sandbox. If the cut happened one step later — a separate function a
    /// future caller could forget to invoke — there would be a window in
    /// which a child carries a context program whose ceiling nobody has
    /// enforced yet. That window is a silent authority gap, not a crash, and
    /// gaps like that stay undiscovered far longer than a panic would.
    ///
    /// # `None` vs. `Some`
    /// `None` means the child requests no ceiling of its own; it simply
    /// inherits whatever ceiling its parent already enforces, unchanged.
    /// This is always safe — inheriting the parent's already-cut ceiling can
    /// never widen anything. `Some(ceiling)` is a declared demand that the
    /// admitting spawner must verify is already fully contained within the
    /// parent's ceiling. A demand that is not contained must cause admission
    /// to fail closed with an error naming the exceeded aspect — it must
    /// never be silently narrowed to fit, because a silently pruned demand
    /// leaves the child running with less context than its own declaration
    /// promised, and nobody is told. That is the same failure mode as a
    /// silently dropped `must_include`.
    pub ceiling: Option<ContextCeiling>,
}

#[derive(Debug)]
pub struct AgentSpawnError {
    pub message: String,
}

impl std::fmt::Display for AgentSpawnError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "agent spawn failed: {}", self.message)
    }
}
impl std::error::Error for AgentSpawnError {}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct LegacySpawner {
        finished: AtomicUsize,
    }

    impl AgentSpawner for LegacySpawner {
        fn spawn_child<'a>(
            &'a self,
            _role: &'a str,
            _input: SpawnInput,
            _sandbox: SandboxSpec,
            _suggestions: Option<AgentSuggestions>,
        ) -> SpawnFuture<'a> {
            Box::pin(async { Ok(SessionId::new()) })
        }

        fn child_finished(&self, _child: &SessionId) {
            self.finished.fetch_add(1, Ordering::SeqCst);
        }
    }

    #[test]
    fn legacy_spawner_default_child_completed_calls_child_finished() {
        let spawner = LegacySpawner {
            finished: AtomicUsize::new(0),
        };

        AgentSpawner::child_completed(&spawner, &SessionId::new(), Timestamp::now())
            .expect("legacy completion hook should report success");

        assert_eq!(spawner.finished.load(Ordering::SeqCst), 1);
    }
}
