package dev.harw.mobile

import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class CloudLibraryImageEditTest {
    private val source = CloudLibraryImageEditIdentity("authority@epoch", 9uL, "asset-1")

    @Test fun requiresAnExactLiveOriginBeforeSendingOrSaving() {
        assertTrue(source.permits("authority@epoch", 9uL, listOf("asset-1"), foreground = true))
        assertFalse(source.permits("other-epoch", 9uL, listOf("asset-1"), foreground = true))
        assertFalse(source.permits("authority@epoch", 10uL, listOf("asset-1"), foreground = true))
        assertFalse(source.permits("authority@epoch", 9uL, listOf("same-name-different-id"), foreground = true))
        assertFalse(source.permits("authority@epoch", 9uL, listOf("asset-1"), foreground = false))
    }

    @Test fun saveLabelNamesSourceAndProviderResultAndStaysWithinGatewayLimit() {
        val digest = "fedcba0123456789".repeat(4)
        val label = source.derivativeName(digest)
        assertTrue(label.contains("asset-1"))
        assertTrue(label.contains("gpt-image-1.5"))
        assertTrue(label.contains(digest.take(16)))
        assertTrue(label.encodeToByteArray().size <= 160)
    }
}
