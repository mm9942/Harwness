package dev.harw.mobile

import com.google.gson.GsonBuilder
import com.google.gson.JsonObject
import com.google.gson.Strictness
import java.math.BigInteger
import java.security.KeyFactory
import java.security.Signature
import java.security.spec.RSAPublicKeySpec
import java.time.Instant
import java.util.Base64

internal class SiwcFailure(message: String) : Exception(message)

internal data class SiwcIdentity(val subject: String, val email: String?)
internal data class SiwcRegistration(val clientId: String, val subject: String)

internal class SiwcTokens(
    val clientId: String,
    val accessToken: String,
    val refreshToken: String,
    val idToken: String,
    val scope: String,
    val expiresAt: ULong,
    val earliestRefreshAt: ULong?,
    val identity: SiwcIdentity,
)

internal val SiwcJson = GsonBuilder().setStrictness(Strictness.STRICT).create()

internal fun JsonObject.string(name: String): String? =
    get(name)?.takeIf { it.isJsonPrimitive && it.asJsonPrimitive.isString }?.asString

internal fun JsonObject.integer(name: String): Long? =
    get(name)?.takeIf { it.isJsonPrimitive && it.asJsonPrimitive.isNumber }?.toString()?.toLongOrNull()

internal fun validateSiwcIdentity(
    token: String,
    jwks: JsonObject,
    clientId: String,
    nonce: String,
    now: Instant,
): SiwcIdentity {
    try {
        val jwt = token.split('.')
        require(jwt.size == 3 && jwt.all { it.isNotEmpty() })
        val decoder = Base64.getUrlDecoder()
        val header = SiwcJson.fromJson(decoder.decode(jwt[0]).decodeToString(throwOnInvalidSequence = true), JsonObject::class.java)
        val kid = requireNotNull(header.string("kid"))
        require(header.string("alg") == "RS256" && !header.has("crit"))
        val key = jwks.getAsJsonArray("keys")
            .map { it.asJsonObject }
            .single { it.string("kid") == kid }
        require(key.string("kty") == "RSA" && key.string("alg") == "RS256" && key.string("use") == "sig")
        val modulus = BigInteger(1, decoder.decode(requireNotNull(key.string("n"))))
        val exponent = BigInteger(1, decoder.decode(requireNotNull(key.string("e"))))
        require(modulus.bitLength() in 2048..8192 && exponent.bitLength() in 2..32)
        val publicKey = KeyFactory.getInstance("RSA").generatePublic(RSAPublicKeySpec(modulus, exponent))
        val verifier = Signature.getInstance("SHA256withRSA").apply {
            initVerify(publicKey)
            update("${jwt[0]}.${jwt[1]}".encodeToByteArray())
        }
        require(verifier.verify(decoder.decode(jwt[2])))

        val claims = SiwcJson.fromJson(decoder.decode(jwt[1]).decodeToString(throwOnInvalidSequence = true), JsonObject::class.java)
        require(claims.string("iss") == SiwcIssuer && siwcEqual(nonce, claims.string("nonce")))
        val audience = claims.get("aud")
        if (audience.isJsonArray) {
            val values = audience.asJsonArray.map {
                require(it.isJsonPrimitive && it.asJsonPrimitive.isString)
                it.asString
            }
            require(clientId in values && (values.size == 1 || claims.string("azp") == clientId))
        } else {
            require(claims.string("aud") == clientId)
        }
        require(!claims.has("azp") || claims.string("azp") == clientId)
        val current = now.epochSecond
        require(requireNotNull(claims.integer("exp")) > current)
        require(!claims.has("iat") || requireNotNull(claims.integer("iat")) <= current + 60)
        require(!claims.has("nbf") || requireNotNull(claims.integer("nbf")) <= current)
        val subject = requireNotNull(claims.string("sub"))
        require(subject.isNotBlank() && subject.length <= 1024 && subject.none(Char::isISOControl))
        val email = claims.string("email")?.takeIf {
            claims.get("email_verified")?.toString() == "true" && it.length in 1..320 && it.none(Char::isISOControl)
        }
        return SiwcIdentity(subject, email)
    } catch (_: Exception) {
        throw SiwcFailure("OpenAI’s ID token could not be verified for this sign-in.")
    }
}
