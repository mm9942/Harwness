package dev.harw.mobile

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotEquals
import org.junit.Test
import uniffi.harw_mobile_ffi.ServerProfile

class ServerProfileScopeTest {
    private val profile = ServerProfile(
        version = 1U,
        label = "Development",
        endpoint = "127.0.0.1:7410",
        serverNodeId = "node-a",
        serverPublicKeyHex = "01".repeat(1952),
    )

    @Test
    fun renamingTheServerPreservesItsWindowAndDraftScope() {
        assertEquals(profile.windowKey(), profile.copy(label = "Workstation").windowKey())
    }

    @Test
    fun replacingThePinAtTheSameEndpointChangesTheScope() {
        assertNotEquals(profile.windowKey(), profile.copy(serverPublicKeyHex = "10".repeat(1952)).windowKey())
    }
}
