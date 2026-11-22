package dev.harw.mobile

import com.google.gson.JsonParseException
import java.io.IOException
import uniffi.harw_mobile_ffi.ChatgptLinkAvailability
import uniffi.harw_mobile_ffi.MobileException

internal const val ChatgptNotConfiguredMessage =
    "This server is not set up for ChatGPT plan linking: its provider_link_device is not configured."
internal const val ChatgptNotLinkDeviceMessage =
    "This phone is not this server’s ChatGPT link device (provider_link_device)."
internal const val ChatgptReachOpenAiMessage =
    "Harw could not reach OpenAI. Check this phone’s connection and try again."
internal const val ChatgptUnreadableResponseMessage =
    "OpenAI returned a sign-in response Harw could not read. Start again from Harw."
internal const val ChatgptCheckFailedMessage =
    "Harw could not check the ChatGPT account on this server."
internal const val ChatgptUnlinkFailedMessage =
    "Harw could not confirm that the account was unlinked. Check the server’s account status."
internal const val ChatgptLinkFailedMessage =
    "Harw could not finish linking ChatGPT. Check your browser and the server’s account status."

private const val MaxDetailLength = 200

internal fun chatgptUnavailableMessage(availability: ChatgptLinkAvailability): String? = when (availability) {
    ChatgptLinkAvailability.AVAILABLE -> null
    ChatgptLinkAvailability.NOT_CONFIGURED -> ChatgptNotConfiguredMessage
    ChatgptLinkAvailability.NOT_LINK_DEVICE -> ChatgptNotLinkDeviceMessage
}

/**
 * User-facing text for a failed ChatGPT operation. Only messages Harw itself wrote
 * (SiwcFailure, MobileException details) are shown; the message of any other throwable
 * may embed OpenAI response bodies or tokens, so those map to fixed texts.
 */
internal fun chatgptFailureMessage(operation: ChatgptOperation, error: Throwable): String {
    if (error is SiwcFailure) {
        return error.message?.takeIf { it.isNotBlank() } ?: generic(operation)
    }
    if (error is MobileException.Disconnected) return "The connection was lost."
    if (error is MobileException.Revoked) return "The server stopped authorizing this connection."
    if (error is MobileException) {
        val detail = when (error) {
            is MobileException.InvalidIdentity -> error.detail
            is MobileException.InvalidProfile -> error.detail
            is MobileException.Connect -> error.detail
            is MobileException.Session -> error.detail
            is MobileException.Denied -> error.detail
            is MobileException.Transport -> error.detail
            is MobileException.Ambiguous -> error.detail
            is MobileException.Internal -> error.detail
            else -> null
        }
        return detail?.trim()?.take(MaxDetailLength)?.takeIf { it.isNotBlank() } ?: generic(operation)
    }
    val network = operation == ChatgptOperation.Browser ||
        operation == ChatgptOperation.Validating ||
        operation == ChatgptOperation.Linking
    if (network && error is IOException) return ChatgptReachOpenAiMessage
    val parsing = operation == ChatgptOperation.Browser || operation == ChatgptOperation.Validating
    if (parsing && (error is JsonParseException || error is IllegalStateException || error is IllegalArgumentException)) {
        return ChatgptUnreadableResponseMessage
    }
    return generic(operation)
}

private fun generic(operation: ChatgptOperation): String = when (operation) {
    ChatgptOperation.Checking -> ChatgptCheckFailedMessage
    ChatgptOperation.Unlinking -> ChatgptUnlinkFailedMessage
    else -> ChatgptLinkFailedMessage
}
