package dev.harw.mobile

import com.google.gson.JsonObject
import java.util.UUID

/** Public OAuth registration metadata only. An unverified client has no subject. */
internal data class GatewaySiwcRegistration(
    val hostId: String,
    val clientId: String? = null,
    val subject: String? = null,
    val revision: Long = 0,
) {
    init {
        require(isGatewaySiwcHostId(hostId) && revision >= 0)
        require(clientId == null || isSiwcClientId(clientId))
        require(subject == null || (clientId != null && subject.isNotBlank() &&
            subject.length <= 1024 && subject.none(Char::isISOControl)))
    }

    /** Do not overwrite a returning client or clear its authenticated subject. */
    fun beforeExchange(callbackClientId: String): GatewaySiwcRegistration {
        if (clientId != null && !siwcEqual(clientId, callbackClientId)) {
            throw SiwcFailure("OpenAI returned a different client registration than the one selected here.")
        }
        return if (clientId == null) copy(clientId = callbackClientId, revision = nextRevision()) else this
    }

    fun afterVerification(verifiedClientId: String, verifiedSubject: String): GatewaySiwcRegistration {
        if (clientId == null || !siwcEqual(clientId, verifiedClientId) ||
            (subject != null && !siwcEqual(subject, verifiedSubject))
        ) {
            throw SiwcFailure("OpenAI returned a different ChatGPT account registration than the one selected here.")
        }
        return copy(subject = verifiedSubject, revision = nextRevision())
    }

    fun afterUnlink(): GatewaySiwcRegistration =
        copy(clientId = null, subject = null, revision = nextRevision())

    private fun nextRevision(): Long = Math.addExact(revision, 1L)
}

internal fun isGatewaySiwcHostId(value: String): Boolean {
    if (!value.startsWith("urn:uuid:")) return false
    val raw = value.removePrefix("urn:uuid:")
    val uuid = runCatching { UUID.fromString(raw) }.getOrNull() ?: return false
    return uuid.version() == 4 && uuid.variant() == 2 && uuid.toString() == raw
}

private val ScopeToken = Regex("[A-Za-z0-9_-]{43}")
internal const val GatewaySiwcMaxScopes = 128

internal fun decodeGatewaySiwcRegistrations(encoded: String): Map<String, GatewaySiwcRegistration> {
    val root = requireNotNull(SiwcJson.fromJson(encoded, JsonObject::class.java))
    require(root.keySet() == setOf("version", "gateways") && root.integer("version") == 1L)
    val rows = root.getAsJsonObject("gateways")
    require(rows.size() <= GatewaySiwcMaxScopes)
    return rows.entrySet().associate { (scope, element) ->
        require(ScopeToken.matches(scope) && element.isJsonObject)
        val row = element.asJsonObject
        require(row.keySet().all { it in setOf("host_id", "client_id", "subject", "revision") })
        val host = requireNotNull(row.string("host_id"))
        val client = if (row.has("client_id")) requireNotNull(row.string("client_id")) else null
        val subject = if (row.has("subject")) requireNotNull(row.string("subject")) else null
        val revision = requireNotNull(row.integer("revision"))
        scope to GatewaySiwcRegistration(host, client, subject, revision)
    }.also { registrations ->
        require(registrations.values.map { it.hostId }.toSet().size == registrations.size)
    }
}

internal fun encodeGatewaySiwcRegistrations(registrations: Map<String, GatewaySiwcRegistration>): String {
    require(registrations.size <= GatewaySiwcMaxScopes && registrations.keys.all(ScopeToken::matches))
    require(registrations.values.map { it.hostId }.toSet().size == registrations.size)
    return SiwcJson.toJson(JsonObject().apply {
        addProperty("version", 1)
        add("gateways", JsonObject().apply {
            registrations.toSortedMap().forEach { (scope, registration) ->
                add(scope, JsonObject().apply {
                    addProperty("host_id", registration.hostId)
                    addProperty("revision", registration.revision)
                    registration.clientId?.let { addProperty("client_id", it) }
                    registration.subject?.let { addProperty("subject", it) }
                })
            }
        })
    })
}

/** Only the exactly matching old profile may claim the formerly global host identity. */
internal fun legacyGatewaySiwcRegistration(
    encoded: String?,
    expectedLegacyScope: String,
    legacyHost: () -> String?,
): GatewaySiwcRegistration? {
    if (encoded == null) return null
    val row = requireNotNull(SiwcJson.fromJson(encoded, JsonObject::class.java))
    if (row.string("server_scope") != expectedLegacyScope) return null
    val host = requireNotNull(legacyHost()) { "The saved ChatGPT registration has no matching host identifier." }
    return GatewaySiwcRegistration(
        host, requireNotNull(row.string("client_id")), requireNotNull(row.string("subject")),
    )
}
