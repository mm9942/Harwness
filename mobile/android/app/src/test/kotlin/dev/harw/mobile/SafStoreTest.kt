package dev.harw.mobile

import java.security.MessageDigest
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotEquals
import org.junit.Assert.assertTrue
import org.junit.Assert.fail
import org.junit.Test
import uniffi.harw_mobile_ffi.DeviceException

class SafStoreTest {
    private fun assertInvalid(path: String, detail: String) {
        try {
            normalizeSafPath(path)
            fail("expected InvalidPath($detail) for \"$path\"")
        } catch (e: DeviceException.InvalidPath) {
            assertEquals(detail, e.detail)
        }
    }

    @Test
    fun rejectsEscapesWithExactDetails() {
        assertInvalid("..", "dot segment")
        assertInvalid("a/../b", "dot segment")
        assertInvalid("./a", "dot segment")
        assertInvalid("/a", "absolute path")
        assertInvalid("a\\b", "backslash")
        assertInvalid("a//b", "empty segment")
        assertInvalid("a\u0000b", "nul byte")
        assertInvalid("x".repeat(256), "segment too long")
        assertInvalid("abcdefgh/".repeat(113) + "abcdefgh", "path too long")
    }

    @Test
    fun normalizesTrailingSlashAndRoot() {
        assertEquals("", normalizeSafPath(""))
        assertEquals("a", normalizeSafPath("a/"))
        assertEquals("a/b", normalizeSafPath("a/b"))
        assertEquals("a/b", normalizeSafPath("a/b/"))
        assertInvalid("a//", "empty segment")
        assertInvalid("/", "absolute path")
    }

    @Test
    fun rootIdIsStableAndShort() {
        val uri = "content://x/tree/a"
        val id = rootIdFor(uri)
        assertEquals(16, id.length)
        assertTrue(id.all { it in '0'..'9' || it in 'a'..'f' })
        assertEquals(id, rootIdFor(uri))
        assertNotEquals(id, rootIdFor("content://x/tree/b"))

        val digest = MessageDigest.getInstance("SHA-256").digest(uri.toByteArray(Charsets.UTF_8))
        val expected = digest.joinToString("") { "%02x".format(it) }.substring(0, 16)
        assertEquals(expected, id)
    }
}
