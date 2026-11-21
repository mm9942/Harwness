package dev.harw.mobile

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import uniffi.harw_mobile_ffi.ConnectionPhase
import uniffi.harw_mobile_ffi.MobileSession
import uniffi.harw_mobile_ffi.MobileSessionState
import uniffi.harw_mobile_ffi.ServerProfile
import uniffi.harw_mobile_ffi.SessionSnapshot
import uniffi.harw_mobile_ffi.TranscriptEntry
import uniffi.harw_mobile_ffi.TranscriptKind

class WorkspaceRevocationTest {
    private val profile = ServerProfile(1U, "Private label", "127.0.0.1:7410", "server-node", "abcd")

    @Test
    fun invalidationClearsSessionViewsAndPreservesEnrollmentForReconnect() {
        val view = SessionSnapshot(
            sessionId = "session-a", head = null, online = true,
            canSubmit = true, canApprove = true, canInterrupt = false,
            runningTurnId = null,
            transcript = listOf(TranscriptEntry("message-1", TranscriptKind.USER, "Private message", null)),
            liveText = "Private reply", approvals = emptyList(), lastFailure = null,
            lastSyncMillis = null, detail = null,
        )
        val live = WorkspaceState(
            profile = profile, deviceNodeId = "phone-key", setupPending = false, hasSavedSetup = true,
            phase = ConnectionPhase.CONNECTED, canCreate = true, creating = true,
            generation = 5L, sessionsLoaded = true,
            sessions = listOf(MobileSession("session-a", "Private title", MobileSessionState.IDLE, 1U, "", null)),
            windows = mapOf("session-a" to WindowState(view, 5L)),
            pendingActions = setOf(SessionAction("session-a", SessionActionKind.Decide)),
        )
        val revoked = live.invalidated()
        assertEquals(ConnectionPhase.REVOKED, revoked.phase)
        assertEquals(1L, revoked.sessionResetEpoch)
        assertTrue(revoked.sessions.isEmpty())
        assertTrue(revoked.windows.isEmpty())
        assertTrue(revoked.pendingActions.isEmpty())
        assertNull(revoked.generation)
        assertFalse(revoked.canCreate)
        assertFalse(revoked.creating)
        assertTrue(revoked.canSetUp)
        assertFalse(revoked.forgetRequired)
        assertEquals(profile, revoked.profile)
        assertEquals("phone-key", revoked.deviceNodeId)
        assertEquals(1L, revoked.invalidated().sessionResetEpoch)
    }

    @Test
    fun anOrdinaryHandshakeFailureRetainsEnrollmentAndProfile() {
        val setup = WorkspaceState(profile = profile, deviceNodeId = "phone-key", setupPending = false)
        val failed = setup.connectionStatus(ConnectionPhase.FAILED, "Wrong host pin", false, null)
        assertEquals(profile, failed.profile)
        assertEquals("phone-key", failed.deviceNodeId)
        assertEquals(0L, failed.sessionResetEpoch)
        assertFalse(failed.forgetRequired)
        assertTrue(failed.canSetUp)
    }

    @Test
    fun anUnreadableMarkerBlocksSetupWithoutClaimingRevocation() {
        val checkFailed = WorkspaceState().cleanupCheckFailed()
        assertFalse(checkFailed.canSetUp)
        assertFalse(checkFailed.setupPending)
        assertFalse(checkFailed.forgetRequired)
        assertEquals(ConnectionPhase.DISCONNECTED, checkFailed.phase)
        assertEquals(0L, checkFailed.sessionResetEpoch)
        assertTrue(checkFailed.cleanupError?.isNotBlank() == true)
    }

    @Test
    fun explicitResetHidesEnrollmentAndBlocksSetupUntilCleanupCompletes() {
        val setup = WorkspaceState(profile = profile, deviceNodeId = "phone-key", setupPending = false, hasSavedSetup = true)
        assertTrue(setup.canForget)
        assertFalse(setup.copy(phase = ConnectionPhase.CONNECTED).canForget)
        val reset = setup.forgetSetup()
        assertNull(reset.profile)
        assertNull(reset.deviceNodeId)
        assertTrue(reset.forgetRequired)
        assertTrue(reset.forgetting)
        assertFalse(reset.canSetUp)
        assertEquals(1L, reset.sessionResetEpoch)
    }
}
