import test from "node:test";
import assert from "node:assert/strict";
import { AdaptiveLane, concurrencyBoundsForModel } from "../src/adaptive.js";

test("model-specific lower and upper bounds", () => {
  assert.deepEqual(concurrencyBoundsForModel("@cf/zai-org/glm-5.3-flash"), { min: 2, max: 10 });
  assert.deepEqual(concurrencyBoundsForModel("@cf/moonshotai/kimi-k2.7-code"), { min: 2, max: 10 });
  assert.deepEqual(concurrencyBoundsForModel("@cf/deepseek-ai/deepseek-v4-flash-0731"), { min: 3, max: 12 });
  assert.deepEqual(concurrencyBoundsForModel("@cf/openai/gpt-oss-20b"), { min: 6, max: 16 });
});

test("demand expands to configured maximum; idle settles back to baseline", () => {
  let now = 0;
  const lane = new AdaptiveLane({ min: 2, max: 10 }, () => now);
  assert.equal(lane.noteDemand(1), 2);
  assert.equal(lane.noteDemand(3), 3);
  assert.equal(lane.noteDemand(8), 8);
  assert.equal(lane.noteDemand(24), 10);
  assert.equal(lane.gatewayAttemptBudget(), 6);
  now = 12_000;
  assert.equal(lane.noteDemand(1), 10);
  now = 73_000;
  assert.equal(lane.noteDemand(1), 2);
  assert.equal(lane.gatewayAttemptBudget(), 3);
});

test("shared model pressure cuts parallelism and enforces cooldown", () => {
  let now = 100_000;
  const lane = new AdaptiveLane({ min: 2, max: 10 }, () => now);
  lane.noteDemand(10);
  assert.equal(lane.onSharedCapacity(), 5);
  assert.equal(lane.isCoolingDown(), true);
  assert.equal(lane.noteDemand(20), 5);
  assert.equal(lane.retryAfterSeconds(), 20);
  now += 19_999;
  assert.equal(lane.isCoolingDown(), true);
  now++;
  assert.equal(lane.isCoolingDown(), false);
  assert.equal(lane.noteDemand(6), 6);
});

test("small models accept more parallelism but never exceed a bounded maximum", () => {
  let now = 0;
  const lane = new AdaptiveLane({ min: 6, max: 16 }, () => now);
  assert.equal(lane.noteDemand(14), 14);
  assert.equal(lane.noteDemand(60), 16);
  lane.onSharedCapacity();
  assert.equal(lane.limit, 8);
  assert.throws(() => new AdaptiveLane({ min: 0, max: 3 }));
});
