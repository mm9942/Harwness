package dev.harw.mobile

import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicInteger
import kotlinx.coroutines.flow.filterIsInstance
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.withTimeout
import org.junit.Assert.*
import org.junit.Test

class UiaRoomChatConnectionTest {
    private val room = UiaRoomId.parse("uia-local-v1:0123456789abcdef0123456789abcdef")!!
    private val other = UiaRoomId.parse("uia-local-v1:1123456789abcdef0123456789abcdef")!!

    @Test fun selectedModelIsPassedToTheReservedPrivateTurn() = runBlocking {
        val routed = kotlinx.coroutines.CompletableDeferred<Triple<Long, String, String>>()
        val port = object : UiaNativeChat {
            override fun snapshot() = UiaNativeRead.Snapshot(room, 0L, emptyList())
            override fun reserve(attempt: Long) = true
            override fun run(attempt: Long, revision: Long, text: String) = error("unexpected default model")
            override fun runSelected(
                attempt: Long, revision: Long, text: String, image: PrivateLibraryAssistantImage?, modelId: String,
            ): Long {
                routed.complete(Triple(revision, text, modelId))
                return -6L
            }
            override fun cancel(attempt: Long) = Unit
        }
        val connection = UiaRoomChatConnection(port)
        try {
            val initial = withTimeout(3000) { connection.states.filterIsInstance<UiaRoomState.Open>().first() }
            val request = checkNotNull(prepareUiaRoomSend(initial, "Hallo"))
            assertTrue(connection.submit(request, modelId = "local-gguf"))
            assertEquals(Triple(0L, "Hallo", "local-gguf"), withTimeout(3000) { routed.await() })
        } finally {
            connection.onRevoked()
        }
    }

    @Test fun relockSuppressesLateSuccessfulModelAndReload() = runBlocking {
        val entered = CountDownLatch(1)
        val release = CountDownLatch(1)
        val returned = CountDownLatch(1)
        val snapshots = AtomicInteger()
        val port = object : UiaNativeChat {
            override fun snapshot(): UiaNativeRead {
                snapshots.incrementAndGet()
                return UiaNativeRead.Snapshot(room, 0L, emptyList())
            }
            override fun reserve(attempt: Long) = true
            override fun run(attempt: Long, revision: Long, text: String): Long {
                entered.countDown()
                check(release.await(3, TimeUnit.SECONDS))
                returned.countDown()
                return 1L
            }
            override fun cancel(attempt: Long) = Unit
        }
        val connection = UiaRoomChatConnection(port)
        try {
            val initial = withTimeout(3000) { connection.states.filterIsInstance<UiaRoomState.Open>().first() }
            assertTrue(connection.submit(prepareUiaRoomSend(initial, "private draft")!!))
            assertTrue(entered.await(3, TimeUnit.SECONDS))
            connection.onRevoked()
            release.countDown()
            assertTrue(returned.await(3, TimeUnit.SECONDS))
            assertSame(UiaRoomState.Locked, connection.states.value)
            assertEquals(1, snapshots.get())
            assertFalse(connection.submit(prepareUiaRoomSend(initial, "old lease")!!))
        } finally {
            release.countDown()
            connection.onRevoked()
        }
    }

    @Test fun laterSnapshotCannotSwitchRoomIdentity() = runBlocking {
        val reads = AtomicInteger()
        val port = object : UiaNativeChat {
            override fun snapshot() = if (reads.getAndIncrement() == 0) UiaNativeRead.Snapshot(room, 0L, emptyList())
            else UiaNativeRead.Snapshot(other, 1L, listOf(UiaCommittedPair("other","private")))
            override fun reserve(attempt: Long) = false
            override fun run(attempt: Long, revision: Long, text: String) = -1L
            override fun cancel(attempt: Long) = Unit
        }
        val connection = UiaRoomChatConnection(port)
        try {
            withTimeout(3000) { connection.states.filterIsInstance<UiaRoomState.Open>().first() }
            connection.reload()
            val result = withTimeout(3000) { connection.states.filterIsInstance<UiaRoomState.Unavailable>().first() }
            assertEquals(UiaRoomIssue.Vault, result.issue)
        } finally { connection.onRevoked() }
    }
}
