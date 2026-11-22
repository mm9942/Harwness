package dev.harw.mobile

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test
import uniffi.harw_mobile_ffi.LocalModel
import uniffi.harw_mobile_ffi.ModelFileState
import uniffi.harw_mobile_ffi.ModelOrigin
import uniffi.harw_mobile_ffi.ModelVerification

class ModelDownloadJobTest {
    private val fake = LocalModel(
        id = "test-model",
        fileName = "test.gguf",
        state = ModelFileState.COMPLETE,
        bytesOnDisk = 10uL,
        sizeBytes = 10uL,
        label = "Test",
        origin = ModelOrigin.CATALOG,
        verification = ModelVerification.VERIFIED,
        sha256 = null,
        path = "/tmp/test.gguf",
    )

    @Test
    fun downloadPercentHandlesBounds() {
        assertEquals(0, downloadPercent(0, 0))
        assertEquals(50, downloadPercent(50, 100))
        assertEquals(100, downloadPercent(200, 100))
    }

    @Test
    fun notificationLineCoversStates() {
        assertEquals("Downloading 41%", notificationLine(ModelDownloadState.Running("m", DownloadPhase.Downloading, 41, 100)))
        assertEquals("Verifying…", notificationLine(ModelDownloadState.Running("m", DownloadPhase.Verifying, 100, 100)))
        assertEquals("Waiting for Wi-Fi", notificationLine(ModelDownloadState.WaitingForNetwork("m")))
        assertEquals("Pinned version, checksum verified", notificationLine(ModelDownloadState.Finished("m", DownloadResult.Done(fake))))
        assertEquals("x", notificationLine(ModelDownloadState.Finished("m", DownloadResult.Failed("x"))))
        assertEquals("", notificationLine(ModelDownloadState.Idle))
    }

    @Test
    fun shouldNotifyThrottles() {
        assertTrue(shouldNotify(0, 5, false))
        assertFalse(shouldNotify(1000, 1500, false))
        assertTrue(shouldNotify(1000, 2000, false))
        assertTrue(shouldNotify(1000, 1100, true))
    }

    @Test
    fun stateAfterMapsResults() {
        assertEquals(ModelDownloadState.Paused("m"), stateAfter("m", DownloadResult.Cancelled, true))
        assertEquals(ModelDownloadState.WaitingForNetwork("m"), stateAfter("m", DownloadResult.Cancelled, false))
        assertEquals(ModelDownloadState.WaitingForNetwork("m"), stateAfter("m", DownloadResult.WaitingForNetwork, true))
        assertEquals(ModelDownloadState.WaitingForNetwork("m"), stateAfter("m", DownloadResult.WaitingForNetwork, false))
        assertEquals(
            ModelDownloadState.Finished("m", DownloadResult.Failed("x")),
            stateAfter("m", DownloadResult.Failed("x"), false),
        )
    }

    @Test
    fun isMeteredAllowedRespectsWifiOnly() {
        assertFalse(isMeteredAllowed(true, true))
        assertTrue(isMeteredAllowed(true, false))
        assertTrue(isMeteredAllowed(false, true))
    }
}
