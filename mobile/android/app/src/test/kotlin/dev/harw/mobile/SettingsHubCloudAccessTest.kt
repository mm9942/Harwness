package dev.harw.mobile

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotEquals
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import uniffi.harw_mobile_ffi.ConnectionPhase
import uniffi.harw_mobile_ffi.ManageSupport
import uniffi.harw_mobile_ffi.ServerProfile

class SettingsHubCloudAccessTest {
    private val full = ManageSupport(
        manage = true,
        stepup = true,
        observe = true,
        steer = false,
        approve = true,
        control = true,
    )
    private val connected = WorkspaceState(
        profile = ServerProfile(
            version = 1U,
            label = "Test",
            endpoint = "test.example:7410",
            serverNodeId = "node-test",
            serverPublicKeyHex = "01".repeat(1952),
            onion = null,
        ),
        phase = ConnectionPhase.CONNECTED,
        setupPending = false,
        manage = ManageState(support = full),
    )

    @Test
    fun offlineAndConnectingGiveDifferentExplanations() {
        val offline = manageOpenBlock(connected.copy(phase = ConnectionPhase.DISCONNECTED))
        val connecting = manageOpenBlock(connected.copy(phase = ConnectionPhase.CONNECTING))
        assertNotNull(offline)
        assertNotNull(connecting)
        assertNotEquals(offline, connecting)
    }

    @Test
    fun profileSetupAndServerCapabilityCheckingAreNotUnsupported() {
        val checking = manageOpenBlock(connected.copy(manage = ManageState(support = null)))
        val unsupported = manageOpenBlock(connected.copy(manage = ManageState(support = full.copy(manage = false))))
        val setup = manageOpenBlock(connected.copy(setupPending = true, profile = null))
        assertNotNull(checking)
        assertNotNull(unsupported)
        assertNotNull(setup)
        assertNotEquals(unsupported, checking)
        assertNotEquals(unsupported, setup)
    }

    @Test
    fun managementRowsRequireCloudProfileAndObserve() {
        assertNull(manageOpenBlock(connected))
        assertNotNull(manageOpenBlock(connected.copy(profile = null)))
        assertNotNull(manageOpenBlock(connected.copy(local = true)))
        assertNotNull(manageOpenBlock(connected.copy(manage = ManageState(support = full.copy(observe = false)))))
    }

    @Test
    fun cloudAvailabilityUsesLocalizedReasonIdsWithoutExposingTheProfile() {
        assertEquals(R.string.settings_hub_access_revoked, manageOpenBlock(connected.copy(phase = ConnectionPhase.REVOKED)))
        assertEquals(R.string.settings_hub_access_offline, manageOpenBlock(connected.copy(phase = ConnectionPhase.DISCONNECTED)))
        assertEquals(R.string.settings_hub_access_unavailable, manageOpenBlock(connected.copy(manage = ManageState(support = full.copy(manage = false)))))
    }

    @Test
    fun advancedConfigWritesRequireManageObserveControlApprovalAndStepUp() {
        assertTrue(configWritable(full))
        assertFalse(configWritable(null))
        for (unsupported in listOf(
            full.copy(manage = false),
            full.copy(observe = false),
            full.copy(control = false),
            full.copy(approve = false),
            full.copy(stepup = false),
        )) {
            assertFalse(configWritable(unsupported))
        }
    }
}
