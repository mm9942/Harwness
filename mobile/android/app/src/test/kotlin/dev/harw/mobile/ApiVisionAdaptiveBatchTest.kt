package dev.harw.mobile

import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.launch
import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.withTimeout
import kotlinx.coroutines.yield
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class ApiVisionAdaptiveBatchTest {
    @Test
    fun confirmedThrottleLowersOnlyLaterAdmissionsAndRequeuesBehindUnsentImages() = runBlocking {
        withTimeout(5_000) {
            val refs = (0..16).toList()
            val inFlightAtStart = CompletableDeferred<Unit>()
            val confirmedThrottle = CompletableDeferred<Unit>()
            val nineFinished = CompletableDeferred<Unit>()
            val sends = mutableListOf<Int>()
            val gates = (0..14).associateWith { CompletableDeferred<ApiVisionReply>() }
            val progress = mutableListOf<ApiVisionBatchProgress>()
            val job = launch {
                runAdaptiveApiVisionBatch(
                    refs, 15, { _, ref -> ref },
                    { index ->
                        sends += index
                        if (sends.size == 15) inFlightAtStart.complete(Unit)
                        if (index in gates && sends.count { it == index } == 1) gates.getValue(index).await()
                        else ApiVisionReply.Caption("image $index")
                    },
                    { index, status ->
                        if (index == 0 && status == ApiVisionStatus.RetryQueued(2)) confirmedThrottle.complete(Unit)
                        if (index == 9 && status is ApiVisionStatus.Caption) nineFinished.complete(Unit)
                    },
                    { progress += it },
                )
            }
            inFlightAtStart.await()
            gates.getValue(0).complete(ApiVisionReply.Throttled(0uL))
            confirmedThrottle.await()
            for (index in 1..9) gates.getValue(index).complete(ApiVisionReply.Caption("image $index"))
            nineFinished.await()
            yield()
            assertEquals((0..14).toList(), sends)
            assertTrue(progress.any { it.configured == 15 && it.effective == 5 && it.active == 14 })
            for (index in 10..14) gates.getValue(index).complete(ApiVisionReply.Caption("image $index"))
            job.join()
            assertEquals((0..16).toList() + 0, sends)
        }
    }

    @Test
    fun billingStopsUnsentRequestsButKeepsAlreadyIssuedResultsAndLimit() = runBlocking {
        withTimeout(5_000) {
            val fiveSent = CompletableDeferred<Unit>()
            val blocked = CompletableDeferred<Unit>()
            val sends = mutableListOf<Int>()
            val results = mutableMapOf<Int, ApiVisionStatus>()
            val replies = (0..4).associateWith { CompletableDeferred<ApiVisionReply>() }
            var last: ApiVisionBatchProgress? = null
            val job = launch {
                runAdaptiveApiVisionBatch(
                    (0..9).toList(), 5, { _, image -> image },
                    { index ->
                        sends += index
                        if (sends.size == 5) fiveSent.complete(Unit)
                        replies[index]?.await() ?: ApiVisionReply.Caption("unexpected")
                    },
                    { index, status -> results[index] = status },
                    {
                        last = it
                        if (it.pause == ApiVisionPause.Billing) blocked.complete(Unit)
                    },
                )
            }
            fiveSent.await()
            replies.getValue(0).complete(ApiVisionReply.BillingBlocked)
            blocked.await()
            val duringPause = checkNotNull(last)
            assertEquals(5, duringPause.effective)
            assertEquals(4, duringPause.active)
            for (index in 1..4) replies.getValue(index).complete(ApiVisionReply.Caption("result $index"))
            job.join()
            assertEquals((0..4).toList(), sends)
            assertEquals(ApiVisionStatus.BillingBlocked, results[0])
            assertEquals(ApiVisionStatus.Caption("result 4"), results[4])
            for (index in 5..9) assertEquals(ApiVisionStatus.NotSent, results[index])
        }
    }

    @Test
    fun aPhotoAlreadyPreparingWhenBillingBlocksIsNeverSent() = runBlocking {
        withTimeout(5_000) {
            val nextDecodeStarted = CompletableDeferred<Unit>()
            val decodeMayFinish = CompletableDeferred<Unit>()
            val blockObserved = CompletableDeferred<Unit>()
            val sends = mutableListOf<Int>()
            val statuses = mutableMapOf<Int, ApiVisionStatus>()
            val job = launch {
                runAdaptiveApiVisionBatch(
                    (0..5).toList(), 5,
                    { _, ref ->
                        if (ref == 1) {
                            nextDecodeStarted.complete(Unit)
                            decodeMayFinish.await()
                        }
                        ref
                    },
                    { ref ->
                        sends += ref
                        nextDecodeStarted.await()
                        ApiVisionReply.BillingBlocked
                    },
                    { index, status -> statuses[index] = status },
                    { if (it.pause == ApiVisionPause.Billing) blockObserved.complete(Unit) },
                )
            }
            blockObserved.await()
            decodeMayFinish.complete(Unit)
            job.join()
            assertEquals(listOf(0), sends)
            assertEquals(ApiVisionStatus.NotSent, statuses[1])
        }
    }

    @Test
    fun throttleAttemptsAreBoundedAndUnknownDeliveryIsNeverRetried() = runBlocking {
        val sends = mutableMapOf<Int, Int>()
        val results = mutableMapOf<Int, ApiVisionStatus>()
        runAdaptiveApiVisionBatch(
            listOf(0, 1), 5, { _, ref -> ref },
            { index ->
                sends[index] = (sends[index] ?: 0) + 1
                if (index == 0) ApiVisionReply.Throttled(0uL) else ApiVisionReply.Failed
            },
            { index, status -> results[index] = status },
        )
        assertEquals(mapOf(0 to 3, 1 to 1), sends)
        assertEquals(ApiVisionStatus.ThrottleUnresolved, results[0])
        assertEquals(ApiVisionStatus.Unavailable, results[1])
    }

    @Test
    fun retryAfterBeyondAutomaticBudgetPausesInsteadOfRetryingEarly() = runBlocking {
        val sends = mutableListOf<Int>()
        var last: ApiVisionBatchProgress? = null
        runAdaptiveApiVisionBatch(
            listOf(0), 30, { _, ref -> ref },
            { index -> sends += index; ApiVisionReply.Throttled(ULong.MAX_VALUE) },
            { _, _ -> },
            { last = it },
        )
        assertEquals(listOf(0), sends)
        val terminal = checkNotNull(last)
        assertEquals(ApiVisionPause.RetryWaitExceeded, terminal.pause)
        assertEquals(20, terminal.effective)
    }

    @Test
    fun separatelyCancelledLoadOrRequestStillSettlesTheJob() = runBlocking {
        withTimeout(2_000) {
            val statuses = mutableMapOf<Int, ApiVisionStatus>()
            runAdaptiveApiVisionBatch(
                (0..2).toList(), 5,
                { _, index -> if (index == 0) throw CancellationException("load abandoned") else index },
                { index ->
                    if (index == 1) throw CancellationException("request abandoned")
                    ApiVisionReply.Caption("ok")
                },
                { index, status -> statuses[index] = status },
            )
            assertEquals(ApiVisionStatus.Unreadable, statuses[0])
            assertEquals(ApiVisionStatus.Unavailable, statuses[1])
            assertEquals(ApiVisionStatus.Caption("ok"), statuses[2])
        }
    }

    @Test
    fun sameWaveExtendsWaitWithoutLoweringAgainAndLaterWaveDropsAtMostFive() {
        val gate = ApiVisionAdmission(30)
        val firstWave = gate.wave
        assertTrue(gate.throttled(firstWave, 1_000uL, 10))
        assertEquals(20, gate.effective)
        assertEquals(911L, gate.remainingDelay(99))
        assertTrue(gate.throttled(firstWave, 2_000uL, 100))
        assertEquals(20, gate.effective)
        assertEquals(1_000L, gate.remainingDelay(1_100))
        assertEquals(0L, gate.remainingDelay(2_100))
        assertTrue(gate.throttled(gate.wave, 0uL, 3_000))
        assertEquals(15, gate.effective)
        assertEquals(30, gate.configured)
    }

    @Test
    fun cumulativeWaitBudgetNeverShortensAConfirmedProviderWait() {
        val gate = ApiVisionAdmission(30)
        assertTrue(gate.throttled(gate.wave, 60_000uL, 0))
        assertTrue(gate.throttled(gate.wave, 60_000uL, 60_000))
        assertFalse(gate.throttled(gate.wave, 1uL, 120_000))
        assertEquals(ApiVisionPause.RetryWaitExceeded, gate.pause)
    }
}
