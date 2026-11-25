package dev.harw.mobile

import java.security.MessageDigest
import java.util.Base64
import uniffi.harw_mobile_ffi.RoutePreference
import uniffi.harw_mobile_ffi.ServerProfile

internal val DefaultProfileRoutePreference = RoutePreference.ONION

/** Input profiles are already normalized and validated by Rust. Match its route authority equality. */
internal fun ServerProfile.routePreferenceKey(): String {
    val identity = listOf(serverNodeId, serverPublicKeyHex, endpoint, onion.orEmpty()).joinToString("\n")
    val digest = MessageDigest.getInstance("SHA-256").digest(identity.encodeToByteArray())
    return "profile_route_v1:" + Base64.getUrlEncoder().withoutPadding().encodeToString(digest)
}

/** No transport preference from the legacy global key may be applied to a gateway. */
internal fun storedProfileRoute(value: String?): RoutePreference =
    RoutePreference.entries.firstOrNull { it.name == value } ?: DefaultProfileRoutePreference

internal fun routeChoiceEnabled(choice: RoutePreference, profile: ServerProfile?): Boolean {
    if (profile == null) return false
    val hasOnion = !profile.onion.isNullOrBlank()
    val hasDirect = profile.endpoint.isNotBlank() // validated IP:port, not an untrusted hostname
    return when (choice) {
        RoutePreference.ONION -> hasOnion
        RoutePreference.DIRECT -> hasDirect
        RoutePreference.AUTO -> hasOnion && hasDirect
    }
}
