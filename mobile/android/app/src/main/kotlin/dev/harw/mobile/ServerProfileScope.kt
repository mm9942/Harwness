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
