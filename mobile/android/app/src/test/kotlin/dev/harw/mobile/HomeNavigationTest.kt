package dev.harw.mobile

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class HomeNavigationTest {
    @Test
    fun homeShortcutsAreOrderedFromTopToBottom() {
        assertEquals(
            listOf(HomeAction.Assistant, HomeAction.Settings, HomeAction.Library, HomeAction.NewSession),
            homeShortcuts(online = true, canCreate = true, creating = false).map { it.action },
        )
    }

    @Test
    fun unavailableAssistantAndMediaLibraryAreNotActionableRoutes() {
        for (online in listOf(false, true)) {
            val shortcuts = homeShortcuts(online = online, canCreate = true, creating = false)
            assertFalse(shortcuts.single { it.action == HomeAction.Library }.enabled)
            assertTrue(shortcuts.single { it.action == HomeAction.Settings }.enabled)
            assertFalse(shortcuts.single { it.action == HomeAction.Assistant }.enabled)
        }
    }

    @Test
    fun newSessionRequiresTheSelectedBackendOnlineAndReadyToCreate() {
        assertTrue(homeShortcuts(online = true, canCreate = true, creating = false).last().enabled)
        assertFalse(homeShortcuts(online = false, canCreate = true, creating = false).last().enabled)
        assertFalse(homeShortcuts(online = true, canCreate = false, creating = false).last().enabled)
        assertFalse(homeShortcuts(online = true, canCreate = true, creating = true).last().enabled)
    }
    @Test
    fun cloudShortcutsStayVisibleButRequireTheirOwnNegotiatedCapability() {
        assertFalse(homeShortcuts(true, true, false, privateLibrary = true, cloudLibrary = true)
            .any { it.action == HomeAction.CloudLibrary || it.action == HomeAction.CloudAssistant })

        val offline = homeShortcuts(false, true, false, viewingLocal = false, cloudLibrary = true)
        assertEquals(CloudShortcutState.Disconnected, offline.single { it.action == HomeAction.CloudLibrary }.cloudState)
        assertFalse(offline.single { it.action == HomeAction.CloudLibrary }.enabled)

        val online = homeShortcuts(true, true, false, viewingLocal = false, cloudLibrary = true)
        assertEquals(
            listOf(HomeAction.CloudAssistant, HomeAction.Settings, HomeAction.CloudLibrary, HomeAction.NewSession),
            online.map { it.action },
        )
        assertTrue(online.single { it.action == HomeAction.CloudLibrary }.enabled)
        assertEquals(CloudShortcutState.Unsupported, online.single { it.action == HomeAction.CloudAssistant }.cloudState)
        assertFalse(online.single { it.action == HomeAction.CloudAssistant }.enabled)
        assertFalse(online.any { it.action == HomeAction.Library || it.action == HomeAction.Assistant })
    }
}
