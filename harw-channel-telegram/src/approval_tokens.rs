//! Opaque, single-use approval-callback tokens.
//!
//! Telegram's `callback_query.data` is attacker-observable transport data.
//! Tokens issued here carry no approval data; the in-memory store resolves a
//! token only after it has been bound to the exact message that rendered its
//! button and the callback's sender context matches that binding.

use std::collections::HashMap;
use std::sync::Mutex;

use harw_types::PeerId;
use jiff::{SignedDuration, Timestamp};

/// Obergrenze gleichzeitig ausstehender (ausgegebener, noch nicht eingelöster)
/// Approval-Tokens je Store. Beim Überschreiten verdrängt [`ApprovalTokenStore::issue`]
/// nach dem Entfernen abgelaufener Tokens das jeweils älteste Token, damit ein
/// Strom nie beantworteter Approval-Nachrichten den Speicher nicht unbegrenzt
/// wachsen lässt.
pub const MAX_OUTSTANDING_APPROVAL_TOKENS: usize = 4096;

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
    /// Chat, in dem die Approval-Nachricht gerendert wurde.
    pub chat_id: TelegramChatId,
    /// Nachricht, die die Schaltflächen trägt.
    pub message_id: TelegramMessageId,
    /// Konvention: die Telegram-User-ID des Genehmigers (`callback.sender.id`),
    /// nicht die Chat-ID — in Gruppen darf nur der gebundene Nutzer einlösen.
    pub peer: PeerId,
    /// Forum-Thema (`message_thread_id`), falls vorhanden.
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
    /// Ablaufzeitpunkt; `None` bei einem Store ohne TTL ([`ApprovalTokenStore::new`]).
    expires_at: Option<Timestamp>,
    /// Monoton steigende Ausgabereihenfolge für die Verdrängung des ältesten
    /// Tokens bei Erreichen von [`MAX_OUTSTANDING_APPROVAL_TOKENS`].
    sequence: u64,
}

impl IssuedApproval {
    fn is_expired(&self, now: Timestamp) -> bool {
        self.expires_at.is_some_and(|expires_at| now >= expires_at)
    }
}

#[derive(Debug, Default)]
struct TokenTable {
    entries: HashMap<String, IssuedApproval>,
    next_sequence: u64,
}

impl TokenTable {
    fn purge_expired(&mut self, now: Timestamp) -> usize {
        let before = self.entries.len();
        self.entries.retain(|_, issued| !issued.is_expired(now));
        before - self.entries.len()
    }

    fn evict_oldest(&mut self) {
        let oldest = self
            .entries
            .iter()
            .min_by_key(|(_, issued)| issued.sequence)
            .map(|(token, _)| token.clone());
        if let Some(token) = oldest {
            self.entries.remove(&token);
        }
    }
}

/// In-memory, single-use store mapping opaque callback tokens back to the
/// `(request_id, decision)` pair they were issued for.
///
/// A token is not redeemable through [`Self::consume_bound_at`] until the
/// outbound transport binds it to the chat, message, peer, and optional thread
/// that actually rendered its button. Any redemption attempt removes the token
/// before comparison, so a mismatch is terminal rather than probeable.
///
/// Tokens eines mit [`Self::with_ttl`] erzeugten Stores laufen nach der TTL ab;
/// abgelaufene Tokens sind nicht mehr einlösbar und werden von
/// [`Self::purge_expired`] bzw. beim Ausgeben neuer Tokens entfernt. Höchstens
/// [`MAX_OUTSTANDING_APPROVAL_TOKENS`] Tokens stehen gleichzeitig aus.
///
/// # Concurrency
/// `Send + Sync` via an internal [`Mutex`]; cheap to hold behind an `Arc` and
/// share across rendering and callback-ingress threads.
pub struct ApprovalTokenStore {
    tokens: Mutex<TokenTable>,
    ttl: Option<SignedDuration>,
}

impl std::fmt::Debug for ApprovalTokenStore {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Tokens sind Einlöse-Geheimnisse und werden nie ausgegeben.
        formatter
            .debug_struct("ApprovalTokenStore")
            .field("outstanding", &self.len())
            .field("ttl", &self.ttl)
            .finish_non_exhaustive()
    }
}

impl ApprovalTokenStore {
    /// Erzeugt einen Store, dessen Tokens nicht zeitlich ablaufen (nur die
    /// Obergrenze [`MAX_OUTSTANDING_APPROVAL_TOKENS`] gilt).
    #[must_use]
    pub fn new() -> Self {
        Self {
            tokens: Mutex::new(TokenTable::default()),
            ttl: None,
        }
    }

