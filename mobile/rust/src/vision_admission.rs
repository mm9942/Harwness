//! Pure, one-route admission for one-image vision work on an API or a device.
//!
//! Supply time from a monotonic clock relative to the job origin. The caller
//! parses Retry-After (including any HTTP date) into a duration, owns the
//! transport and physical worker permits, and only materializes an image
//! after receiving its ticket. Use an opaque route key covering provider, model, endpoint
//! and other route settings that affect the rate budget. There is no fallback
//! route, network, persistence, or clock read in this module.

use crate::local_vision::MAX_LOCAL_VISION_CONCURRENCY;
use std::{collections::VecDeque, sync::Arc, time::Duration};

/// Maximum images in a job at construction, including those later in flight.
pub(crate) const MAX_SELECTED_VISION_IMAGES: usize = 50;
pub(crate) const MIN_API_VISION_CONCURRENCY: usize = 5;
pub(crate) const MAX_VISION_REQUEST_CONCURRENCY: usize = 50;
pub(crate) const DEFAULT_API_VISION_CONCURRENCY: usize = 30;
const LOWEST_EFFECTIVE_API_CONCURRENCY: usize = 1;
const FIRST_THROTTLE_DECREASE: usize = 10;
const LATER_THROTTLE_DECREASE: usize = 5;
/// Includes the original request and any 429 retries.
pub(crate) const MAX_VISION_ATTEMPTS: u8 = 3;
const DEFAULT_RETRY_AFTER: Duration = Duration::from_secs(1);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AdmissionError {
    InvalidSelectionCount,
    InvalidConcurrency,
    InvalidDeviceCapacity,
}

/// Saved or learned advisory ceiling for one exact route. The caller owns
/// persistence and must not use it as consent or as a provider quota claim.
pub(crate) struct RouteRecommendation<R> {
    route: R,
    concurrency: usize,
}

impl<R> RouteRecommendation<R> {
    pub(crate) fn new(route: R, concurrency: usize) -> Result<Self, AdmissionError> {
        if !(LOWEST_EFFECTIVE_API_CONCURRENCY..=MAX_VISION_REQUEST_CONCURRENCY)
            .contains(&concurrency)
        {
            return Err(AdmissionError::InvalidConcurrency);
        }
        Ok(Self { route, concurrency })
    }

    pub(crate) fn route(&self) -> &R {
        &self.route
    }

    pub(crate) fn concurrency(&self) -> usize {
        self.concurrency
    }
}

/// Opaque lease for one request-local position and one attempt. Private origin
/// and wave prevent an old job or an already settled callback changing this job.
#[derive(Clone)]
pub(crate) struct VisionTicket {
    origin: Arc<()>,
    request: PendingImage,
    wave: u8,
}

impl VisionTicket {
    pub(crate) fn ordinal(&self) -> usize {
        self.request.ordinal
    }

    pub(crate) fn attempt(&self) -> u8 {
        self.request.attempt
    }
}

pub(crate) enum GatePoll {
    Ready(VisionTicket),
    AtCapacity,
    WaitUntil(Duration),
    /// An otherwise valid server delay overflowed the supplied clock domain.
    /// No request can be admitted; surface this rather than shortening it.
    WaitBeyondClock,
    /// No new work or automatic resume. The caller may still drain issued tickets.
    QuotaBlocked(QuotaBlock),
    Draining,
    Done,
    Cancelled,
}

/// Reasons positively identified by a provider adapter, never inferred from
/// the HTTP status alone or from the presence/absence of a Retry-After header.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum QuotaBlock {
    Billing,
    SpendLimit,
    UsageLimit,
}

