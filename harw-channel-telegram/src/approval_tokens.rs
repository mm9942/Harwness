//! Opaque, single-use approval-callback tokens.
//!
//! Telegram's `callback_query.data` is attacker-observable transport data.
//! Tokens issued here carry no approval data; the in-memory store resolves a
//! token only after it has been bound to the exact message that rendered its
//! button and the callback's sender context matches that binding.

use std::collections::HashMap;
use std::sync::Mutex;

use harw_types::PeerId;

/// A Telegram chat identifier for an approval message.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TelegramChatId(pub i64);

/// A Telegram message identifier within a [`TelegramChatId`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TelegramMessageId(pub i64);

/// An optional Telegram topic/thread identifier for an approval message.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TelegramThreadId(pub i64);

/// The trusted callback context supplied by Telegram transport after it has
/// parsed an inbound callback query.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApprovalCallbackContext {
    pub chat_id: TelegramChatId,
    pub message_id: TelegramMessageId,
    pub peer: PeerId,
    pub thread_id: Option<TelegramThreadId>,
}

impl ApprovalCallbackContext {
    #[must_use]
    pub fn new(chat_id: TelegramChatId, message_id: TelegramMessageId, peer: PeerId) -> Self {
        Self {
            chat_id,
            message_id,
            peer,
            thread_id: None,
        }
    }

    #[must_use]
    pub fn with_thread(mut self, thread_id: TelegramThreadId) -> Self {
        self.thread_id = Some(thread_id);
        self
    }
}

/// The original decision a rendered approval button correlates to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingApproval {
    pub request_id: String,
    pub decision: String,
}

#[derive(Debug)]
struct IssuedApproval {
    pending: PendingApproval,
    binding: Option<ApprovalCallbackContext>,
}

/// In-memory, single-use store mapping opaque callback tokens back to the
/// `(request_id, decision)` pair they were issued for.
///
/// A token is not redeemable through [`Self::consume_bound`] until the outbound
/// transport binds it to the chat, message, peer, and optional thread that
/// actually rendered its button. Any redemption attempt removes the token
/// before comparison, so a mismatch is terminal rather than probeable.
///
/// # Concurrency
/// `Send + Sync` via an internal [`Mutex`]; cheap to hold behind an `Arc` and
/// share across rendering and callback-ingress threads.
pub struct ApprovalTokenStore {
    tokens: Mutex<HashMap<String, IssuedApproval>>,
}

impl ApprovalTokenStore {
    #[must_use]
    pub fn new() -> Self {
        Self {
            tokens: Mutex::new(HashMap::new()),
        }
    }

    /// Issues a fresh opaque token for one `(request_id, decision)` pair.
    ///
    /// The token cannot be resolved by [`Self::consume_bound`] until
    /// [`Self::bind`] succeeds after Telegram reports the rendered message id.
    #[must_use]
    pub fn issue(&self, request_id: &str, decision: &str) -> String {
        let mut tokens = self
            .tokens
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        loop {
            let token = format!("{:032x}", rand::random::<u128>());
            if !tokens.contains_key(&token) {
                tokens.insert(
                    token.clone(),
                    IssuedApproval {
                        pending: PendingApproval {
                            request_id: request_id.to_owned(),
                            decision: decision.to_owned(),
                        },
                        binding: None,
                    },
                );
                return token;
            }
        }
    }

    /// Binds an issued token exactly once to its rendered Telegram context.
    ///
    /// Returns `false` for unknown, already-bound, or already-consumed tokens.
    /// Rebinding is forbidden so a later message cannot take over a token that
    /// was issued for an earlier rendered button.
    pub fn bind(&self, token: &str, context: ApprovalCallbackContext) -> bool {
        let mut tokens = self
            .tokens
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(issued) = tokens.get_mut(token) else {
            return false;
        };
        if issued.binding.is_some() {
            return false;
        }
        issued.binding = Some(context);
        true
    }

    /// Consumes a bound token only when the callback context exactly matches.
    ///
    /// Unknown, unbound, stale, replayed, and context-mismatched tokens return
    /// `None`. A known token is removed before matching, so every failed
    /// context-aware redemption is terminal.
    pub fn consume_bound(
        &self,
        token: &str,
        context: &ApprovalCallbackContext,
    ) -> Option<PendingApproval> {
        let issued = self
            .tokens
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(token)?;
        (issued.binding.as_ref() == Some(context)).then_some(issued.pending)
    }

