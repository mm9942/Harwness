package dev.harw.mobile

import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class DreamSessionVisibilityTest {
    @Test
    fun legacyRequiresBothGeneratedIdAndTheFullStablePromptOpening() {
        val prefix = "Du träumst (Leerlauf (15 min)). Konsolidiere still, was zuletzt wichtig war, welche offenen Fäden bleiben"
        assertTrue(suppressLegacyDreamRow("dream-20261123T174501", prefix))
        assertTrue(suppressLegacyDreamRow("dream-20261123T174501-2", prefix))
        assertFalse(suppressLegacyDreamRow("ordinary-session", prefix))
        assertFalse(suppressLegacyDreamRow("dream-20261123T174501", "Du träumst heute wieder."))
        assertFalse(suppressLegacyDreamRow("dream-20261123T174501", null))
    }
}
