package dev.harw.mobile

import org.junit.Assert.assertEquals
import org.junit.Test

class VisionConcurrencySettingTest {
    @Test
    fun appliesNativeLimitThenSavesForRestart() {
        val device = Device(1)
        assertEquals(VisionConcurrencyChange.Applied, device.change(3))
        assertEquals(listOf("native:3", "save:3"), device.calls)
        assertEquals(3, device.live)
        assertEquals(3, device.saved)
    }

    @Test
    fun invalidOrUnchangedLimitDoesNotTouchRuntimeOrDisk() {
        val device = Device(2)
        for (invalid in listOf(0, 6, 30)) {
            assertEquals(VisionConcurrencyChange.Invalid, device.change(invalid))
        }
        assertEquals(VisionConcurrencyChange.Unchanged, device.change(2))
        assertEquals(emptyList<String>(), device.calls)
    }

    @Test
    fun nativeRejectionDoesNotSaveAnUnsupportedLimit() {
        val device = Device(1, rejectedLive = 3)
        assertEquals(VisionConcurrencyChange.NativeUnavailable, device.change(3))
        assertEquals(listOf("native:3"), device.calls)
        assertEquals(1, device.saved)
    }

    @Test
    fun failedDiskWriteRestoresTheRuntimeAndPreferencesCache() {
        val device = Device(1, rejectedSave = 3)
        assertEquals(VisionConcurrencyChange.PersistenceFailed, device.change(3))
        assertEquals(listOf("native:3", "save:3", "native:1", "save:1"), device.calls)
        assertEquals(1, device.live)
        assertEquals(1, device.saved)
    }

    @Test
    fun failedNativeRollbackStillAttemptsPreferenceRestorationAndReportsUncertainty() {
        val device = Device(1, rejectedLive = 1, rejectedSave = 3)
        assertEquals(VisionConcurrencyChange.RestoreFailed, device.change(3))
        assertEquals(1, device.saved)
    }

    @Test
    fun passesConfiguredFiveToNativeWithoutPreClampingThePreference() {
        val device = Device(3)
        assertEquals(VisionConcurrencyChange.Applied, device.change(5))
        assertEquals(5, device.saved)
        assertEquals(5, device.live)
        assertEquals(listOf("native:5", "save:5"), device.calls)
        assertEquals(3, DefaultLocalVisionConcurrency)
    }

    private class Device(
        private val previous: Int,
        private val rejectedLive: Int? = null,
        private val rejectedSave: Int? = null,
    ) {
        val calls = mutableListOf<String>()
        var live = previous
            private set
        var saved = previous
            private set

        fun change(requested: Int) = changeVisionConcurrency(
            current = previous,
            requested = requested,
            applyNative = { count ->
                calls += "native:$count"
                (count != rejectedLive).also { if (it) live = count }
            },
            persist = { count ->
                calls += "save:$count"
                // SharedPreferences can update its in-memory value before a failed disk write.
                saved = count
                count != rejectedSave
            },
        )
    }
}
