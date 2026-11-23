package dev.harw.mobile

import java.io.File
import java.nio.file.Files
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Test

class ModelStoreTest {
    private val docs = File("/docs")
    private val files = File("/files")

    @Test
    fun modelsDirForDocumentsHidden() {
        assertEquals(File("/docs/.harw/models"), modelsDirFor(ModelLocation.DocumentsHidden, docs, files))
    }

    @Test
    fun modelsDirForDocumentsVisible() {
        assertEquals(File("/docs/harw/models"), modelsDirFor(ModelLocation.DocumentsVisible, docs, files))
    }

    @Test
    fun modelsDirForAppPrivate() {
        assertEquals(File("/files/models"), modelsDirFor(ModelLocation.AppPrivate, docs, files))
    }

    @Test
    fun locationFromKnownKeys() {
        assertEquals(ModelLocation.DocumentsHidden, locationFromKey("documents-hidden"))
        assertEquals(ModelLocation.DocumentsVisible, locationFromKey("documents-visible"))
        assertEquals(ModelLocation.AppPrivate, locationFromKey("app-private"))
    }

    @Test
    fun locationFromUnknownOrNullKey() {
        assertNull(locationFromKey("x"))
        assertNull(locationFromKey(null))
    }

    @Test
    fun locationOrderIsCompleteAndStartsHidden() {
        assertEquals(ModelLocation.DocumentsHidden, LocationOrder.first())
        assertEquals(ModelLocation.entries.toSet(), LocationOrder.toSet())
        assertEquals(3, LocationOrder.size)
    }
    @Test
    fun probeFailsWhenItCannotRemoveTheProbeFile() {
        val dir = Files.createTempDirectory("harw-probe-test").toFile()
        try {
            val result = probeModelDirectory(dir) { parent ->
                val file = File.createTempFile(ProbePrefix, null, parent)
                object : File(file.path) {
                    override fun delete(): Boolean = false
                }
            }
            assertNotNull(result)
        } finally {
            dir.deleteRecursively()
        }
    }

    @Test
    fun successfulProbeLeavesNoFileBehind() {
        val dir = Files.createTempDirectory("harw-probe-test").toFile()
        try {
            lateinit var probe: File
            assertNull(probeModelDirectory(dir) { parent ->
                File.createTempFile(ProbePrefix, null, parent).also { probe = it }
            })
            assertFalse(probe.exists())
        } finally {
            dir.deleteRecursively()
        }
    }
}
