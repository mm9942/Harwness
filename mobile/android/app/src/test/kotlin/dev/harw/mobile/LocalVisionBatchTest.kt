package dev.harw.mobile

import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.awaitCancellation
import kotlinx.coroutines.cancelAndJoin
import kotlinx.coroutines.launch
import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.withTimeout
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Test

class LocalVisionBatchTest {
    @Test
    fun selectionIsAtomicAndKeepsIdsAfterRemovingMiddleRef() {
        val initial = (1UL..49UL).map { LocalVisionRef(it, "ref$it") }
        val remaining = initial.filter { it.id != 3UL }
        val selected = appendLocalVisionRefs(remaining, listOf("ref2", "new", "new"), 50UL)!!
        assertEquals(remaining + LocalVisionRef(50UL, "new"), selected)
        assertNull(appendLocalVisionRefs(selected, listOf("extra", "extra2"), 51UL))
        assertEquals(selected + LocalVisionRef(51UL, "extra"), appendLocalVisionRefs(selected, listOf("extra"), 51UL))
    }

    @Test
    fun preparesTwoAtATimeSendsOneFrozenBatchAndMapsOutOfOrderPartialResults() = runBlocking {
        withTimeout(5_000) {
            val refs = listOf(16UL, 39UL, 2UL, 59UL, 60UL).map { LocalVisionRef(it, "Image-$it") }
            var active = 0
            var max = 0
            var calls = 0
            val startTwo = CompletableDeferred<Unit>()
            val free = CompletableDeferred<Unit>()
            val updates = mutableMapOf<ULong, LocalVisionItemState>()
            val task = launch {
                runLocalVisionBatch(
                    refs = refs, maxEncodedBytes = 7, prepare = { ref ->
                        active++
                        max = maxOf(max, active)
                        if (active == 2) startTwo.complete(Unit)
                        try {
                            free.await()
                            when (ref.id) {
                                39UL -> null
                                59UL -> LocalVisionPrepared(ref.value, 8)
                                else -> LocalVisionPrepared(ref.value, 3)
                            }
                        } finally { active-- }
                    },
                    describe = { images, receive ->
                        calls++
                        assertEquals(listOf(16UL, 2UL), images.map { it.id })
                        receive(2UL, LocalVisionItemState.Partial("incomplete", LocalVisionPartialCause.ContextLimit))
                        receive(999UL, LocalVisionItemState.Caption("unexpected"))
                        receive(16UL, LocalVisionItemState.Caption("complete"))
                        receive(16UL, LocalVisionItemState.Caption("duplicate"))
                    },
                    update = { id, state -> updates[id] = state },
                )
            }
            startTwo.await()
            assertEquals(2, active)
            free.complete(Unit)
            task.join()
            assertEquals(1, calls)
            assertEquals(2, max)
            assertEquals(
                mapOf(
                    16UL to LocalVisionItemState.Caption("complete"),
                    39UL to LocalVisionItemState.Unreadable,
                    2UL to LocalVisionItemState.Partial("incomplete", LocalVisionPartialCause.ContextLimit),
                    59UL to LocalVisionItemState.SizeLimit,
                    60UL to LocalVisionItemState.SizeLimit,
                ),
                updates,
            )
        }
    }

    @Test
    fun cancellationDoesNotStartNativeAfterCancelledImagePreparation() = runBlocking {
        val started = CompletableDeferred<Unit>()
        var called = false
        val run = launch {
            runLocalVisionBatch<Int, Int>(
                refs = listOf(LocalVisionRef(1UL, 1)), prepare = {
                    started.complete(Unit)
                    awaitCancellation()
                }, describe = { _, _ -> called = true }, update = { _, _ -> },
            )
        }
        started.await()
        run.cancelAndJoin()
        assertFalse(called)
    }

    @Test
    fun retainsCompletedItemsMarksMissingAndDoesNotLeakRawErrors() = runBlocking {
        val updates = mutableMapOf<ULong, LocalVisionItemState>()
        runLocalVisionBatch(
            refs = (1UL..3UL).map { LocalVisionRef(it, it) },
            prepare = { LocalVisionPrepared(it.value, 1) },
            describe = { _, receive ->
                receive(1UL, LocalVisionItemState.Caption("good"))
                receive(3UL, LocalVisionItemState.Partial("", LocalVisionPartialCause.OutputLimit))
                throw IllegalStateException("private")
            },
            update = { id, state -> updates[id] = state },
        )
        assertEquals(
            mapOf(
                1UL to LocalVisionItemState.Caption("good"),
                2UL to LocalVisionItemState.Failed,
                3UL to LocalVisionItemState.Partial("", LocalVisionPartialCause.OutputLimit),
            ),
            updates,
        )
    }
}
