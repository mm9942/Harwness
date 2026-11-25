package dev.harw.mobile

import kotlin.time.TimeSource
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.CoroutineStart
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.channels.Channel
import kotlinx.coroutines.coroutineScope
import kotlinx.coroutines.ensureActive
import kotlinx.coroutines.launch
import kotlinx.coroutines.selects.onTimeout
import kotlinx.coroutines.selects.select
import kotlinx.coroutines.sync.Semaphore
import kotlinx.coroutines.sync.withPermit

internal const val ApiVisionMaxRefs = 50
internal const val ApiVisionMinParallelism = 5
private const val ApiVisionDefaultParallelism = 30
private const val ApiVisionMaxDecodes = 2
private const val ApiVisionMaxAttempts = 3
private const val ApiVisionDefaultRetryAfterMs = 1_000uL
private const val ApiVisionMaxRetryAfterMs = 60_000uL
private const val ApiVisionMaxTotalRetryWaitMs = 120_000L
private val apiVisionDecodeGate = Semaphore(ApiVisionMaxDecodes)

internal fun apiVisionConfiguredParallelism(value: Any?): Int =
    (value as? Int)?.takeIf { it in ApiVisionMinParallelism..ApiVisionMaxRefs } ?: ApiVisionDefaultParallelism

internal fun apiVisionWorkerCount(parallelism: Int, refCount: Int): Int =
    minOf(apiVisionConfiguredParallelism(parallelism), refCount.coerceIn(0, ApiVisionMaxRefs))

/** Keep accepted picker order; reject the complete addition rather than silently dropping photos. */
internal fun <T> appendApiVisionRefs(current: List<T>, picked: List<T>): List<T>? {
    val combined = LinkedHashSet(current)
    for (ref in picked) {
        combined.add(ref)
        if (combined.size > ApiVisionMaxRefs) return null
    }
    return combined.toList().takeIf { it.size <= ApiVisionMaxRefs }
}

/** Only the native provider adapter can positively distinguish billing from temporary throttling. */
internal sealed interface ApiVisionReply {
    data class Caption(val text: String) : ApiVisionReply
    data class Throttled(val waitMs: ULong?) : ApiVisionReply
    data object BillingBlocked : ApiVisionReply
    data object Failed : ApiVisionReply
}

internal sealed interface ApiVisionStatus {
    data object Preparing : ApiVisionStatus
    data object Sending : ApiVisionStatus
    data object Unreadable : ApiVisionStatus
    data object Unavailable : ApiVisionStatus
    data object BillingBlocked : ApiVisionStatus
    data object NotSent : ApiVisionStatus
    data object ThrottleUnresolved : ApiVisionStatus
    data class RetryQueued(val attempt: Int) : ApiVisionStatus
    data class Caption(val text: String) : ApiVisionStatus
}

internal enum class ApiVisionPause { Billing, RetryWaitExceeded }

internal data class ApiVisionBatchProgress(
    val configured: Int,
    val effective: Int,
    val active: Int,
    val pause: ApiVisionPause?,
    val coolingDown: Boolean,
)

/** Mutable only on the scheduler owner; no real clock, request, or wait lives here. */
internal class ApiVisionAdmission(parallelism: Int) {
    val configured = apiVisionConfiguredParallelism(parallelism)
    var effective = configured
        private set
    var wave = 0
        private set
    var pause: ApiVisionPause? = null
        private set
    private var cooldownUntil = 0L
    private var budgetUsed = 0L
    private var time = 0L

    fun remainingDelay(now: Long): Long {
        time = maxOf(time, now)
        return (cooldownUntil - time).coerceAtLeast(0)
    }

    fun billBlocked() {
        pause = ApiVisionPause.Billing
    }

    /** Matching-wave 429s share one reduction but may extend the server-defined wait. */
    fun throttled(issuedWave: Int, waitMs: ULong?, now: Long): Boolean {
        if (pause != null) return false
        if (issuedWave == wave) {
            effective = (effective - if (wave == 0) 10 else 5).coerceAtLeast(1)
            wave++
        }
        val wait = waitMs ?: ApiVisionDefaultRetryAfterMs
        val remaining = remainingDelay(now)
        if (wait > ApiVisionMaxRetryAfterMs || time > Long.MAX_VALUE - wait.toLong()) {
            pause = ApiVisionPause.RetryWaitExceeded
            return false
        }
        val extension = (wait.toLong() - remaining).coerceAtLeast(0)
        if (budgetUsed + extension > ApiVisionMaxTotalRetryWaitMs) {
            pause = ApiVisionPause.RetryWaitExceeded
            return false
        }
        budgetUsed += extension
        cooldownUntil = maxOf(cooldownUntil, time + wait.toLong())
        return true
    }
}

private data class ApiVisionAttempt(val index: Int, val attempt: Int = 1)
private sealed interface ApiVisionEvent<out Image> {
    data class Loaded<Image : Any>(val item: ApiVisionAttempt, val image: Image?) : ApiVisionEvent<Image>
    data class Observed(val item: ApiVisionAttempt, val wave: Int, val reply: ApiVisionReply) : ApiVisionEvent<Nothing>
}

/**
 * A single owner controls admission and reads completions before issuing new work. Keep at most two
 * locally prepared images, never more than the effective limit in flight, and don't cancel
 * already-admitted requests when the effective limit falls or billing blocks the route.
 */
