package dev.harw.mobile

import java.io.File
import java.io.IOException
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertThrows
import org.junit.Assert.assertTrue
import org.junit.Test

class ModelManagerTest {
    @Test
    fun rangeHeaderIsNullForFreshDownload() {
        assertNull(rangeHeader(0L))
    }

    @Test
    fun rangeHeaderRequestsSuffixFromOffset() {
        assertEquals("bytes=1024-", rangeHeader(1024L))
    }

    @Test
    fun partialContentMatchingOffsetAppends() {
        assertEquals(
            ResumeDecision.Append,
            resumeDecision(206, "bytes 100-999/1000", 100L, 1000L),
        )
    }

    @Test
    fun partialContentFromWrongStartFails() {
        assertEquals(
            ResumeDecision.Fail,
            resumeDecision(206, "bytes 0-999/1000", 100L, 1000L),
        )
    }

    @Test
    fun partialContentWithoutContentRangeFails() {
        assertEquals(ResumeDecision.Fail, resumeDecision(206, null, 100L, 1000L))
    }

    @Test
    fun fullResponseRestarts() {
        assertEquals(ResumeDecision.Restart, resumeDecision(200, null, 100L, 1000L))
    }

    @Test
    fun rangeNotSatisfiableAtFullSizeCompletes() {
        assertEquals(ResumeDecision.Complete, resumeDecision(416, null, 1000L, 1000L))
    }

    @Test
    fun rangeNotSatisfiableBeforeFullSizeFails() {
        assertEquals(ResumeDecision.Fail, resumeDecision(416, null, 500L, 1000L))
    }

    @Test
    fun serverErrorFails() {
        assertEquals(ResumeDecision.Fail, resumeDecision(500, null, 0L, 1000L))
    }

    @Test
    fun partialContentFromZeroAppendsLikeFreshDownload() {
        assertEquals(
            ResumeDecision.Append,
            resumeDecision(206, "bytes 0-999/1000", 0L, 1000L),
        )
    }

    @Test
    fun roomExactlyAtBoundaryIsAccepted() {
        val free = 1000L + StorageMarginBytes
        assertTrue(hasRoom(free, 1000L, 0L))
    }

    @Test
    fun oneByteBelowBoundaryIsRejected() {
        val free = 1000L + StorageMarginBytes - 1L
        assertFalse(hasRoom(free, 1000L, 0L))
    }

    @Test
    fun haveBytesBeyondSizeNeedsNoSpaceForFile() {
        assertTrue(hasRoom(StorageMarginBytes, 1000L, 5000L))
        assertFalse(hasRoom(StorageMarginBytes - 1L, 1000L, 5000L))
    }

    @Test
    fun alreadyDownloadedBytesReduceRequiredSpace() {
        val free = 100L + StorageMarginBytes
        assertTrue(hasRoom(free, 1000L, 900L))
        assertFalse(hasRoom(free - 1L, 1000L, 900L))
    }

    private val hash = "ab".repeat(32)
    private val base = java.net.URL("https://cdn.example/models/a.gguf")

    @Test
    fun constantsHaveExpectedValues() {
        assertEquals(5, MaxRedirects)
        assertEquals(100L * 1024 * 1024, StorageMarginBytes)
    }

    @Test
    fun redirectStatusesAreRecognised() {
        for (code in listOf(301, 302, 303, 307, 308)) assertTrue(code.toString(), isRedirect(code))
        for (code in listOf(200, 206, 300, 304, 305, 306, 309, 404, 500)) {
            assertFalse(code.toString(), isRedirect(code))
        }
    }

    @Test
    fun resolveRedirectFollowsAbsoluteHttps() {
        assertEquals(
            "https://other.example/x",
            resolveRedirect(base, "https://other.example/x")?.toString(),
        )
    }

    @Test
    fun resolveRedirectResolvesRelativeLocation() {
        assertEquals(
            "https://cdn.example/models/b.gguf",
            resolveRedirect(base, "b.gguf")?.toString(),
        )
        assertEquals(
            "https://cdn.example/z",
            resolveRedirect(base, "/z")?.toString(),
        )
    }

    @Test
    fun resolveRedirectTrimsWhitespace() {
        assertEquals(
            "https://cdn.example/y",
            resolveRedirect(base, "  /y \n")?.toString(),
        )
    }

