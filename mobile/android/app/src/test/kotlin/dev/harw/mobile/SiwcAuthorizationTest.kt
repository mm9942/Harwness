package dev.harw.mobile

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotEquals
import org.junit.Assert.assertThrows
import org.junit.Test

class SiwcAuthorizationTest {
    @Test
    fun challengeMatchesThePkceS256TestVector() {
        assertEquals(
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM",
            siwcPkceChallenge("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
        )
    }

    @Test
    fun everyAttemptGetsNewStateNonceAndVerifier() {
        val first = SiwcAttempt(1455, "urn:uuid:example", null)
        val next = SiwcAttempt(1455, "urn:uuid:example", null)
        assertNotEquals(first.state, next.state)
        assertNotEquals(first.nonce, next.nonce)
        assertNotEquals(first.verifier, next.verifier)
        assertEquals("http://127.0.0.1:1455/auth/callback", next.redirectUri)
    }

    @Test
    fun dynamicRegistrationRequiresAnIssuedClientAndRejectsStateSwap() {
        val attempt = SiwcAttempt(1455, "urn:uuid:example", null)
        val target = "/auth/callback?code=valid-code&client_id=oaiapp_issued&state="
        val callback = attempt.callback(target + attempt.state) as SiwcCallback.Code
        assertEquals("valid-code", callback.code)
        assertEquals("oaiapp_issued", callback.clientId)
        for (request in listOf(
            target + "wrong",
            target.replace("oaiapp_issued", "dynamic_agent_client") + attempt.state,
            target + attempt.state + "&state=${attempt.state}",
            target.replace("/auth/callback", "/callback") + attempt.state,
        )) {
            assertThrows(IllegalArgumentException::class.java) { attempt.callback(request) }
        }
    }

    @Test
    fun returningRegistrationRejectsAReplacementClientAndChecksDenialState() {
        val attempt = SiwcAttempt(1455, "urn:uuid:example", "oaiapp_selected")
        val target = "/auth/callback?state=${attempt.state}"
        val callback = attempt.callback("$target&code=valid-code") as SiwcCallback.Code
        assertEquals("valid-code", callback.code)
        assertEquals("oaiapp_selected", callback.clientId)
        assertEquals(SiwcCallback.Denied, attempt.callback("$target&error=access_denied"))
        assertThrows(IllegalArgumentException::class.java) {
            attempt.callback("$target&code=valid-code&client_id=oaiapp_other")
        }
        assertThrows(IllegalArgumentException::class.java) {
            attempt.callback("/auth/callback?state=wrong&error=access_denied")
        }
    }
}
