package dev.harw.mobile

import java.io.File
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test
import org.junit.rules.TemporaryFolder

class AtomicFileStatusTest {
    @get:Rule
    val folder = TemporaryFolder()

    @Test
    fun interruptedFirstWriteStillCountsAsAPendingMarker() {
        val marker = File(folder.root, "setup-reset.pending")
        assertFalse(atomicFileExists(marker))
        File(folder.root, "setup-reset.pending.new").createNewFile()
        assertTrue(atomicFileExists(marker))
    }

    @Test
    fun legacyBackupCountsAsAPendingMarker() {
        val marker = File(folder.root, "setup-reset.pending")
        File(folder.root, "setup-reset.pending.bak").createNewFile()
        assertTrue(atomicFileExists(marker))
    }
}
