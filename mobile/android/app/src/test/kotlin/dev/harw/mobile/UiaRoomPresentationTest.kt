package dev.harw.mobile

import org.junit.Assert.*
import org.junit.Test

class UiaRoomPresentationTest {
    private val roomId = UiaRoomId.parse("uia-local-v1:0123456789abcdef0123456789abcdef")!!

    @Test fun roomIdCannotBeARegularCodingSessionOrForgedPath() {
        assertNull(UiaRoomId.parse("session/0123456789abcdef0123456789abcdef"))
        assertNull(UiaRoomId.parse("uia-local-v1:../0123456789abcdef0123456789abc"))
        assertNull(UiaRoomId.parse("uia-local-v1:00000000000000000000000000000000"))
        assertNull(UiaRoomId.parse("uia-local-v1:0123456789ABCDEF0123456789ABCDEF"))
        assertEquals(roomId, UiaRoomId.parse("uia-local-v1:0123456789abcdef0123456789abcdef"))
    }

    @Test fun lockedLoadingAndProcessingNeverAdmitASubmission() {
        val lease = UiaRoomUnlockLease(roomId)
        val snapshot = UiaRoomSnapshot.fromCore(lease, 0, emptyList())!!
        assertNull(prepareUiaRoomSend(UiaRoomState.Locked, "hello"))
        assertNull(prepareUiaRoomSend(UiaRoomState.Opening, "hello"))
        assertNull(prepareUiaRoomSend(UiaRoomState.Unavailable(UiaRoomIssue.Vault), "hello"))
        assertNull(prepareUiaRoomSend(UiaRoomState.Open(snapshot, UiaRoomWork.Processing(UiaRoomAttempt())), "hello"))
    }

    @Test fun sendBindsTheExactUnlockAndRevisionAndDoesNotAppendOptimistically() {
        val original = UiaRoomUnlockLease(roomId)
        val fresh = UiaRoomUnlockLease(roomId)
        val snapshot = UiaRoomSnapshot.fromCore(original, 1, listOf(UiaCommittedPair("question", "answer")))!!
        val state = UiaRoomState.Open(snapshot, UiaRoomWork.Idle)
        val request = prepareUiaRoomSend(state, "  next\n")!!
        assertSame(original, request.lease)
        assertNotEquals(fresh, request.lease)
        assertEquals(1L, request.expectedRevision)
        assertEquals("next", request.text)
        assertEquals(1, state.snapshot.pairs.size)
        assertFalse(request.toString().contains("next"))
    }

    @Test fun staleComposedButtonCannotSendAfterLockOrReopeningSameRoom() {
        val lease = UiaRoomUnlockLease(roomId)
        val snapshot = UiaRoomSnapshot.fromCore(lease, 0, emptyList())!!
        val request = prepareUiaRoomSend(UiaRoomState.Open(snapshot, UiaRoomWork.Idle), "private text")!!
        val newUnlock = UiaRoomSnapshot.fromCore(UiaRoomUnlockLease(roomId), 0, emptyList())!!
        assertFalse(request.isFor(UiaRoomState.Locked))
        assertFalse(request.isFor(UiaRoomState.Open(newUnlock, UiaRoomWork.Idle)))
        assertFalse(request.isFor(UiaRoomState.Open(snapshot, UiaRoomWork.Processing(UiaRoomAttempt()))))
        assertTrue(request.isFor(UiaRoomState.Open(snapshot, UiaRoomWork.Idle)))
    }

    @Test fun staleCancelCannotInterruptAnotherTurnAtTheSameRevision() {
        val lease = UiaRoomUnlockLease(roomId)
        val snapshot = UiaRoomSnapshot.fromCore(lease, 0, emptyList())!!
        val state = UiaRoomState.Open(snapshot, UiaRoomWork.Processing(UiaRoomAttempt()))
        val replacement = UiaRoomState.Open(snapshot, UiaRoomWork.Processing(UiaRoomAttempt()))
        val cancel = prepareUiaRoomCancel(state)!!
        assertTrue(cancel.isFor(state))
        assertFalse(cancel.isFor(replacement))
        assertFalse(cancel.isFor(UiaRoomState.Locked))
    }

    @Test fun failedTurnsMayBeRetriedExplicitlyButUnhealthyVaultsCannotSend() {
        val lease = UiaRoomUnlockLease(roomId)
        val snapshot = UiaRoomSnapshot.fromCore(lease, 0, emptyList())!!
        assertNotNull(prepareUiaRoomSend(UiaRoomState.Open(snapshot, UiaRoomWork.Failed(UiaRoomRetryable.Incomplete)), "retry"))
        assertNull(prepareUiaRoomSend(UiaRoomState.Unavailable(UiaRoomIssue.Vault), "retry"))
    }

    @Test fun validatesUtf8BeforePassingToNative() {
        val lease = UiaRoomUnlockLease(roomId)
        val state = UiaRoomState.Open(UiaRoomSnapshot.fromCore(lease, 0, emptyList())!!, UiaRoomWork.Idle)
        assertNotNull(prepareUiaRoomSend(state, "😀".repeat(2048)))
        assertNull(prepareUiaRoomSend(state, "😀".repeat(2049)))
        assertNull(prepareUiaRoomSend(state, "\uD800"))
        assertNull(prepareUiaRoomSend(state, "private\u0000message"))
        assertNull(prepareUiaRoomSend(state, " \r\n\t"))
    }

    @Test fun malformedCoreSnapshotIsNotTreatedLikeAnEmptyRoom() {
        val lease = UiaRoomUnlockLease(roomId)
        val source = mutableListOf(UiaCommittedPair("first question", "first answer"))
        assertNull(UiaRoomSnapshot.fromCore(lease, 2, source))
        val snapshot = UiaRoomSnapshot.fromCore(lease, 1, source)!!
        source.clear()
        assertEquals(1, snapshot.pairs.size)
        assertNull(UiaRoomSnapshot.fromCore(lease, 1, listOf(UiaCommittedPair("valid", ""))))
        val maxPair = UiaCommittedPair("x".repeat(8192), "y".repeat(16384))
        assertNull(UiaRoomSnapshot.fromCore(lease, 11, List(11) { maxPair }))
    }
}
