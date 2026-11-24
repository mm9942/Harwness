package dev.harw.mobile

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotEquals
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import uniffi.harw_mobile_ffi.ConnectionPhase
import uniffi.harw_mobile_ffi.ConnectionRoute
import uniffi.harw_mobile_ffi.RoutePreference
import uniffi.harw_mobile_ffi.ServerProfile
import uniffi.harw_mobile_ffi.TorBootstrap

class ConnectionRouteTest {
    private val vector = "2gzyxa5ihm7nsggfxnu52rck2vv4rvmdlkiu3zzui5du4xyclen53wid.onion:443"

    private fun profile(onion: String?) = ServerProfile(
        version = 1U,
        label = "Development",
        endpoint = "127.0.0.1:7410",
        serverNodeId = "node-a",
        serverPublicKeyHex = "01".repeat(1952),
        onion = onion,
    )

    private fun tor(percent: UInt = 45U, ready: Boolean = false, blocked: String? = null) =
        TorBootstrap(percent = percent, ready = ready, blocked = blocked, summary = "loading directory")

    @Test
    fun profileRoutesFollowTheProfile() {
        assertEquals(emptyList<ConnectionRoute>(), profileRoutes(null))
        assertEquals(listOf(ConnectionRoute.DIRECT), profileRoutes(profile(null)))
        assertEquals(
            listOf(ConnectionRoute.ONION, ConnectionRoute.DIRECT),
            profileRoutes(profile(vector)),
        )
    }

    @Test
    fun routeChoicesNeedMatchingProfileRoutes() {
        for (choice in RoutePreference.values()) {
            assertFalse(routeChoiceEnabled(choice, null))
        }
        assertFalse(routeChoiceEnabled(RoutePreference.ONION, profile(null)))
        assertTrue(routeChoiceEnabled(RoutePreference.DIRECT, profile(null)))
        assertTrue(routeChoiceEnabled(RoutePreference.ONION, profile(vector)))
        assertFalse(routeChoiceEnabled(RoutePreference.AUTO, profile(null)))
        assertTrue(routeChoiceEnabled(RoutePreference.AUTO, profile(vector)))
        val onionOnly = profile(vector).copy(endpoint = "")
        assertEquals(listOf(ConnectionRoute.ONION), profileRoutes(onionOnly))
        assertFalse(routeChoiceEnabled(RoutePreference.DIRECT, onionOnly))
        assertFalse(routeChoiceEnabled(RoutePreference.AUTO, onionOnly))
    }

    @Test
    fun badgeTextNamesTheRoute() {
        assertEquals("Live · Onion", connectionBadgeText(ConnectionPhase.CONNECTED, false, ConnectionRoute.ONION))
        assertEquals("Live · direkt", connectionBadgeText(ConnectionPhase.CONNECTED, false, ConnectionRoute.DIRECT))
        assertEquals("Verbinde · Onion", connectionBadgeText(ConnectionPhase.CONNECTING, false, ConnectionRoute.ONION))
        assertEquals("Live", connectionBadgeText(ConnectionPhase.CONNECTED, false, null))
        assertEquals("Wird beendet", connectionBadgeText(ConnectionPhase.CONNECTED, true, ConnectionRoute.ONION))
        assertEquals("Fehlgeschlagen", connectionBadgeText(ConnectionPhase.FAILED, false, ConnectionRoute.ONION))
    }

    @Test
    fun torProgressTextDescribesBootstrap() {
        assertNull(torProgressText(null, ConnectionPhase.CONNECTING, ConnectionRoute.ONION))
        val blocked = torProgressText(
            tor(blocked = "no network"),
            ConnectionPhase.CONNECTING,
            ConnectionRoute.ONION,
        )
        assertNotNull(blocked)
        assertTrue(blocked!!.contains("no network"))
        val loading = torProgressText(tor(), ConnectionPhase.CONNECTING, ConnectionRoute.ONION)
        assertNotNull(loading)
        assertTrue(loading!!.contains("45%"))
        assertTrue(loading.contains("loading directory"))
        assertEquals(
            "Tor bereit · erreiche den .onion-Dienst",
            torProgressText(tor(100U, true), ConnectionPhase.CONNECTING, ConnectionRoute.ONION),
        )
        assertNull(torProgressText(tor(100U, true), ConnectionPhase.CONNECTED, ConnectionRoute.ONION))
    }

    @Test
    fun shortOnionKeepsPrefixAndPort() {
        assertEquals("2gzyxa….onion:443", shortOnion(vector))
    }

    @Test
    fun missingOrMalformedScopedPreferenceNeverChoosesDirectOrAuto() {
        assertEquals(RoutePreference.ONION, storedProfileRoute(null))
        assertEquals(RoutePreference.ONION, storedProfileRoute("garbled"))
        assertEquals(RoutePreference.ONION, storedProfileRoute("direct"))
        assertEquals(RoutePreference.DIRECT, storedProfileRoute("DIRECT"))
        assertEquals(RoutePreference.AUTO, storedProfileRoute("AUTO"))
    }

    @Test
    fun consentKeyBindsBothLocatorsAndPinnedIdentityButNotLabel() {
        val saved = profile(vector)
        val key = saved.routePreferenceKey()
        assertEquals(key, saved.copy(label = "Renamed").routePreferenceKey())
        assertNotEquals(key, saved.copy(endpoint = "").routePreferenceKey())
        assertNotEquals(key, saved.copy(onion = null).routePreferenceKey())
        assertNotEquals(key, saved.copy(serverNodeId = "node-b").routePreferenceKey())
        assertNotEquals(key, saved.copy(serverPublicKeyHex = "02".repeat(1952)).routePreferenceKey())
    }
}
