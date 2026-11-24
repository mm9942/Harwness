package dev.harw.mobile

import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.awaitCancellation
import kotlinx.coroutines.cancelAndJoin
import kotlinx.coroutines.launch
import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.withTimeout
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test

class ApiVisionDialogTest {
    @Test
    fun overLimitPickerSelectionIsRejectedWithoutPartiallyAddingOrReordering() {
        val first = (0..46).toList()
        assertEquals((0..49).toList(), appendApiVisionRefs(first, listOf(45, 47, 48, 47, 49)))
        assertNull(appendApiVisionRefs(first, listOf(47, 48, 49, 50)))
        assertEquals(first, appendApiVisionRefs(first, listOf(46, 1, 1)))
    }

    @Test
    fun concurrencyRestoresOwnerRangeOrDefaultAndEffectiveWorkIsBounded() {
        assertEquals(30, apiVisionConfiguredParallelism(null))
        assertEquals(30, apiVisionConfiguredParallelism("5"))
        assertEquals(30, apiVisionConfiguredParallelism(51))
        assertEquals(5, apiVisionConfiguredParallelism(5))
        assertEquals(50, apiVisionConfiguredParallelism(50))
        assertEquals(2, apiVisionWorkerCount(50, 2))
        assertEquals(50, apiVisionWorkerCount(50, 50))
    }

    @Test
    fun batchLimitsDecodeAndRequestsSeparatelyAndKeepsIndexOnReversedCompletions() = runBlocking {
        withTimeout(5_000) {
            val references = (0..8).toList()
            var decoding = 0
            var decodingPeak = 0
            var describing = 0
            var describingPeak = 0
            val twoDecodes = CompletableDeferred<Unit>()
            val allowDecode = CompletableDeferred<Unit>()
            val fiveRequests = CompletableDeferred<Unit>()
            val allowSend = references.associateWith { CompletableDeferred<Unit>() }
            val order = mutableListOf<Int>()
            val results = mutableMapOf<Int, ApiVisionStatus>()
            val batch = launch {
                runAdaptiveApiVisionBatch(references, parallelism = 5, load = { index, ref ->
                    assertEquals(index, ref)
                    decoding++
                    decodingPeak = maxOf(decodingPeak, decoding)
                    if (decoding == 2) twoDecodes.complete(Unit)
                    allowDecode.await()
                    decoding--
                    ref
                }, describe = { image ->
                    describing++
                    describingPeak = maxOf(describingPeak, describing)
                    if (describing == 5) fiveRequests.complete(Unit)
                    allowSend.getValue(image).await()
                    describing--
                    ApiVisionReply.Caption("Caption $image")
                }, update = { i, status ->
                    results[i] = status
                    if (status is ApiVisionStatus.Caption) order += i
                })
            }
            twoDecodes.await()
            assertEquals(2, decoding)
            allowDecode.complete(Unit)
            fiveRequests.await()
            for (index in 8 downTo 0) allowSend.getValue(index).complete(Unit)
            batch.join()
            assertEquals(2, decodingPeak)
            assertEquals(5, describingPeak)
            assertEquals(4, order.first())
            assertEquals(references.map { ApiVisionStatus.Caption("Caption $it") }, references.map { results[it] })
        }
    }

    @Test
    fun unreadableAndRejectedImagesStayIndependentAndAreNeverRetried() = runBlocking {
        val calls = mutableListOf<Int>()
        val results = mutableMapOf<Int, ApiVisionStatus>()
        runAdaptiveApiVisionBatch((0..4).toList(), parallelism = 5, load = { _, ref ->
            when (ref) {
                0 -> throw IllegalStateException("private URI")
                1 -> null
                else -> ref
            }
        }, describe = { image ->
            calls += image
            when (image) {
                2 -> throw IllegalStateException("private endpoint detail")
                3 -> ApiVisionReply.Caption(" ")
                else -> ApiVisionReply.Caption("good")
            }
        }, update = { i, status -> results[i] = status })
        assertEquals(listOf(2, 3, 4), calls.sorted())
        assertEquals(ApiVisionStatus.Unreadable, results[0])
        assertEquals(ApiVisionStatus.Unreadable, results[1])
        assertEquals(ApiVisionStatus.Unavailable, results[2])
        assertEquals(ApiVisionStatus.Unavailable, results[3])
        assertEquals(ApiVisionStatus.Caption("good"), results[4])
    }

    @Test
    fun cancelNeverStartsQueuedRequestsOrProducesAFalseCaption() = runBlocking {
        withTimeout(5_000) {
            val started = CompletableDeferred<Unit>()
            var calls = 0
            val results = mutableMapOf<Int, ApiVisionStatus>()
            val job = launch {
                runAdaptiveApiVisionBatch((0..8).toList(), parallelism = 5, load = { _, ref -> ref },
                    describe = {
                        if (++calls == 5) started.complete(Unit)
                        awaitCancellation()
                    },
                    update = { i, state -> results[i] = state },
                )
            }
            started.await()
            job.cancelAndJoin()
            assertEquals(5, calls)
            assertEquals(List(5) { ApiVisionStatus.Sending }, results.values.toList())
        }
    }
}