    /// Erzeugt einen Store, dessen Tokens `ttl` nach ihrer Ausgabe ablaufen.
    ///
    /// Eine nicht positive TTL lässt jedes Token sofort ablaufen (fail closed).
    #[must_use]
    pub fn with_ttl(ttl: SignedDuration) -> Self {
        Self {
            tokens: Mutex::new(TokenTable::default()),
            ttl: Some(ttl),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, TokenTable> {
        self.tokens
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Issues a fresh opaque token for one `(request_id, decision)` pair.
    ///
    /// The token cannot be resolved by [`Self::consume_bound_at`] until
    /// [`Self::bind`] succeeds after Telegram reports the rendered message id.
    /// Ist die Obergrenze erreicht, werden zuerst abgelaufene Tokens entfernt
    /// und danach, falls nötig, die ältesten verdrängt.
    #[must_use]
    pub fn issue(&self, request_id: &str, decision: &str) -> String {
        let now = Timestamp::now();
        let expires_at = self.ttl.map(|ttl| {
            if ttl.is_positive() {
                // Überlauf nur bei absurd großer TTL: dann praktisch nie ablaufend.
                now.checked_add(ttl).unwrap_or(Timestamp::MAX)
            } else {
                // Nicht positive TTL: sofort abgelaufen (fail closed).
                now
            }
        });
        let mut table = self.lock();
        if table.entries.len() >= MAX_OUTSTANDING_APPROVAL_TOKENS {
            table.purge_expired(now);
            while table.entries.len() >= MAX_OUTSTANDING_APPROVAL_TOKENS {
                table.evict_oldest();
            }
        }
        let sequence = table.next_sequence;
        table.next_sequence = table.next_sequence.wrapping_add(1);
        loop {
            let token = format!("{:032x}", rand::random::<u128>());
            if !table.entries.contains_key(&token) {
                table.entries.insert(
                    token.clone(),
                    IssuedApproval {
                        pending: PendingApproval {
                            request_id: request_id.to_owned(),
                            decision: decision.to_owned(),
                        },
                        binding: None,
                        expires_at,
                        sequence,
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
        let mut table = self.lock();
        let Some(issued) = table.entries.get_mut(token) else {
            return false;
        };
        if issued.binding.is_some() {
            return false;
        }
        issued.binding = Some(context);
        true
    }

    /// Consumes a bound token only when the callback context exactly matches,
    /// evaluated against the current wall clock.
    ///
    /// Delegiert an [`Self::consume_bound_at`] mit [`Timestamp::now`].
    pub fn consume_bound(
        &self,
        token: &str,
        context: &ApprovalCallbackContext,
    ) -> Option<PendingApproval> {
        self.consume_bound_at(token, context, Timestamp::now())
    }

    /// Consumes a bound token only when the callback context exactly matches
    /// and the token has not expired as of `now`.
    ///
    /// Unknown, unbound, expired, replayed, and context-mismatched tokens
    /// return `None`. A known token is removed before matching, so every
    /// failed context-aware redemption is terminal.
    pub fn consume_bound_at(
        &self,
        token: &str,
        context: &ApprovalCallbackContext,
        now: Timestamp,
    ) -> Option<PendingApproval> {
        let issued = self.lock().entries.remove(token)?;
        if issued.is_expired(now) {
            return None;
        }
        (issued.binding.as_ref() == Some(context)).then_some(issued.pending)
    }

    /// Entfernt alle Tokens (gebunden oder nicht) für `request_id`, z. B. wenn
    /// die Approval-Nachricht nicht zugestellt werden konnte oder die Anfrage
    /// anderweitig entschieden wurde.
    ///
    /// # Returns
    /// Anzahl der entfernten Tokens.
    pub fn revoke_request(&self, request_id: &str) -> usize {
        let mut table = self.lock();
        let before = table.entries.len();
        table
            .entries
            .retain(|_, issued| issued.pending.request_id != request_id);
        before - table.entries.len()
    }

    /// Entfernt alle zum Zeitpunkt `now` abgelaufenen Tokens.
    ///
    /// # Returns
    /// Anzahl der entfernten Tokens.
    pub fn purge_expired(&self, now: Timestamp) -> usize {
        self.lock().purge_expired(now)
    }

    /// Anzahl der aktuell ausstehenden (auch abgelaufenen, noch nicht
    /// entfernten) Tokens.
    #[must_use]
    pub fn len(&self) -> usize {
        self.lock().entries.len()
    }

    /// Ob keine Tokens ausstehen.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
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
        ApprovalCallbackContext, ApprovalTokenStore, MAX_OUTSTANDING_APPROVAL_TOKENS,
        PendingApproval, TelegramChatId, TelegramMessageId, TelegramThreadId,
    };
    use harw_types::PeerId;
    use jiff::{SignedDuration, Timestamp};

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
    fn expired_token_is_rejected_and_invalidated() {
        let store = ApprovalTokenStore::with_ttl(SignedDuration::from_secs(60));
        let token = store.issue("request-1", "approve");
        let context = context();
        assert!(store.bind(&token, context.clone()));

        let later = Timestamp::now()
            .checked_add(SignedDuration::from_secs(120))
            .unwrap_or(Timestamp::MAX);
        assert_eq!(store.consume_bound_at(&token, &context, later), None);
        assert_eq!(store.len(), 0);
        assert_eq!(store.consume_bound(&token, &context), None);
    }

    #[test]
    fn token_within_ttl_consumes() {
        let store = ApprovalTokenStore::with_ttl(SignedDuration::from_secs(60));
        let token = store.issue("request-1", "approve");
        let context = context();
        assert!(store.bind(&token, context.clone()));
        assert!(
            store
                .consume_bound_at(&token, &context, Timestamp::now())
                .is_some()
        );
    }

    #[test]
    fn store_without_ttl_never_expires() {
        let store = ApprovalTokenStore::new();
        let token = store.issue("request-1", "approve");
        let context = context();
        assert!(store.bind(&token, context.clone()));
        assert_eq!(store.purge_expired(Timestamp::MAX), 0);
        assert!(
            store
                .consume_bound_at(&token, &context, Timestamp::MAX)
                .is_some()
        );
    }

    #[test]
    fn purge_expired_removes_only_expired_tokens() {
        let store = ApprovalTokenStore::with_ttl(SignedDuration::from_secs(60));
        let _first = store.issue("request-1", "approve");
        let _second = store.issue("request-2", "deny");
        assert_eq!(store.len(), 2);
        assert_eq!(store.purge_expired(Timestamp::now()), 0);
        let later = Timestamp::now()
            .checked_add(SignedDuration::from_secs(61))
            .unwrap_or(Timestamp::MAX);
        assert_eq!(store.purge_expired(later), 2);
        assert!(store.is_empty());
    }

    #[test]
    fn consume_bound_at_mismatch_is_terminal_even_within_ttl() {
        let store = ApprovalTokenStore::with_ttl(SignedDuration::from_secs(60));
        let token = store.issue("request-1", "approve");
        let expected = context();
        assert!(store.bind(&token, expected.clone()));
        let wrong = ApprovalCallbackContext::new(
            TelegramChatId(101),
            TelegramMessageId(202),
            PeerId::from_str("999"),
        )
        .with_thread(TelegramThreadId(404));

        let now = Timestamp::now();
        assert_eq!(store.consume_bound_at(&token, &wrong, now), None);
        assert_eq!(store.consume_bound_at(&token, &expected, now), None);
        assert!(store.is_empty());
    }

    #[test]
    fn revoke_request_removes_all_tokens_of_that_request_only() {
        let store = ApprovalTokenStore::new();
        let approve = store.issue("request-1", "approve");
        let deny = store.issue("request-1", "deny");
        let other = store.issue("request-2", "approve");
        let context = context();
        assert!(store.bind(&approve, context.clone()));

        assert_eq!(store.revoke_request("request-1"), 2);
        assert_eq!(store.revoke_request("request-1"), 0);
        assert_eq!(store.len(), 1);
        assert_eq!(store.consume_bound(&approve, &context), None);
        assert!(!store.bind(&deny, context.clone()));
        assert!(store.bind(&other, context.clone()));
        assert!(store.consume_bound(&other, &context).is_some());
    }

    #[test]
    fn cap_evicts_the_oldest_token() {
        let store = ApprovalTokenStore::new();
        let oldest = store.issue("request-oldest", "approve");
        let second = store.issue("request-second", "approve");
        for index in 2..MAX_OUTSTANDING_APPROVAL_TOKENS {
            let _ = store.issue(&format!("request-{index}"), "approve");
        }
        assert_eq!(store.len(), MAX_OUTSTANDING_APPROVAL_TOKENS);

        let newest = store.issue("request-newest", "approve");
        assert_eq!(store.len(), MAX_OUTSTANDING_APPROVAL_TOKENS);
        let context = context();
        assert!(!store.bind(&oldest, context.clone()));
        assert!(store.bind(&second, context.clone()));
        assert!(store.bind(&newest, context));
    }

    #[test]
    fn cap_purges_expired_tokens_before_evicting_live_ones() {
        // TTL null: jedes Token ist bei der nächsten Ausgabe bereits abgelaufen.
        let store = ApprovalTokenStore::with_ttl(SignedDuration::ZERO);
        for index in 0..MAX_OUTSTANDING_APPROVAL_TOKENS {
            let _ = store.issue(&format!("request-{index}"), "approve");
        }
        assert_eq!(store.len(), MAX_OUTSTANDING_APPROVAL_TOKENS);
        let _ = store.issue("request-new", "approve");
        assert_eq!(store.len(), 1);
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