    /// Legacy in-crate resolution for the pre-transport callback path.
    ///
    /// This deliberately is not public outside this crate. It resolves only
    /// tokens that have never been bound, so it cannot bypass
    /// [`Self::consume_bound`] for a rendered Telegram button. New ingress
    /// code must bind then use [`Self::consume_bound`].
    #[allow(dead_code)]
    pub(crate) fn consume(&self, token: &str) -> Option<PendingApproval> {
        let mut tokens = self
            .tokens
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if tokens
            .get(token)
            .is_some_and(|issued| issued.binding.is_some())
        {
            return None;
        }
        tokens.remove(token).map(|issued| issued.pending)
    }
}

impl Default for ApprovalTokenStore {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ApprovalCallbackContext, ApprovalTokenStore, PendingApproval, TelegramChatId,
        TelegramMessageId, TelegramThreadId,
    };
    use harw_types::PeerId;

    fn context() -> ApprovalCallbackContext {
        ApprovalCallbackContext::new(
            TelegramChatId(101),
            TelegramMessageId(202),
            PeerId::from_str("303"),
        )
        .with_thread(TelegramThreadId(404))
    }

    #[test]
    fn correctly_bound_token_consumes_once() {
        let store = ApprovalTokenStore::new();
        let token = store.issue("request-1", "approve");
        let context = context();

        assert!(store.bind(&token, context.clone()));
        assert_eq!(
            store.consume_bound(&token, &context),
            Some(PendingApproval {
                request_id: "request-1".to_owned(),
                decision: "approve".to_owned(),
            })
        );
        assert_eq!(store.consume_bound(&token, &context), None);
    }

    #[test]
    fn wrong_callback_contexts_deny_and_invalidate() {
        let cases = [
            (
                "chat",
                ApprovalCallbackContext::new(
                    TelegramChatId(999),
                    TelegramMessageId(202),
                    PeerId::from_str("303"),
                )
                .with_thread(TelegramThreadId(404)),
            ),
            (
                "message",
                ApprovalCallbackContext::new(
                    TelegramChatId(101),
                    TelegramMessageId(999),
                    PeerId::from_str("303"),
                )
                .with_thread(TelegramThreadId(404)),
            ),
            (
                "peer",
                ApprovalCallbackContext::new(
                    TelegramChatId(101),
                    TelegramMessageId(202),
                    PeerId::from_str("999"),
                )
                .with_thread(TelegramThreadId(404)),
            ),
            (
                "thread",
                ApprovalCallbackContext::new(
                    TelegramChatId(101),
                    TelegramMessageId(202),
                    PeerId::from_str("303"),
                )
                .with_thread(TelegramThreadId(999)),
            ),
        ];

        for (name, wrong_context) in cases {
            let store = ApprovalTokenStore::new();
            let token = store.issue("request-1", "approve");
            let expected = context();
            assert!(store.bind(&token, expected.clone()), "{name}");
            assert_eq!(store.consume_bound(&token, &wrong_context), None, "{name}");
            assert_eq!(store.consume_bound(&token, &expected), None, "{name}");
        }
    }

    #[test]
    fn unbound_token_rejects_and_is_invalidated() {
        let store = ApprovalTokenStore::new();
        let token = store.issue("request-1", "approve");
        let context = context();

        assert_eq!(store.consume_bound(&token, &context), None);
        assert!(!store.bind(&token, context));
    }

    #[test]
    fn legacy_consume_cannot_bypass_a_bound_token() {
        let store = ApprovalTokenStore::new();
        let token = store.issue("request-1", "approve");
        let context = context();

        assert!(store.bind(&token, context.clone()));
        assert_eq!(store.consume(&token), None);
        assert!(store.consume_bound(&token, &context).is_some());
    }

    #[test]
    fn issued_token_does_not_expose_approval_data() {
        let store = ApprovalTokenStore::new();
        let token = store.issue("sensitive-request-identifier", "sensitive-decision");

        assert_eq!(token.len(), 32);
        assert!(token.bytes().all(|byte| byte.is_ascii_hexdigit()));
        assert!(!token.contains("sensitive-request-identifier"));
        assert!(!token.contains("sensitive-decision"));
    }
}
