package dev.harw.mobile

import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class AttachmentReadEpochsTest {
    @Test
    fun reloadDiscardsOnlyTheReadForTheReloadedSession() {
        val epochs = AttachmentReadEpochs()
        val oldA = epochs.begin("a")
        val openingB = epochs.begin("b")

        epochs.invalidate("a")
        val newA = epochs.begin("a")

        assertFalse(epochs.finish("a", oldA))
        assertTrue(epochs.finish("b", openingB))
        assertTrue(epochs.finish("a", newA))
    }
}
