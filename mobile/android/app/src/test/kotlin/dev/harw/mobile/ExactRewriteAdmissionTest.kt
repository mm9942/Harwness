package dev.harw.mobile

import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test
import uniffi.harw_mobile_ffi.SessionCursor

class ExactRewriteAdmissionTest {
    @Test
    fun optimisticSendEchoNeverBecomesARewriteTarget() {
        assertFalse(isDurableRewriteItemId("mobile-04950b37"))
        assertFalse(isDurableRewriteItemId("legacy item with spaces"))
        assertTrue(isDurableRewriteItemId("01JY42WVSAQZZSPH6K9A7F2FX3"))
    }

    @Test
    fun transientLiveUpdatesDoNotResolveAnUncertainRewrite() {
        val previous = SessionCursor(7u, 42uL, 1u)
        val liveUpdate = SessionCursor(7u, 42uL, 77u)
        assertTrue(liveUpdate.hasSameRewriteBoundary(previous))
        assertFalse(liveUpdate.isAfterRewriteBoundary(previous))
        assertFalse(SessionCursor(7u, 41uL, 0u).isAfterRewriteBoundary(previous))
        assertTrue(SessionCursor(7u, 43uL, 0u).isAfterRewriteBoundary(previous))
        assertTrue(SessionCursor(8u, 1uL, 0u).isAfterRewriteBoundary(previous))
    }
}
