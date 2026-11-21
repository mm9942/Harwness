package dev.harw.mobile

import java.net.URI
import java.net.URLDecoder
import java.net.URLEncoder
import java.security.MessageDigest
import java.security.SecureRandom
import java.util.Base64

internal const val SiwcIssuer = "https://auth.openai.com"
internal const val SiwcResource = "https://api.openai.com/v1"
internal const val SiwcPlanScope = "chatgpt.tokens.use.direct"
private const val CallbackPath = "/auth/callback"
private const val DynamicClient = "dynamic_agent_client"
private val Random = SecureRandom()
private val IssuedClient = Regex("[A-Za-z0-9_-]{1,256}")

internal sealed interface SiwcCallback {
    // Keep authorization codes out of generated toString output.
    class Code(val code: String, val clientId: String) : SiwcCallback
    data object Denied : SiwcCallback
}

internal class SiwcAttempt(port: Int, hostId: String, private val returningClient: String?) {
    val state = siwcRandomValue()
    val nonce = siwcRandomValue()
    val verifier = siwcRandomValue()
    val redirectUri = "http://127.0.0.1:$port$CallbackPath"
    val authorizeUrl = "$SiwcIssuer/api/accounts/authorize?" + siwcForm(
        listOfNotNull(
            "client_id" to (returningClient ?: DynamicClient),
            ("agent_name_hint" to "Harw").takeIf { returningClient == null },
            "ext_agent_host_id" to hostId,
            "response_type" to "code",
            "redirect_uri" to redirectUri,
            "scope" to "openid profile email offline_access resource.invoke $SiwcPlanScope",
            "resource" to SiwcResource,
            "state" to state,
            "nonce" to nonce,
            "code_challenge_method" to "S256",
            "code_challenge" to siwcPkceChallenge(verifier),
        ),
    )

    init {
        require(returningClient == null || isSiwcClientId(returningClient))
    }

    fun callback(target: String): SiwcCallback {
        val uri = try {
            URI(target)
        } catch (_: Exception) {
            throw IllegalArgumentException("Invalid callback URI.")
        }
        require(!uri.isAbsolute && uri.rawAuthority == null && uri.rawPath == CallbackPath && uri.rawFragment == null)
        val fields = mutableMapOf<String, String>()
        for (field in requireNotNull(uri.rawQuery).split('&')) {
            val parts = field.split('=', limit = 2)
            require(parts.size == 2)
            val key = URLDecoder.decode(parts[0], "UTF-8")
            val value = URLDecoder.decode(parts[1], "UTF-8")
            require(fields.put(key, value) == null)
        }
        require(siwcEqual(state, fields["state"]))
        if ("error" in fields) {
            require("code" !in fields)
            return SiwcCallback.Denied
        }
        val issued = fields["client_id"]
        if (returningClient != null && issued != null) require(siwcEqual(returningClient, issued))
        val client = requireNotNull(returningClient ?: issued)
        val code = requireNotNull(fields["code"])
        require(isSiwcClientId(client) && code.length in 1..4096 && code.all { it in '!'..'~' })
        return SiwcCallback.Code(code, client)
    }
}

internal fun isSiwcClientId(value: String): Boolean = value != DynamicClient && IssuedClient.matches(value)

internal fun siwcPkceChallenge(verifier: String): String =
    siwcBase64(MessageDigest.getInstance("SHA-256").digest(verifier.encodeToByteArray()))

internal fun siwcEqual(expected: String, actual: String?): Boolean =
    actual != null && MessageDigest.isEqual(expected.encodeToByteArray(), actual.encodeToByteArray())

internal fun siwcForm(values: List<Pair<String, String>>): String =
    values.joinToString("&") { (key, value) -> "${URLEncoder.encode(key, "UTF-8")}=${URLEncoder.encode(value, "UTF-8")}" }

private fun siwcRandomValue(): String = siwcBase64(ByteArray(32).also(Random::nextBytes))

internal fun siwcBase64(bytes: ByteArray): String = Base64.getUrlEncoder().withoutPadding().encodeToString(bytes)
