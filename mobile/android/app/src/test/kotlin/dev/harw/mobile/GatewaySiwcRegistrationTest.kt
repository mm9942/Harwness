package dev.harw.mobile

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertThrows
import org.junit.Test

class GatewaySiwcRegistrationTest {
    private val first = "urn:uuid:66f7ecbd-52ba-4812-8ecc-5d17c8314bb5"
    private val second = "urn:uuid:93fc367b-037c-4eae-ace2-92a269225d5a"
    private val alpha = chatgptGatewayScope("gateway-alpha", "aa12")
    private val beta = chatgptGatewayScope("gateway-beta", "bb23")

    @Test
    fun gatewayIdentityDependsOnPinnedNodeAndKeyNotKeyHexCase() {
        assertEquals(alpha, chatgptGatewayScope("gateway-alpha", "AA12"))
        assertNotEquals(alpha, chatgptGatewayScope("gateway-alpha", "aa13"))
        assertNotEquals(alpha, chatgptGatewayScope("gateway-beta", "aa12"))
    }

    @Test
    fun pendingAndVerifiedRegistrationsRemainSeparateAcrossGateways() {
        val pending = GatewaySiwcRegistration(first).beforeExchange("client_alpha")
        val verified = GatewaySiwcRegistration(second).beforeExchange("client_beta")
            .afterVerification("client_beta", "verified-subject")
        val encoded = encodeGatewaySiwcRegistrations(mapOf(alpha to pending, beta to verified))
        val restored = decodeGatewaySiwcRegistrations(encoded)
        assertEquals(first, restored.getValue(alpha).hostId)
        assertEquals("client_alpha", restored.getValue(alpha).clientId)
        assertNull(restored.getValue(alpha).subject)
        assertEquals(verified, restored.getValue(beta))
        assertFalse(encoded.contains("access_token"))
        assertFalse(encoded.contains("refresh_token"))
        assertFalse(encoded.contains("authorization_code"))
        assertThrows(IllegalArgumentException::class.java) {
            encodeGatewaySiwcRegistrations(mapOf(alpha to pending, beta to pending))
        }
    }

    @Test
    fun pendingRetryUsesSavedClientWithFreshAttemptAndOnlyVerificationSetsSubject() {
        val pending = GatewaySiwcRegistration(first).beforeExchange("client_alpha")
        val retry = SiwcAttempt(16001, pending.hostId, pending.clientId)
        val alternate = SiwcAttempt(16002, pending.hostId, pending.clientId)
        assertNotEquals(retry.state, alternate.state)
        assertNotEquals(retry.verifier, alternate.verifier)
        val callback = retry.callback("/auth/callback?state=${retry.state}&code=code_only") as SiwcCallback.Code
        assertEquals(pending, pending.beforeExchange(callback.clientId))
        assertNull(pending.subject)
        val committed = pending.afterVerification(callback.clientId, "verified-subject")
        assertEquals("verified-subject", committed.subject)
        assertThrows(SiwcFailure::class.java) { pending.beforeExchange("client_other") }
        assertThrows(SiwcFailure::class.java) { pending.afterVerification("client_other", "verified-subject") }
        assertThrows(SiwcFailure::class.java) { committed.afterVerification("client_alpha", "other-subject") }
    }

    @Test
    fun unlinkPreservesHostButCannotRecreateAnOldCompareAndSwapSnapshot() {
        val empty = GatewaySiwcRegistration(first)
        val reset = empty.beforeExchange("client_alpha").afterVerification("client_alpha", "original").afterUnlink()
        assertEquals(empty.hostId, reset.hostId)
        assertNull(reset.clientId)
        assertNull(reset.subject)
        assertNotEquals(empty, reset)
    }

    @Test
    fun legacyHostOnlyMigratesWithMatchingOldProfileScope() {
        val legacy = """{"server_scope":"legacy-alpha","client_id":"client_alpha","subject":"original"}"""
        assertEquals(
            GatewaySiwcRegistration(first, "client_alpha", "original"),
            legacyGatewaySiwcRegistration(legacy, "legacy-alpha") { first },
        )
        assertNull(legacyGatewaySiwcRegistration(legacy, "legacy-beta") { error("unrelated host accessed") })
        assertNull(legacyGatewaySiwcRegistration(null, "legacy-alpha") { error("unbound host accessed") })
    }
}
