package dev.harw.mobile

import java.security.MessageDigest
import java.util.Base64
import uniffi.harw_mobile_ffi.ServerProfile

/** Use the same normalized transport identity as Rust's profile and submission cache. */
internal fun ServerProfile.windowKey(): String {
    val identity = "$endpoint\n$serverNodeId\n$serverPublicKeyHex"
    val digest = MessageDigest.getInstance("SHA-256").digest(identity.encodeToByteArray())
    return Base64.getUrlEncoder().withoutPadding().encodeToString(digest)
}

/** ChatGPT installation identity follows the pinned gateway, not its selected route or endpoint. */
internal fun ServerProfile.chatgptGatewayScope(): String =
    chatgptGatewayScope(serverNodeId, serverPublicKeyHex)

internal fun chatgptGatewayScope(serverNodeId: String, serverPublicKeyHex: String): String {
    require(serverNodeId.isNotBlank() && serverNodeId.none(Char::isISOControl))
    require(serverPublicKeyHex.isNotEmpty() && serverPublicKeyHex.all { it in '0'..'9' || it in 'a'..'f' || it in 'A'..'F' })
    val identity = "harw:siwc-gateway:v1\u0000$serverNodeId\n${serverPublicKeyHex.lowercase(java.util.Locale.ROOT)}"
    return Base64.getUrlEncoder().withoutPadding().encodeToString(
        MessageDigest.getInstance("SHA-256").digest(identity.encodeToByteArray()),
    )
}
