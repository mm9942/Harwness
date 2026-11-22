package dev.harw.mobile

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test

class MainActivityRouteTest {
    @Test
    fun backClosesSettingsThenSessionThenProject() {
        assertEquals(BackStep.CloseSettings, backStep(true, "s1", "p1"))
        assertEquals(BackStep.CloseSession, backStep(false, "s1", "p1"))
        assertEquals(BackStep.CloseProject, backStep(false, null, "p1"))
        assertEquals(BackStep.Exit, backStep(false, null, null))
    }

    @Test
    fun deepLinkAcceptsProjectAndSession() {
        assertEquals(DeepLink("proj-1", "sess_1"), parseDeepLink("harw", "open", "proj-1", "sess_1"))
        assertEquals(DeepLink(null, "sess_1"), parseDeepLink("harw", "open", null, "sess_1"))
        assertEquals(DeepLink("proj-1", null), parseDeepLink("harw", "open", "proj-1", null))
    }

    @Test
    fun emptyProjectMeansNoProject() {
        assertEquals(DeepLink(NoProjectKey, "s1"), parseDeepLink("harw", "open", "", "s1"))
    }

    @Test
    fun deepLinkRejectsForeignSchemeHostAndEmptyTarget() {
        assertNull(parseDeepLink("https", "open", "p", "s"))
        assertNull(parseDeepLink("harw", "elsewhere", "p", "s"))
        assertNull(parseDeepLink("harw", "open", null, null))
    }

    @Test
    fun deepLinkRejectsHostileIds() {
        assertNull(parseDeepLink("harw", "open", "../x", "s"))
        assertNull(parseDeepLink("harw", "open", "p", "a b"))
        assertNull(parseDeepLink("harw", "open", "p", "s".repeat(65)))
        assertNull(parseDeepLink("harw", "open", "p".repeat(129), "s"))
        assertNull(parseDeepLink("harw", "open", "p", ""))
    }

    @Test
    fun notificationExtraIsValidated() {
        assertEquals(DeepLink(null, "sess-1"), parseOpenSessionExtra("sess-1"))
        assertNull(parseOpenSessionExtra("bad id"))
        assertNull(parseOpenSessionExtra(null))
    }
}
