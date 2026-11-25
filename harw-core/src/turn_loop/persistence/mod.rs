//! Transcript and title-seed persistence helpers.

use super::*;

/// Persistiert das zuletzt angehängte History-Item über den `StateStore`.
///
/// Emits a `transcript.persist` debug event with `records_written = 1` when a
/// record is actually saved, or `records_written = 0` when the history is empty.
/// Never logs the content of the record (redaction-by-default).
pub(super) async fn persist_last(session: &AgentSession, store: &dyn StateStore) -> CoreResult<()> {
    if let Some(item) = session.history().last() {
        store
            .save_turn(session.id(), item)
            .await
            .map_err(|error| state_store_error(error, "persistence"))?;
        tracing::debug!(records_written = 1u64, "transcript.persist");
    } else {
        tracing::debug!(records_written = 0u64, "transcript.persist");
    }
    Ok(())
}

pub(super) async fn persist_title_seed_last(
    session: &AgentSession, store: &dyn StateStore,
) -> CoreResult<Option<harw_session_store::title_seed::TitleSeedLocation>> {
    let Some(item) = session.history().last() else { return Ok(None); };
    store.save_title_seed_item(session.id(), item).await
        .map_err(|error| state_store_error(error, "persistence"))
}

pub(super) async fn bind_title_user(
    store: &dyn StateStore, ctx: &TurnInputContext,
    user: Option<harw_session_store::title_seed::TitleSeedLocation>,
) {
    if store.attach_title_seed_user(&ctx.session_id, &ctx.turn_id, user).await.is_err() {
        tracing::warn!("title source user origin was not confirmed");
    }
}

pub(super) async fn settle_title_seed_failure(
    store: &dyn StateStore, ctx: &TurnInputContext, result: &CoreResult<TurnOutcome>,
) {
    let known_not_completed = !matches!(result,
        Ok(TurnOutcome::Completed | TurnOutcome::AwaitingApproval { .. } | TurnOutcome::AwaitingChild { .. }));
    if known_not_completed &&
        store.finish_title_seed(&ctx.session_id, &ctx.turn_id, false, None).await.is_err()
    {
        tracing::warn!("title source terminal state was not confirmed");
    }
}

pub(super) fn state_store_error(error: StateStoreError, operation_name: &str) -> CoreError {
    match error {
        StateStoreError::PoisonedMutex { operation } => CoreError::TurnRejected(format!(
            "state store {operation_name} failed during {operation}"
        )),
        StateStoreError::SessionStore(error) => CoreError::SessionStore(error),
        StateStoreError::SequenceExhausted { session } => CoreError::TurnRejected(format!(
            "state store {operation_name} failed: transcript sequence exhausted for session {}",
            session.as_str()
        )),
    }
}
