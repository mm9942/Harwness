package dev.harw.mobile

import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertThrows
import org.junit.Test

class VpnProfileSettingsTest {
    @Test
    fun normalizesPrivateDnsHostAndRejectsUnencryptedCustomUrls() {
        assertEquals("", VpnDnsMode.Tor.endpoint(""))
        assertEquals("https://dns.google/dns-query", VpnDnsMode.Google.endpoint(""))
        assertEquals("tls://dns.example.com:853", VpnDnsMode.CustomTls.endpoint("dns.example.com:853"))
        assertNull(VpnDnsMode.CustomHttps.endpoint("http://dns.example.com/dns-query"))
        assertNull(VpnDnsMode.CustomHttps.endpoint("https://user@dns.example.com/dns-query"))
        assertNull(VpnDnsMode.CustomTls.endpoint("dns.example.com/dns-query"))
    }

    @Test
    fun ignoresRemovedAndResignedPackagesBeforeEncodingUidRules() {
        val settings = VpnProfileSettings(
            rules = listOf(
                VpnAppRule("app.current", "a", VpnRoute.OnionOnly),
                VpnAppRule("app.removed", "b", VpnRoute.InternetAndOnion),
                VpnAppRule("app.resigned", "c", VpnRoute.InternetAndOnion),
            ),
        )
        val resolved = resolveVpnSettings(settings) { name ->
            when (name) {
                "app.current" -> VpnAppIdentity(0x1234, "a")
                "app.resigned" -> VpnAppIdentity(0x3456, "new")
                else -> null
            }
        }
        assertEquals(0, resolved.mode)
        assertEquals(setOf("app.current"), resolved.packages)
        assertArrayEquals(byteArrayOf(0, 0, 0x12, 0x34, 1), resolved.nativeRules())
    }

    @Test
    fun rejectsConflictingSharedUidRules() {
        val settings = VpnProfileSettings(
            deviceWide = true,
            rules = listOf(
                VpnAppRule("app.first", "a", VpnRoute.OnionOnly),
                VpnAppRule("app.second", "a", VpnRoute.InternetAndOnion),
            ),
        )
        assertThrows(IllegalArgumentException::class.java) {
            resolveVpnSettings(settings) { VpnAppIdentity(1234, "a") }
        }
    }
}