@OptIn(ExperimentalCoroutinesApi::class)
internal suspend fun <Ref, Image : Any> runAdaptiveApiVisionBatch(
    refs: List<Ref>,
    parallelism: Int,
    load: suspend (Int, Ref) -> Image?,
    describe: suspend (Image) -> ApiVisionReply,
    update: (Int, ApiVisionStatus) -> Unit,
    progress: (ApiVisionBatchProgress) -> Unit = {},
) = coroutineScope {
    require(refs.size <= ApiVisionMaxRefs)
    val gate = ApiVisionAdmission(parallelism)
    val startedAt = TimeSource.Monotonic.markNow()
    fun now() = startedAt.elapsedNow().inWholeMilliseconds
    val pending = ArrayDeque(refs.indices.map(::ApiVisionAttempt))
    val preparing = ArrayDeque<ApiVisionAttempt>()
    val loaded = mutableMapOf<ApiVisionAttempt, Image?>()
    // Each issued worker posts exactly one completion. No unbounded buffering.
    val events = Channel<ApiVisionEvent<Image>>(ApiVisionMaxRefs + ApiVisionMaxDecodes)
    var active = 0
    var loading = 0
    var latest: ApiVisionBatchProgress? = null

    fun handle(event: ApiVisionEvent<Image>) {
        when (event) {
            is ApiVisionEvent.Loaded -> {
                loading--
                if (gate.pause == null) loaded[event.item] = event.image
            }
            is ApiVisionEvent.Observed -> {
                active--
                val item = event.item
                val status = when (val reply = event.reply) {
                    is ApiVisionReply.Caption -> reply.text.trim().takeIf { it.isNotEmpty() }
                        ?.let(ApiVisionStatus::Caption) ?: ApiVisionStatus.Unavailable
                    ApiVisionReply.Failed -> ApiVisionStatus.Unavailable
                    ApiVisionReply.BillingBlocked -> {
                        gate.billBlocked()
                        ApiVisionStatus.BillingBlocked
                    }
                    is ApiVisionReply.Throttled -> {
                        val mayRetry = gate.throttled(event.wave, reply.waitMs, now())
                        if (mayRetry && item.attempt < ApiVisionMaxAttempts) {
                            val retry = item.copy(attempt = item.attempt + 1)
                            pending.addLast(retry)
                            ApiVisionStatus.RetryQueued(retry.attempt)
                        } else {
                            ApiVisionStatus.ThrottleUnresolved
                        }
                    }
                }
                update(item.index, status)
            }
        }
    }

    while (true) {
        ensureActive()
        val delayMs = if (gate.pause != null) 0L else gate.remainingDelay(now())
        val state = ApiVisionBatchProgress(gate.configured, gate.effective, active, gate.pause, delayMs > 0)
        if (state != latest) {
            latest = state
            progress(state)
        }
        // Prioritize known billing/throttle before starting another network request.
        val signalled = events.tryReceive().getOrNull()
        if (signalled != null) {
            handle(signalled)
            continue
        }
        if (gate.pause != null) {
            for (item in preparing + pending) {
                update(item.index, if (item.attempt == 1) ApiVisionStatus.NotSent else ApiVisionStatus.ThrottleUnresolved)
            }
            preparing.clear()
            pending.clear()
            loaded.clear()
        } else if (delayMs == 0L) {
            val front = preparing.firstOrNull()
            if (front != null && active < gate.effective && loaded.containsKey(front)) {
                preparing.removeFirst()
                val image = loaded.remove(front)
                if (image == null) {
                    update(front.index, ApiVisionStatus.Unreadable)
                } else {
                    active++
                    val wave = gate.wave
                    update(front.index, ApiVisionStatus.Sending)
                    launch(start = CoroutineStart.UNDISPATCHED) {
                        ensureActive()
                        val reply = try {
                            describe(image)
                        } catch (_: CancellationException) {
                            // A nested operation may time out independently; only job cancellation aborts the batch.
                            ensureActive()
                            ApiVisionReply.Failed
                        } catch (_: Exception) {
                            ApiVisionReply.Failed
                        }
                        ensureActive()
                        events.send(ApiVisionEvent.Observed(front, wave, reply))
                    }
                }
                continue
            }
            if (pending.isNotEmpty() && preparing.size < ApiVisionMaxDecodes && active + preparing.size < gate.effective) {
                val item = pending.removeFirst()
                preparing.addLast(item)
                loading++
                update(item.index, ApiVisionStatus.Preparing)
                launch {
                    ensureActive()
                    val image = try {
                        apiVisionDecodeGate.withPermit { load(item.index, refs[item.index]) }
                    } catch (_: CancellationException) {
                        ensureActive()
                        null
                    } catch (_: Exception) {
                        null
                    }
                    ensureActive()
                    events.send(ApiVisionEvent.Loaded(item, image))
                }
                continue
            }
        }
        if (active == 0 && loading == 0 && preparing.isEmpty() && pending.isEmpty()) break
        val event = if (delayMs > 0L) {
            select<ApiVisionEvent<Image>?> {
                events.onReceive { it }
                onTimeout(delayMs) { null }
            }
        } else {
            events.receive()
        }
        if (event != null) handle(event)
    }
}
