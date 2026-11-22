package dev.harw.mobile

import org.junit.Assert.assertEquals
import org.junit.Assert.fail
import org.junit.Test
import uniffi.harw_mobile_ffi.DeviceException

class TermuxBridgeTest {
    @Test
    fun clampTimeoutDefaultsAndCaps() {
        assertEquals(30_000L, clampTimeout(0L))
        assertEquals(30_000L, clampTimeout(-5L))
        assertEquals(500L, clampTimeout(500L))
        assertEquals(120_000L, clampTimeout(999_999L))
    }

    @Test
    fun clampOutputDefaultsAndCaps() {
        assertEquals(65_536, clampOutput(0))
        assertEquals(65_536, clampOutput(70_000))
        assertEquals(100, clampOutput(100))
    }

    @Test
    fun emptyArgvIsRejected() {
        rejectsInvalid("empty command") { checkShellArgv(emptyList(), null) }
    }

    @Test
    fun tooManyArgsIsRejected() {
        rejectsInvalid("too many arguments") { checkShellArgv(List(65) { "a" }, null) }
    }

    @Test
    fun nulAndOversizedArgsAreRejected() {
        rejectsInvalid("invalid argument") { checkShellArgv(listOf("ls", "a\u0000b"), null) }
        rejectsInvalid("invalid argument") { checkShellArgv(listOf("ls", "x".repeat(4097)), null) }
        rejectsInvalid("invalid argument") { checkShellArgv(listOf(""), null) }
    }

    @Test
    fun cwdRules() {
        checkShellArgv(listOf("ls"), null)
        checkShellArgv(listOf("ls"), "work/dir")
        checkShellArgv(listOf("ls"), "/data/data/com.termux/files/home")
        rejectsInvalidPath("working directory outside Termux home") {
            checkShellArgv(listOf("ls"), "/data/data/com.termux/files/../x")
        }
        rejectsAnyInvalidPath { checkShellArgv(listOf("ls"), "/etc") }
        rejectsAnyInvalidPath { checkShellArgv(listOf("ls"), "../x") }
    }

    @Test
    fun capOutputKeepsShortText() {
        assertEquals(Pair("hello", false), capOutput("hello", 100))
    }

    @Test
    fun capOutputCutsOnCodePointBoundary() {
        assertEquals(Pair("a", true), capOutput("aä", 2))
        assertEquals(Pair("", true), capOutput("😀", 3))
        assertWithinBudget("aä😀b", 4)
        assertWithinBudget("😀😀", 5)
    }

    @Test
    fun capOutputZeroBudget() {
        assertEquals(Pair("", true), capOutput("abc", 0))
    }

    private fun assertWithinBudget(text: String, maxBytes: Int) {
        val (kept, truncated) = capOutput(text, maxBytes)
        val size = kept.toByteArray(Charsets.UTF_8).size
        if (size > maxBytes) fail("kept $size bytes, budget $maxBytes")
        if (kept.contains('�')) fail("replacement character in output")
        assertEquals(true, truncated)
    }

    private fun rejectsInvalid(detail: String, block: () -> Unit) {
        try {
            block()
        } catch (e: DeviceException.Invalid) {
            assertEquals(detail, e.detail)
            return
        }
        fail("expected DeviceException.Invalid with detail: $detail")
    }

    private fun rejectsInvalidPath(detail: String, block: () -> Unit) {
        try {
            block()
        } catch (e: DeviceException.InvalidPath) {
            assertEquals(detail, e.detail)
            return
        }
        fail("expected DeviceException.InvalidPath with detail: $detail")
    }

    private fun rejectsAnyInvalidPath(block: () -> Unit) {
        try {
            block()
        } catch (e: DeviceException.InvalidPath) {
            return
        }
        fail("expected DeviceException.InvalidPath")
    }
}
