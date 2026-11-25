package dev.harw.mobile

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class ModelImportTest {
    @Test
    fun stagingNameIsLowercaseHex() {
        assertEquals(
            ".harw-import-0001020304050607.gguf.part",
            stagingName(ByteArray(8) { it.toByte() }),
        )
        assertEquals(
            ".harw-import-ffffffffffffffff.gguf.part",
            stagingName(ByteArray(8) { -1 }),
        )
    }

    @Test
    fun candidateModelDirDocIdsCoversPickedFolders() {
        assertEquals(
            listOf("primary:Documents/.harw/models", "primary:Documents/harw/models"),
            candidateModelDirDocIds("primary:Documents"),
        )
        assertEquals(
            listOf("primary:Documents/.harw/models"),
            candidateModelDirDocIds("primary:Documents/.harw"),
        )
        assertEquals(
            listOf("primary:Documents/harw/models"),
            candidateModelDirDocIds("primary:Documents/harw"),
        )
        assertEquals(
            listOf("primary:Documents/.harw/models"),
            candidateModelDirDocIds("primary:Documents/.harw/models"),
        )
        assertEquals(
            listOf("primary:Download/x"),
            candidateModelDirDocIds("primary:Download/x"),
        )
    }

    @Test
    fun alternateNameAppendsCounterToStem() {
        assertEquals(
            "Qwen3.5-4B-Q4_K_M-2.gguf",
            alternateName("Qwen3.5-4B-Q4_K_M.gguf", 2),
        )
    }

    @Test
    fun isGgufNameChecksSuffixAndHidden() {
        assertTrue(isGgufName("a.GGUF"))
        assertFalse(isGgufName(".a.gguf"))
        assertFalse(isGgufName("a.gguf.json"))
    }
}
