package dev.harw.mobile

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotEquals
import org.junit.Assert.assertTrue
import org.junit.Test
import uniffi.harw_mobile_ffi.MobileSession
import uniffi.harw_mobile_ffi.MobileSessionState

class SessionAlertTrackerTest {
    private fun s(id: String, state: MobileSessionState) = MobileSession(id, null, state, 0U, "", null)

    private fun trackerWith(state: MobileSessionState): SessionAlertTracker =
        SessionAlertTracker().also { it.update(listOf(s("sess-a1", state))) }

    @Test
    fun firstCallIsOnlyBaseline() {
        val (alerts, cleared) = SessionAlertTracker().update(listOf(s("sess-a1", MobileSessionState.RUNNING)))
        assertTrue(alerts.isEmpty())
        assertTrue(cleared.isEmpty())
    }

    @Test
    fun runningToIdleIsTurnEnd() {
        val tracker = trackerWith(MobileSessionState.RUNNING)
        val (alerts, cleared) = tracker.update(listOf(s("sess-a1", MobileSessionState.IDLE)))
        assertEquals(listOf(SessionAlert("sess-a1", SessionAlertKind.TurnEnd)), alerts)
        assertTrue(cleared.isEmpty())
    }

    @Test
    fun runningToWaitingForApprovalIsApproval() {
        val tracker = trackerWith(MobileSessionState.RUNNING)
        val (alerts, cleared) = tracker.update(listOf(s("sess-a1", MobileSessionState.WAITING_FOR_APPROVAL)))
        assertEquals(listOf(SessionAlert("sess-a1", SessionAlertKind.Approval)), alerts)
        assertTrue(cleared.isEmpty())
    }

    @Test
    fun waitingForApprovalToIdleEndsTurnAndClearsApproval() {
        val tracker = trackerWith(MobileSessionState.WAITING_FOR_APPROVAL)
        val (alerts, cleared) = tracker.update(listOf(s("sess-a1", MobileSessionState.IDLE)))
        assertEquals(listOf(SessionAlert("sess-a1", SessionAlertKind.TurnEnd)), alerts)
        assertEquals(listOf("sess-a1"), cleared)
    }

    @Test
    fun unchangedStateYieldsNothing() {
        val tracker = trackerWith(MobileSessionState.RUNNING)
        val (alerts, cleared) = tracker.update(listOf(s("sess-a1", MobileSessionState.RUNNING)))
        assertTrue(alerts.isEmpty())
        assertTrue(cleared.isEmpty())
    }

    @Test
    fun vanishedSessionIsClearedAndNewSessionTriggersNothing() {
        val tracker = trackerWith(MobileSessionState.RUNNING)
        val (alerts, cleared) = tracker.update(listOf(s("sess-a2", MobileSessionState.WAITING_FOR_APPROVAL)))
        assertTrue(alerts.isEmpty())
        assertEquals(listOf("sess-a1"), cleared)
    }

    @Test
    fun idleToFailedYieldsNothing() {
        val tracker = trackerWith(MobileSessionState.IDLE)
        val (alerts, cleared) = tracker.update(listOf(s("sess-a1", MobileSessionState.FAILED)))
        assertTrue(alerts.isEmpty())
        assertTrue(cleared.isEmpty())
    }

    @Test
    fun resetMakesNextCallBaselineAgain() {
        val tracker = trackerWith(MobileSessionState.RUNNING)
        tracker.reset()
        val (alerts, cleared) = tracker.update(listOf(s("sess-a1", MobileSessionState.IDLE)))
        assertTrue(alerts.isEmpty())
        assertTrue(cleared.isEmpty())
    }

    @Test
    fun notificationTagsSeparateBackendsProfilesSessionsAndKinds() {
        val scoped = SessionHomeId(SessionHomeScope(SessionHomeBackend.Cloud, "profile-a"), "sess-a1")
        val approval = SessionAlertTarget(scoped, SessionAlertKind.Approval)
        val original = approval.notificationTag()
        assertEquals(original, approval.copy().notificationTag())
        assertNotEquals(original, approval.copy(kind = SessionAlertKind.TurnEnd).notificationTag())
        assertNotEquals(original, approval.copy(id = scoped.copy(sessionId = "sess-a2")).notificationTag())
        val differentProfile = scoped.copy(scope = scoped.scope.copy(profileId = "profile-b"))
        assertNotEquals(original, approval.copy(id = differentProfile).notificationTag())
        val local = scoped.copy(scope = scoped.scope.copy(backend = SessionHomeBackend.Local))
        assertNotEquals(original, approval.copy(id = local).notificationTag())
    }
}
