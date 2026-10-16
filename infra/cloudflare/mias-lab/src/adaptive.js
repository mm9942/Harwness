// Model-local concurrency policy. Gateway count never changes Workers AI quota.
export function concurrencyBoundsForModel(model) {
  if (/kimi-k2\.[67]|glm-5\.[23]/.test(model)) {
    return { min: 2, max: 10 };
  }
  if (/gpt-oss-120b|deepseek-v4/.test(model)) {
    return { min: 3, max: 12 };
  }
  return { min: 6, max: 16 };
}

export class AdaptiveLane {
  constructor(bounds, clock = () => Date.now()) {
    if (!Number.isInteger(bounds.min) || !Number.isInteger(bounds.max) ||
        bounds.min < 1 || bounds.max < bounds.min) {
      throw new TypeError('Invalid concurrency bounds');
    }
    this.min = bounds.min;
    this.max = bounds.max;
    this.limit = bounds.min;
    this.clock = clock;
    this.lastPressureAt = clock();
    this.cooldownUntil = 0;
  }

  // Demand is requests currently running + queued + this new request.
  // Scale up immediately when a burst needs additional slots. Only scale down
  // after a quiet minute, avoiding repeated cold/cache-placement churn.
  noteDemand(demand) {
    const now = this.clock();
    if (demand > this.min) this.lastPressureAt = now;
    if (now < this.cooldownUntil) return this.limit;
    if (demand > this.limit) {
      this.limit = Math.min(this.max, demand);
    } else if (demand <= this.min && now - this.lastPressureAt >= 60_000) {
      this.limit = this.min;
    }
    return this.limit;
  }

  isCoolingDown() {
    return this.clock() < this.cooldownUntil;
  }

  retryAfterSeconds() {
    return Math.max(1, Math.ceil((this.cooldownUntil - this.clock()) / 1000));
  }

  // Account/model capacity pressure: do not walk the entire gateway ring.
  // Reduce local concurrency and reject new admissions for 20s.
  onSharedCapacity() {
    this.limit = Math.max(this.min, Math.floor(this.limit / 2));
    this.cooldownUntil = Math.max(this.cooldownUntil, this.clock() + 20_000);
    return this.limit;
  }

  // Every gateway remains eligible as a sticky primary. Only transient
  // route-local failures consume attempts from this bounded failover budget.
  gatewayAttemptBudget() {
    return this.limit >= Math.ceil(this.max / 2) ? 6 : 3;
  }
}
