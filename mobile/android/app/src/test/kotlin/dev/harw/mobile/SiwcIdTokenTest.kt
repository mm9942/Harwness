package dev.harw.mobile

import com.google.gson.JsonArray
import com.google.gson.JsonObject
import java.security.KeyPairGenerator
import java.security.Signature
import java.security.interfaces.RSAPublicKey
import java.time.Instant
import org.junit.Assert.assertEquals
import org.junit.Assert.assertThrows
import org.junit.Test

class SiwcIdTokenTest {
    private val now = Instant.ofEpochSecond(1_800_000_000)

    @Test
    fun signedIdentityMustMatchIssuerClientNonceAndCurrentTime() {
        assertEquals(SiwcIdentity("account-1", "sample@example.test"), verify(token()))
        assertThrows(SiwcFailure::class.java) { verify(token(nonce = "wrong")) }
        assertThrows(SiwcFailure::class.java) { verify(token(audience = "oaiapp_other")) }
        assertThrows(SiwcFailure::class.java) { verify(token(issuer = "https://invalid.example")) }
        assertThrows(SiwcFailure::class.java) { verify(token(exp = now.epochSecond)) }
    }

    @Test
    fun tamperedClaimsAndUnsupportedAlgorithmsAreRejected() {
        val original = token()
        val fields = original.split('.')
        val differentPayload = token(nonce = "wrong").split('.')[1]
        assertThrows(SiwcFailure::class.java) { verify("${fields[0]}.$differentPayload.${fields[2]}") }
        assertThrows(SiwcFailure::class.java) { verify(token(alg = "HS256")) }
    }

    private fun verify(jwt: String) = validateSiwcIdentity(jwt, jwks, "oaiapp_current", "nonce-1", now)

    private fun token(
        audience: String = "oaiapp_current",
        issuer: String = SiwcIssuer,
        nonce: String = "nonce-1",
        exp: Long = now.epochSecond + 3600,
        alg: String = "RS256",
    ): String {
        val header = """{"kid":"test-key","alg":"$alg"}"""
        val claims = """{"iss":"$issuer","aud":"$audience","nonce":"$nonce","exp":$exp,"sub":"account-1","email":"sample@example.test","email_verified":true}"""
        val input = "${siwcBase64(header.encodeToByteArray())}.${siwcBase64(claims.encodeToByteArray())}"
        val signed = Signature.getInstance("SHA256withRSA").run {
            initSign(key.private)
            update(input.encodeToByteArray())
            sign()
        }
        return "$input.${siwcBase64(signed)}"
    }

    private companion object {
        val key = KeyPairGenerator.getInstance("RSA").apply { initialize(2048) }.generateKeyPair()
        val jwks = JsonObject().apply {
            val publicKey = key.public as RSAPublicKey
            add("keys", JsonArray().apply {
                add(JsonObject().apply {
                    addProperty("kid", "test-key")
                    addProperty("kty", "RSA")
                    addProperty("alg", "RS256")
                    addProperty("use", "sig")
                    addProperty("n", siwcBase64(publicKey.modulus.toByteArray()))
                    addProperty("e", siwcBase64(publicKey.publicExponent.toByteArray()))
                })
            })
        }
    }
}