    @Test
    fun resolveRedirectRejectsMissingOrBlank() {
        assertNull(resolveRedirect(base, null))
        assertNull(resolveRedirect(base, ""))
        assertNull(resolveRedirect(base, "   "))
    }

    @Test
    fun resolveRedirectRejectsHttpDowngrade() {
        assertNull(resolveRedirect(base, "http://cdn.example/x"))
    }

    @Test
    fun resolveRedirectRejectsUserInfo() {
        assertNull(resolveRedirect(base, "https://user:pw@cdn.example/x"))
    }

    @Test
    fun resolveRedirectRejectsOtherSchemesAndMalformed() {
        assertNull(resolveRedirect(base, "ftp://cdn.example/x"))
        assertNull(resolveRedirect(base, "foo://cdn.example/x"))
    }

    @Test
    fun resolveRedirectRejectsEmptyHost() {
        assertNull(resolveRedirect(base, "https:///x"))
    }

    @Test
    fun linkedEtagNullHeaderIsUnknown() {
        assertNull(linkedEtagMatches(null, hash))
    }

    @Test
    fun linkedEtagMatchesQuotedWeakAndCase() {
        assertEquals(true, linkedEtagMatches(hash, hash))
        assertEquals(true, linkedEtagMatches("\"$hash\"", hash))
        assertEquals(true, linkedEtagMatches("W/\"$hash\"", hash))
        assertEquals(true, linkedEtagMatches(" \"${hash.uppercase()}\" ", hash))
        assertEquals(true, linkedEtagMatches(hash, hash.uppercase()))
    }

    @Test
    fun linkedEtagMismatchIsFalse() {
        assertEquals(false, linkedEtagMatches("\"" + "cd".repeat(32) + "\"", hash))
        assertEquals(false, linkedEtagMatches("", hash))
    }

    @Test
    fun linkedSizeNullHeaderIsUnknown() {
        assertNull(linkedSizeMatches(null, 1000L))
    }

    @Test
    fun linkedSizeMatchesPlainAndQuoted() {
        assertEquals(true, linkedSizeMatches("1000", 1000L))
        assertEquals(true, linkedSizeMatches(" 1000 ", 1000L))
        assertEquals(true, linkedSizeMatches("\"1000\"", 1000L))
    }

    @Test
    fun linkedSizeMismatchOrGarbageIsFalse() {
        assertEquals(false, linkedSizeMatches("999", 1000L))
        assertEquals(false, linkedSizeMatches("abc", 1000L))
        assertEquals(false, linkedSizeMatches("", 1000L))
    }
    @Test
    fun oversizedPartialWithFailedDeleteIsNotEligibleForDownload() {
        val part = StagedPart(bytes = 101L, deleteSucceeded = false)
        assertThrows(IOException::class.java) { initialPartialOffset(part, 100L) }
        assertEquals(1, part.deleteAttempts)
    }

    @Test
    fun oversizedPartialRestartsAfterPassingCleanup() {
        val part = StagedPart(bytes = 101L, deleteSucceeded = true)
        assertEquals(0L, initialPartialOffset(part, 100L))
        assertEquals(1, part.deleteAttempts)
    }

    @Test
    fun validPartialIsResumedWithoutDeletion() {
        val part = StagedPart(bytes = 25L, deleteSucceeded = false)
        assertEquals(25L, initialPartialOffset(part, 100L))
        assertEquals(0, part.deleteAttempts)
    }

    @Test
    fun nonRegularPartialIsNotEligibleForDownload() {
        val part = StagedPart(bytes = 0L, deleteSucceeded = false, isRegular = false)
        assertThrows(IOException::class.java) { initialPartialOffset(part, 100L) }
        assertEquals(0, part.deleteAttempts)
    }

    private class StagedPart(
        private val bytes: Long,
        private val deleteSucceeded: Boolean,
        private val isRegular: Boolean = true,
    ) : File("test-model.gguf.part") {
        var deleteAttempts = 0
            private set

        override fun exists(): Boolean = true
        override fun isFile(): Boolean = isRegular
        override fun length(): Long = bytes
        override fun delete(): Boolean {
            deleteAttempts++
            return deleteSucceeded
        }
    }
}