pub(crate) enum VisionCompletion {
    Succeeded,
    Failed,
    Cancelled,
    /// The request may have reached the provider; do not retry automatically.
    UnknownDelivery,
    QuotaBlocked(QuotaBlock),
    /// Only a provider-confirmed temporary throttle can trigger a retry.
    RateLimited {
        retry_after: Option<Duration>,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CompletionDisposition {
    Ignored,
    Succeeded,
    Failed,
    Cancelled,
    UnknownDelivery,
    QuotaBlocked(QuotaBlock),
    /// An issued request was throttled after quota had already paused this job.
    RetrySuppressed(QuotaBlock),
    Requeued,
    AttemptsExhausted,
}

#[derive(Clone, Copy)]
struct PendingImage {
    ordinal: usize,
    attempt: u8,
}

#[derive(Clone, Copy)]
enum Cooldown {
    Open,
    Until(Duration),
    BeyondClock,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Lane {
    Api,
    Local,
}

pub(crate) struct VisionAdmission<R> {
    route: R,
    lane: Lane,
    origin: Arc<()>,
    configured: usize,
    effective: usize,
    recommendation: Option<usize>,
    pending: VecDeque<PendingImage>,
    active: [Option<u8>; MAX_SELECTED_VISION_IMAGES],
    wave: u8,
    clock: Duration,
    cooldown: Cooldown,
    quota_block: Option<QuotaBlock>,
    cancelled: bool,
}

impl<R: Clone + Eq> VisionAdmission<R> {
    /// API mode only. The prior recommendation is trusted app-owned state for
    /// an exact route and represents an earlier, verified throttle episode.
    pub(crate) fn new(
        route: R,
        selection_count: usize,
        configured_concurrency: usize,
        prior: Option<&RouteRecommendation<R>>,
    ) -> Result<Self, AdmissionError> {
        if !(MIN_API_VISION_CONCURRENCY..=MAX_VISION_REQUEST_CONCURRENCY)
            .contains(&configured_concurrency)
        {
            return Err(AdmissionError::InvalidConcurrency);
        }
        let recommendation = prior
            .filter(|recommendation| recommendation.route() == &route)
            .map(RouteRecommendation::concurrency);
        let effective =
            configured_concurrency.min(recommendation.unwrap_or(configured_concurrency));
        Self::with_limits(
            route,
            selection_count,
            configured_concurrency,
            effective,
            recommendation,
            Lane::Api,
        )
    }

    /// Local mode accepts a configured 1..=5, clamped to the hardware/backend
    /// cap actually supported for this operation. Zero capacity is unavailable.
    pub(crate) fn new_local(
        route: R,
        selection_count: usize,
        configured_concurrency: usize,
        device_cap: usize,
    ) -> Result<Self, AdmissionError> {
        if !(1..=MAX_LOCAL_VISION_CONCURRENCY).contains(&configured_concurrency) {
            return Err(AdmissionError::InvalidConcurrency);
        }
        if device_cap == 0 {
            return Err(AdmissionError::InvalidDeviceCapacity);
        }
        Self::with_limits(
            route,
            selection_count,
            configured_concurrency,
            configured_concurrency.min(device_cap),
            None,
            Lane::Local,
        )
    }

    fn with_limits(
        route: R,
        selection_count: usize,
        configured: usize,
        effective: usize,
        recommendation: Option<usize>,
        lane: Lane,
    ) -> Result<Self, AdmissionError> {
        if !(1..=MAX_SELECTED_VISION_IMAGES).contains(&selection_count) {
            return Err(AdmissionError::InvalidSelectionCount);
        }
        Ok(Self {
            route,
            lane,
            origin: Arc::new(()),
            configured,
            effective,
            recommendation,
            pending: (1..=selection_count)
                .map(|ordinal| PendingImage {
                    ordinal,
                    attempt: 1,
                })
                .collect(),
            active: [None; MAX_SELECTED_VISION_IMAGES],
            wave: 0,
            clock: Duration::ZERO,
            cooldown: Cooldown::Open,
            quota_block: None,
            cancelled: false,
        })
    }

    /// Freeze this route when issuing a transport request, not its UI default.
    pub(crate) fn route(&self) -> &R {
        &self.route
    }

    pub(crate) fn configured_concurrency(&self) -> usize {
        self.configured
    }

    pub(crate) fn effective_concurrency(&self) -> usize {
        self.effective
    }

    pub(crate) fn in_flight(&self) -> usize {
        self.active.iter().flatten().count()
    }

    pub(crate) fn queued(&self) -> usize {
        self.pending.len()
    }

    /// Successes do not automatically raise a held recommendation; reducing
    /// the user's requested concurrency alone is not evidence of a rate limit.
    pub(crate) fn recommendation(&self) -> Option<RouteRecommendation<R>> {
        self.recommendation.map(|concurrency| RouteRecommendation {
            route: self.route.clone(),
            concurrency,
        })
    }

    /// Admits at most one image. A decrease in effective concurrency only
    /// delays new admissions; it does not revoke issued worker tickets.
    pub(crate) fn poll(&mut self, now: Duration) -> GatePoll {
        if self.cancelled {
            return GatePoll::Cancelled;
        }
        if let Some(reason) = self.quota_block {
            return GatePoll::QuotaBlocked(reason);
        }
        self.clock = self.clock.max(now);
        let in_flight = self.in_flight();
        if self.pending.is_empty() {
            return if in_flight == 0 {
                GatePoll::Done
            } else {
                GatePoll::Draining
            };
        }
        match self.cooldown {
            Cooldown::Until(until) if self.clock < until => return GatePoll::WaitUntil(until),
            Cooldown::BeyondClock => return GatePoll::WaitBeyondClock,
            Cooldown::Open | Cooldown::Until(_) => {}
        }
        if in_flight >= self.effective_concurrency() {
            return GatePoll::AtCapacity;
        }

        let Some(request) = self.pending.pop_front() else {
            return GatePoll::Done;
        };
        self.active[request.ordinal - 1] = Some(request.attempt);
        GatePoll::Ready(VisionTicket {
            origin: Arc::clone(&self.origin),
            request,
            wave: self.wave,
        })
    }

    pub(crate) fn complete(
        &mut self,
        ticket: &VisionTicket,
        completion: VisionCompletion,
        now: Duration,
    ) -> CompletionDisposition {
        if !Arc::ptr_eq(&ticket.origin, &self.origin) {
            return CompletionDisposition::Ignored;
        }
        let slot = &mut self.active[ticket.ordinal() - 1];
        if *slot != Some(ticket.attempt()) {
            return CompletionDisposition::Ignored;
        }
        *slot = None;
        if self.cancelled {
            return CompletionDisposition::Ignored;
        }
        self.clock = self.clock.max(now);
        match completion {
            VisionCompletion::Succeeded => CompletionDisposition::Succeeded,
            VisionCompletion::Failed => CompletionDisposition::Failed,
            VisionCompletion::Cancelled => CompletionDisposition::Cancelled,
            VisionCompletion::UnknownDelivery => CompletionDisposition::UnknownDelivery,
            VisionCompletion::QuotaBlocked(_) | VisionCompletion::RateLimited { .. }
                if self.lane == Lane::Local =>
            {
                CompletionDisposition::Failed
            }
            VisionCompletion::QuotaBlocked(reason) => {
                self.quota_block.get_or_insert(reason);
                CompletionDisposition::QuotaBlocked(reason)
            }
            VisionCompletion::RateLimited { retry_after } => {
                if let Some(reason) = self.quota_block {
                    return CompletionDisposition::RetrySuppressed(reason);
                }
                self.extend_cooldown(retry_after.unwrap_or(DEFAULT_RETRY_AFTER));
                // Several requests already in flight can receive the same
                // overload signal. Retries extend the shared wait, but lower
                // the ceiling at most once until a new wave has been admitted.
                if ticket.wave == self.wave {
                    let decrease = if self.recommendation.is_some() {
                        LATER_THROTTLE_DECREASE
                    } else {
                        FIRST_THROTTLE_DECREASE
                    };
                    self.effective = self
                        .effective
                        .saturating_sub(decrease)
                        .max(LOWEST_EFFECTIVE_API_CONCURRENCY);
                    self.recommendation = Some(self.effective);
                    self.wave = self.wave.saturating_add(1);
                }
                if ticket.attempt() >= MAX_VISION_ATTEMPTS {
                    return CompletionDisposition::AttemptsExhausted;
                }
                self.pending.push_back(PendingImage {
                    ordinal: ticket.ordinal(),
                    attempt: ticket.attempt() + 1,
                });
                CompletionDisposition::Requeued
            }
        }
    }

    /// Stops new work. Issued tickets remain counted until their workers drain;
    /// their callbacks must not learn recommendations or reschedule anything.
    pub(crate) fn cancel(&mut self) {
        self.cancelled = true;
        self.pending.clear();
    }

    fn extend_cooldown(&mut self, delay: Duration) {
        self.cooldown = match (self.cooldown, self.clock.checked_add(delay)) {
            (Cooldown::BeyondClock, _) | (_, None) => Cooldown::BeyondClock,
            (Cooldown::Until(prior), Some(next)) => Cooldown::Until(prior.max(next)),
            (Cooldown::Open, Some(next)) => Cooldown::Until(next),
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(value: u64) -> Duration {
        Duration::from_millis(value)
    }

    fn start(n: usize, concurrency: usize) -> VisionAdmission<&'static str> {
        VisionAdmission::new("route-a/model-x", n, concurrency, None).unwrap()
    }

    fn admit<R: Clone + Eq>(state: &mut VisionAdmission<R>, now: Duration) -> VisionTicket {
        let GatePoll::Ready(ticket) = state.poll(now) else {
            panic!("expected one image to be admitted");
        };
        ticket
    }

    fn rate_limited(retry_after: Option<Duration>) -> VisionCompletion {
        VisionCompletion::RateLimited { retry_after }
    }

    #[test]
    fn selection_and_configured_concurrency_have_independent_hard_limits() {
        assert_eq!(DEFAULT_API_VISION_CONCURRENCY, 30);
        assert_eq!(crate::local_vision::DEFAULT_LOCAL_VISION_CONCURRENCY, 3);
        for (n, limit, expected) in [
            (0, 5, AdmissionError::InvalidSelectionCount),
            (51, 5, AdmissionError::InvalidSelectionCount),
            (2, 4, AdmissionError::InvalidConcurrency),
            (2, 51, AdmissionError::InvalidConcurrency),
        ] {
            assert_eq!(
                VisionAdmission::new("route", n, limit, None).err(),
                Some(expected)
            );
        }
        assert_eq!(start(50, 50).effective_concurrency(), 50);
        let mut state = start(50, DEFAULT_API_VISION_CONCURRENCY);
        let mut running = Vec::new();
        for ordinal in 1..=30 {
            let ticket = admit(&mut state, ms(0));
            assert_eq!((ticket.ordinal(), ticket.attempt()), (ordinal, 1));
            running.push(ticket);
        }
        assert_eq!(state.in_flight(), 30);
        assert!(matches!(state.poll(ms(0)), GatePoll::AtCapacity));
        for (completed, next) in running.iter().zip(31..=50) {
            assert_eq!(
                state.complete(completed, VisionCompletion::Succeeded, ms(1)),
                CompletionDisposition::Succeeded
            );
            assert_eq!(admit(&mut state, ms(1)).ordinal(), next);
        }
        assert!(matches!(state.poll(ms(1)), GatePoll::Draining));
    }

    #[test]
    fn configured_concurrency_caps_outstanding_single_image_tickets() {
        let mut state = start(8, 5);
        let first = admit(&mut state, ms(0));
        for ordinal in 2..=5 {
            assert_eq!(admit(&mut state, ms(0)).ordinal(), ordinal);
        }
        assert!(matches!(state.poll(ms(0)), GatePoll::AtCapacity));
        assert_eq!(
            state.complete(&first, VisionCompletion::Succeeded, ms(3)),
            CompletionDisposition::Succeeded
        );
        assert_eq!(admit(&mut state, ms(3)).ordinal(), 6);
        assert_eq!(state.in_flight(), 5);
    }

    #[test]
    fn local_job_can_queue_fifty_but_only_three_run_at_once() {
        assert_eq!(
            VisionAdmission::new_local("device/model", 50, 6, 3).err(),
            Some(AdmissionError::InvalidConcurrency)
        );
        assert_eq!(
            VisionAdmission::new_local("device/model", 50, 5, 0).err(),
            Some(AdmissionError::InvalidDeviceCapacity)
        );
        let mut state = VisionAdmission::new_local("device/model", 50, 5, 3).unwrap();
        assert_eq!(state.configured_concurrency(), 5);
        assert_eq!(state.effective_concurrency(), 3);
        assert_eq!(state.queued(), 50);
        let first = admit(&mut state, ms(0));
        assert_eq!(admit(&mut state, ms(0)).ordinal(), 2);
        assert_eq!(admit(&mut state, ms(0)).ordinal(), 3);
        assert_eq!(state.queued(), 47);
        assert!(matches!(state.poll(ms(0)), GatePoll::AtCapacity));
        assert_eq!(
            state.complete(&first, VisionCompletion::Succeeded, ms(0)),
            CompletionDisposition::Succeeded
        );
        assert_eq!(admit(&mut state, ms(0)).ordinal(), 4);
    }

    #[test]
    fn local_jobs_cannot_learn_api_limits_or_requeue_api_throttle() {
        let mut state = VisionAdmission::new_local("device/model", 4, 3, 5).unwrap();
        let first = admit(&mut state, ms(0));
        assert_eq!(
            state.complete(&first, rate_limited(Some(ms(1_000))), ms(0)),
            CompletionDisposition::Failed
        );
        assert_eq!(admit(&mut state, ms(0)).ordinal(), 2);
        assert_eq!(state.effective_concurrency(), 3);
        assert!(state.recommendation().is_none());
    }

    #[test]
    fn quota_stops_pending_work_without_cancelling_active_or_learning_limits() {
        let prior = RouteRecommendation::new("route", 2).unwrap();
        let mut state = VisionAdmission::new("route", 5, 10, Some(&prior)).unwrap();
        let first = admit(&mut state, ms(0));
        let second = admit(&mut state, ms(0));
        assert_eq!(
            state.complete(
                &first,
                VisionCompletion::QuotaBlocked(QuotaBlock::SpendLimit),
                ms(1)
            ),
            CompletionDisposition::QuotaBlocked(QuotaBlock::SpendLimit)
        );
        assert_eq!(state.in_flight(), 1);
        assert_eq!(state.queued(), 3);
        assert!(matches!(
            state.poll(ms(2)),
            GatePoll::QuotaBlocked(QuotaBlock::SpendLimit)
        ));
        assert_eq!(
            state.complete(&second, rate_limited(Some(ms(500))), ms(2)),
            CompletionDisposition::RetrySuppressed(QuotaBlock::SpendLimit)
        );
        assert_eq!(state.in_flight(), 0);
        assert_eq!(state.queued(), 3);
        assert!(matches!(
            state.poll(ms(10_000)),
            GatePoll::QuotaBlocked(QuotaBlock::SpendLimit)
        ));
        assert_eq!(state.effective_concurrency(), 2);
        assert_eq!(state.recommendation().unwrap().concurrency(), 2);
        state.cancel();
        assert_eq!(state.queued(), 0);
        assert!(matches!(state.poll(ms(10_000)), GatePoll::Cancelled));
    }

    #[test]
    fn late_quota_does_not_override_first_route_block_and_success_still_settles() {
        let mut state = start(3, 5);
        let first = admit(&mut state, ms(0));
        let second = admit(&mut state, ms(0));
        let third = admit(&mut state, ms(0));
        assert_eq!(
            state.complete(
                &first,
                VisionCompletion::QuotaBlocked(QuotaBlock::Billing),
                ms(1)
            ),
            CompletionDisposition::QuotaBlocked(QuotaBlock::Billing)
        );
        assert_eq!(
            state.complete(
                &second,
                VisionCompletion::QuotaBlocked(QuotaBlock::UsageLimit),
                ms(2)
            ),
            CompletionDisposition::QuotaBlocked(QuotaBlock::UsageLimit)
        );
        assert_eq!(
            state.complete(&third, VisionCompletion::Succeeded, ms(2)),
            CompletionDisposition::Succeeded
        );
        assert!(matches!(
            state.poll(ms(3)),
            GatePoll::QuotaBlocked(QuotaBlock::Billing)
        ));
        assert!(state.recommendation().is_none());
    }

    #[test]
    fn ambiguous_delivery_is_terminal_and_not_a_throttle() {
        let mut state = start(2, 5);
        let first = admit(&mut state, ms(0));
        assert_eq!(
            state.complete(&first, VisionCompletion::UnknownDelivery, ms(0)),
            CompletionDisposition::UnknownDelivery
        );
        let second = admit(&mut state, ms(0));
        assert_eq!(second.ordinal(), 2);
        state.complete(&second, VisionCompletion::Succeeded, ms(0));
        assert!(matches!(state.poll(ms(0)), GatePoll::Done));
        assert!(state.recommendation().is_none());
    }

    #[test]
    fn retry_after_is_respected_and_the_429_goes_behind_existing_work() {
        let mut state = start(3, 5);
        let first = admit(&mut state, ms(0));
        assert_eq!(
            state.complete(&first, rate_limited(Some(ms(2_000))), ms(10)),
            CompletionDisposition::Requeued
        );
        assert!(matches!(state.poll(ms(2_009)), GatePoll::WaitUntil(t) if t == ms(2_010)));
        for ordinal in [2, 3] {
            let next = admit(&mut state, ms(2_010));
            assert_eq!((next.ordinal(), next.attempt()), (ordinal, 1));
            assert_eq!(
                state.complete(&next, VisionCompletion::Succeeded, ms(2_010)),
                CompletionDisposition::Succeeded
            );
        }
        let retry = admit(&mut state, ms(2_010));
        assert_eq!((retry.ordinal(), retry.attempt()), (1, 2));
    }

    #[test]
    fn rate_limit_reduces_once_per_wave_and_does_not_revoke_running_tickets() {
        let mut state = start(40, 20);
        let running: Vec<_> = (0..20).map(|_| admit(&mut state, ms(0))).collect();
        assert_eq!(
            state.complete(&running[0], rate_limited(Some(ms(50))), ms(10)),
            CompletionDisposition::Requeued
        );
        assert_eq!(state.effective_concurrency(), 10);
        assert_eq!(state.in_flight(), 19);
        assert_eq!(state.configured_concurrency(), 20);
        assert!(matches!(state.poll(ms(60)), GatePoll::AtCapacity));
        assert_eq!(
            state.complete(&running[1], rate_limited(Some(ms(100))), ms(61)),
            CompletionDisposition::Requeued
        );
        assert_eq!(state.effective_concurrency(), 10);
        assert!(matches!(state.poll(ms(160)), GatePoll::WaitUntil(t) if t == ms(161)));
        for ticket in &running[2..11] {
            assert_eq!(
                state.complete(ticket, VisionCompletion::Succeeded, ms(161)),
                CompletionDisposition::Succeeded
            );
        }
        let new_wave = admit(&mut state, ms(161));
        assert_eq!(new_wave.ordinal(), 21);
        assert_eq!(
            state.complete(&new_wave, rate_limited(Some(ms(1))), ms(162)),
            CompletionDisposition::Requeued
        );
        assert_eq!(state.effective_concurrency(), 5);
        assert_eq!(state.in_flight(), 9);
        let held = state
            .recommendation()
            .expect("observed lower route ceiling");
        assert_eq!(held.concurrency(), 5);
        assert_eq!(held.route(), &"route-a/model-x");
        assert_eq!(
            state.complete(&running[11], VisionCompletion::Succeeded, ms(164)),
            CompletionDisposition::Succeeded
        );
        assert_eq!(state.recommendation().unwrap().concurrency(), 5);
    }

    #[test]
    fn first_decrease_cannot_cross_effective_floor_and_prior_counts_as_episode() {
        let mut state = start(2, 5);
        let first = admit(&mut state, ms(0));
        state.complete(&first, rate_limited(Some(ms(0))), ms(0));
        assert_eq!(state.effective_concurrency(), 1);
        assert_eq!(state.configured_concurrency(), 5);

        let recommendation = RouteRecommendation::new("route", 18).unwrap();
        let mut state = VisionAdmission::new("route", 2, 30, Some(&recommendation)).unwrap();
        let first = admit(&mut state, ms(0));
        state.complete(&first, rate_limited(Some(ms(0))), ms(0));
        assert_eq!(state.effective_concurrency(), 13);
    }

    #[test]
    fn a_route_recommendation_does_not_affect_another_provider_or_model() {
        let saved = RouteRecommendation::new("route-a/model-x", 9).unwrap();
        let same = VisionAdmission::new("route-a/model-x", 20, 12, Some(&saved)).unwrap();
        assert_eq!(same.route(), &"route-a/model-x");
        assert_eq!(same.effective_concurrency(), 9);
        let x = VisionAdmission::new("route-a/model-x", 20, 5, Some(&saved)).unwrap();
        assert_eq!(x.effective_concurrency(), 5);
        assert_eq!(x.recommendation().unwrap().concurrency(), 9);
        for other in ["route-b/model-x", "route-a/model-y"] {
            let other = VisionAdmission::new(other, 20, 12, Some(&saved)).unwrap();
            assert_eq!(other.effective_concurrency(), 12);
            assert!(other.recommendation().is_none());
        }
    }

    #[test]
    fn exhausted_rate_limit_and_non_rate_limit_failures_are_terminal() {
        let mut state = start(1, 5);
        for (attempt, expected) in [
            (1, CompletionDisposition::Requeued),
            (2, CompletionDisposition::Requeued),
            (3, CompletionDisposition::AttemptsExhausted),
        ] {
            let ticket = admit(&mut state, ms(0));
            assert_eq!(ticket.attempt(), attempt);
            assert_eq!(
                state.complete(&ticket, rate_limited(Some(ms(0))), ms(0)),
                expected
            );
        }
        assert!(matches!(state.poll(ms(0)), GatePoll::Done));

        let mut state = start(1, 5);
        let ticket = admit(&mut state, ms(0));
        assert_eq!(
            state.complete(&ticket, VisionCompletion::Failed, ms(0)),
            CompletionDisposition::Failed
        );
        assert!(matches!(state.poll(ms(0)), GatePoll::Done));
    }

    #[test]
    fn foreign_duplicate_and_older_attempt_completions_are_inert() {
        let mut other = start(1, 5);
        let foreign = admit(&mut other, ms(0));
        let mut state = start(2, 5);
        let first = admit(&mut state, ms(0));
        assert_eq!(
            state.complete(&foreign, rate_limited(Some(Duration::MAX)), Duration::MAX),
            CompletionDisposition::Ignored
        );
        assert!(state.recommendation().is_none());
        assert_eq!(
            state.complete(&first, rate_limited(Some(ms(10))), ms(0)),
            CompletionDisposition::Requeued
        );
        let other_ordinal = admit(&mut state, ms(10));
        state.complete(&other_ordinal, VisionCompletion::Succeeded, ms(10));
        let retry = admit(&mut state, ms(10));
        assert_eq!(retry.attempt(), 2);
        assert_eq!(
            state.complete(&first, rate_limited(Some(Duration::MAX)), Duration::MAX),
            CompletionDisposition::Ignored
        );
        assert_eq!(state.in_flight(), 1);
        assert_eq!(
            state.complete(&retry, VisionCompletion::Succeeded, ms(11)),
            CompletionDisposition::Succeeded
        );
        assert_eq!(
            state.complete(&retry, VisionCompletion::Succeeded, ms(11)),
            CompletionDisposition::Ignored
        );
        assert!(matches!(state.poll(ms(11)), GatePoll::Done));
    }

    #[test]
    fn cancelling_a_job_drains_late_callbacks_without_learning_a_limit() {
        let mut state = start(3, 5);
        let a = admit(&mut state, ms(0));
        let b = admit(&mut state, ms(0));
        state.cancel();
        assert!(matches!(state.poll(ms(10)), GatePoll::Cancelled));
        assert_eq!(state.in_flight(), 2);
        assert_eq!(
            state.complete(&a, rate_limited(Some(ms(50))), ms(0)),
            CompletionDisposition::Ignored
        );
        assert_eq!(
            state.complete(&b, VisionCompletion::Succeeded, ms(0)),
            CompletionDisposition::Ignored
        );
        assert_eq!(state.in_flight(), 0);
        assert!(state.recommendation().is_none());
        assert!(matches!(state.poll(ms(1_000)), GatePoll::Cancelled));
    }

    #[test]
    fn missing_retry_after_uses_default_and_clock_overflow_never_shortens_wait() {
        let mut state = start(2, 5);
        let first = admit(&mut state, ms(200));
        state.complete(&first, rate_limited(None), ms(100)); // Regressing synthetic time.
        assert!(matches!(state.poll(ms(1_199)), GatePoll::WaitUntil(t) if t == ms(1_200)));
        assert_eq!(admit(&mut state, ms(1_200)).ordinal(), 2);

        let mut state = start(2, 5);
        let first = admit(&mut state, ms(1));
        state.complete(&first, rate_limited(Some(Duration::MAX)), ms(1));
        assert!(matches!(
            state.poll(Duration::MAX),
            GatePoll::WaitBeyondClock
        ));
    }

    #[test]
    fn cancelling_one_ticket_never_requeues_it() {
        let mut state = start(2, 5);
        let first = admit(&mut state, ms(0));
        assert_eq!(
            state.complete(&first, VisionCompletion::Cancelled, ms(0)),
            CompletionDisposition::Cancelled
        );
        let second = admit(&mut state, ms(0));
        assert_eq!(second.ordinal(), 2);
        state.complete(&second, VisionCompletion::Succeeded, ms(0));
        assert!(matches!(state.poll(ms(0)), GatePoll::Done));
    }
}
