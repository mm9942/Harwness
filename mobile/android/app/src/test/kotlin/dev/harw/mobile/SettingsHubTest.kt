package dev.harw.mobile

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import uniffi.harw_mobile_ffi.ConnectionPhase
import uniffi.harw_mobile_ffi.ManageSupport
import uniffi.harw_mobile_ffi.ServerProfile

class SettingsHubTest {
    @Test
    fun parentRoutes() {
        assertNull(settingsParent(SettingsRoute.Hub))
        assertEquals(SettingsRoute.Hub, settingsParent(SettingsRoute.Connection))
        assertEquals(SettingsRoute.Hub, settingsParent(SettingsRoute.Models))
        assertEquals(SettingsRoute.Hub, settingsParent(SettingsRoute.Commands))
    }

    @Test
    fun diagnosticHasNoProfileOrDetail() {
        val profile = ServerProfile(
            version = 1U,
            label = "Mein Testprofil",
            endpoint = "llm.example.test:7410",
            serverNodeId = "node-a",
            serverPublicKeyHex = "01".repeat(1952),
            onion = null,
        )
        val state = WorkspaceState(
            profile = profile,
            phase = ConnectionPhase.CONNECTED,
            detail = "Fehler bei llm.example.test",
            setupNotice = "Hinweis llm.example.test",
            manage = ManageState(
                support = ManageSupport(
                    manage = true, stepup = true, observe = true, steer = true, approve = true, control = false,
                ),
            ),
        )
        val report = diagnosticReport(state, "1.0")
        assertFalse(report.contains("example.test"))
        assertFalse(report.contains("Mein Testprofil"))
        assertTrue(report.contains("app: 1.0"))
        assertTrue(report.contains("verwalten: manage="))
    }

    @Test
    fun imageConcurrencyPrefMigratesBooleanAndPreservesTwo() {
        assertEquals(3, restoredImageConcurrency(null, null))
        assertEquals(1, restoredImageConcurrency(null, false))
        assertEquals(3, restoredImageConcurrency(null, true))
        assertEquals(1, restoredImageConcurrency(1, true))
        assertEquals(2, restoredImageConcurrency(2, false))
        assertEquals(3, restoredImageConcurrency(3, false))
        assertEquals(1, restoredImageConcurrency(0, true))
        assertEquals(4, restoredImageConcurrency(4, true))
        assertEquals(5, restoredImageConcurrency(5, true))
        assertEquals(1, restoredImageConcurrency(6, true))
        assertEquals(1, restoredImageConcurrency("3", true))
    }

    @Test
    fun themeOverrideMapping() {
        assertEquals(false, themeDarkOverride("light"))
        assertEquals(true, themeDarkOverride("dark"))
        assertNull(themeDarkOverride("system"))
        assertNull(themeDarkOverride("x"))
    }
}
