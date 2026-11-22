package dev.harw.mobile

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
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
}
