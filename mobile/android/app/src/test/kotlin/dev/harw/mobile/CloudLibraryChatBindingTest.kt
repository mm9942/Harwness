package dev.harw.mobile

import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class CloudLibraryChatBindingTest {
    private val target = CloudLibraryChatTarget("profile", "gateway@epoch", 12L, "chat-a")

    @Test fun attachmentIsOnlyConsumableByTheOriginalLiveForegroundConversation() {
        assertTrue(target.permits("profile", "gateway@epoch", 12L, "chat-a", local = false, ready = true, foreground = true))
        assertFalse(target.permits("other-profile", "gateway@epoch", 12L, "chat-a", false, true, true))
        assertFalse(target.permits("profile", "other-device-epoch", 12L, "chat-a", false, true, true))
        assertFalse(target.permits("profile", "gateway@epoch", 13L, "chat-a", false, true, true))
        assertFalse(target.permits("profile", "gateway@epoch", 12L, "chat-b", false, true, true))
        assertFalse(target.permits("profile", "gateway@epoch", 12L, "chat-a", true, true, true))
        assertFalse(target.permits("profile", "gateway@epoch", 12L, "chat-a", false, false, true))
        assertFalse(target.permits("profile", "gateway@epoch", 12L, "chat-a", false, true, false))
    }

    @Test fun pendingLookupCannotCarryOverAReplacementSelectionDraftOrTranscriptHead() {
        val original = Any()
        val replacement = Any()
        assertTrue(sameCloudLibrarySend(original, original, "draft-a", "draft-a", 3, 3))
        assertFalse(sameCloudLibrarySend(original, replacement, "draft-a", "draft-a", 3, 3))
        assertFalse(sameCloudLibrarySend(original, original, "draft-a", "draft-b", 3, 3))
        assertFalse(sameCloudLibrarySend(original, original, "draft-a", "draft-a", 3, 4))
    }
}
