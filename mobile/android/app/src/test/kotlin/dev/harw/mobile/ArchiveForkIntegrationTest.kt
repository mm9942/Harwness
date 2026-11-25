package dev.harw.mobile

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import uniffi.harw_mobile_ffi.SessionCursor

class ArchiveForkIntegrationTest {
    @Test fun observedRevisionUsesDurableCursorOnlyForLiveMatchingSettledAttachment() {
        val head = SessionCursor(11u, 201uL, 5u)
        assertEquals(ArchiveRevision(11u, 201uL), observedArchiveRevision("source", "source", true, false, head))
        assertNull(observedArchiveRevision("source", "other", true, false, head))
        assertNull(observedArchiveRevision("source", "source", false, false, head))
        assertNull(observedArchiveRevision("source", "source", true, true, head))
        assertNull(observedArchiveRevision("source", "source", true, false, null))
        assertNull(observedArchiveRevision("source", "source", true, false, SessionCursor(11u, 0uL, 5u)))
    }

    @Test fun destinationNavigationIsFencedToExactViewProjectAndAttachment() {
        val scope = ArchiveScope(ArchiveBackend.CLOUD, "profile", "server", "source")
        val at = ArchiveAttachment(scope, 4L, true)
        val sheet = Any()
        val route = ArchiveForkRoute("source", "project", at, sheet)
        assertTrue(route.isStillOpen("source", "project", at, sheet))
        assertFalse(route.isStillOpen("destination", "project", at, sheet))
        assertFalse(route.isStillOpen("source", "elsewhere", at, sheet))
        assertFalse(route.isStillOpen("source", "project", at.copy(epoch = 5L), sheet))
        assertFalse(route.isStillOpen("source", "project", at.copy(scope = scope.copy(serverId = "other")), sheet))
        assertFalse(route.isStillOpen("source", "project", at, null))
        assertFalse(route.isStillOpen("source", "project", at, Any()))
    }
}
