package dev.harw.mobile

import com.google.gson.JsonParseException
import java.io.IOException
import java.net.UnknownHostException
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Test
import uniffi.harw_mobile_ffi.ChatgptLinkAvailability
import uniffi.harw_mobile_ffi.MobileException

class ChatgptMessagesTest {
    private val allOperations = ChatgptOperation.values().toList()

    private fun generic(operation: ChatgptOperation): String =
        when (operation) {
            ChatgptOperation.Checking -> ChatgptCheckFailedMessage
            ChatgptOperation.Unlinking -> ChatgptUnlinkFailedMessage
            else -> ChatgptLinkFailedMessage
        }

    @Test
    fun unavailableMessageCoversEveryAvailability() {
        assertNull(chatgptUnavailableMessage(ChatgptLinkAvailability.AVAILABLE))
        assertEquals(
            ChatgptNotConfiguredMessage,
            chatgptUnavailableMessage(ChatgptLinkAvailability.NOT_CONFIGURED),
        )
        assertEquals(
            ChatgptNotLinkDeviceMessage,
            chatgptUnavailableMessage(ChatgptLinkAvailability.NOT_LINK_DEVICE),
        )
    }

    @Test
    fun siwcFailureMessageIsPassedThroughInEveryOperation() {
        for (operation in allOperations) {
            assertEquals(
                "Sign-in was cancelled.",
                chatgptFailureMessage(operation, SiwcFailure("Sign-in was cancelled.")),
            )
        }
    }

    @Test
    fun siwcFailureWithBlankMessageFallsBackToGeneric() {
        for (operation in allOperations) {
            assertEquals(
                generic(operation),
                chatgptFailureMessage(operation, SiwcFailure("   ")),
            )
        }
    }

    @Test
    fun disconnectedAndRevokedHaveFixedTexts() {
        for (operation in allOperations) {
            assertEquals(
                "The connection was lost.",
                chatgptFailureMessage(operation, MobileException.Disconnected()),
            )
            assertEquals(
                "The server stopped authorizing this connection.",
                chatgptFailureMessage(operation, MobileException.Revoked()),
            )
        }
    }

    @Test
    fun detailCarryingMobileExceptionsReturnTheirDetail() {
        val errors =
            listOf(
                MobileException.InvalidIdentity("identity detail"),
                MobileException.InvalidProfile("profile detail"),
                MobileException.Connect("connect detail"),
                MobileException.Session("session detail"),
                MobileException.Denied("denied detail"),
                MobileException.Transport("transport detail"),
                MobileException.Ambiguous("ambiguous detail"),
                MobileException.Internal("internal detail"),
            )
        val expected =
            listOf(
                "identity detail",
                "profile detail",
                "connect detail",
                "session detail",
                "denied detail",
                "transport detail",
                "ambiguous detail",
                "internal detail",
            )
        for (index in errors.indices) {
            assertEquals(
                expected[index],
                chatgptFailureMessage(ChatgptOperation.Checking, errors[index]),
            )
        }
    }

    @Test
    fun detailIsTrimmed() {
        assertEquals(
            "padded",
            chatgptFailureMessage(
                ChatgptOperation.Unlinking,
                MobileException.Denied("  \n padded \t "),
            ),
        )
    }

    @Test
    fun longDetailIsTruncatedToTwoHundredCharacters() {
        val message =
            chatgptFailureMessage(
                ChatgptOperation.Checking,
                MobileException.Session("a".repeat(300)),
            )
        assertEquals(200, message.length)
        assertEquals("a".repeat(200), message)
    }

    @Test
    fun blankDetailFallsBackToGenericForEveryOperation() {
        for (operation in allOperations) {
            assertEquals(
                generic(operation),
                chatgptFailureMessage(operation, MobileException.Denied("   ")),
            )
        }
    }

    @Test
    fun ioErrorsInBrowserValidatingAndLinkingMeanOpenAiIsUnreachable() {
        val operations =
            listOf(
                ChatgptOperation.Browser,
                ChatgptOperation.Validating,
                ChatgptOperation.Linking,
            )
        for (operation in operations) {
            assertEquals(
                ChatgptReachOpenAiMessage,
                chatgptFailureMessage(operation, IOException("boom")),
            )
            assertEquals(
                ChatgptReachOpenAiMessage,
                chatgptFailureMessage(operation, UnknownHostException("auth.openai.com")),
            )
        }
    }

    @Test
    fun ioErrorsInCheckingAndUnlinkingStayGeneric() {
        assertEquals(
            ChatgptCheckFailedMessage,
            chatgptFailureMessage(ChatgptOperation.Checking, IOException("boom")),
        )
        assertEquals(
            ChatgptUnlinkFailedMessage,
            chatgptFailureMessage(ChatgptOperation.Unlinking, IOException("boom")),
        )
        assertEquals(
            ChatgptCheckFailedMessage,
            chatgptFailureMessage(ChatgptOperation.Checking, UnknownHostException("host")),
        )
        assertEquals(
            ChatgptUnlinkFailedMessage,
            chatgptFailureMessage(ChatgptOperation.Unlinking, UnknownHostException("host")),
        )
    }

    private fun unreadableCandidates(): List<Throwable> =
        listOf(
            JsonParseException("bad json"),
            IllegalStateException("bad state"),
            IllegalArgumentException("bad argument"),
        )

    @Test
    fun unreadableResponsesInBrowserAndValidatingGetTheUnreadableText() {
        for (error in unreadableCandidates()) {
            assertEquals(
                ChatgptUnreadableResponseMessage,
                chatgptFailureMessage(ChatgptOperation.Browser, error),
            )
            assertEquals(
                ChatgptUnreadableResponseMessage,
                chatgptFailureMessage(ChatgptOperation.Validating, error),
            )
        }
    }

    @Test
    fun unreadableResponseErrorsElsewhereStayGeneric() {
        for (error in unreadableCandidates()) {
            assertEquals(
                ChatgptCheckFailedMessage,
                chatgptFailureMessage(ChatgptOperation.Checking, error),
            )
            assertEquals(
                ChatgptLinkFailedMessage,
                chatgptFailureMessage(ChatgptOperation.Linking, error),
            )
        }
    }

    @Test
    fun plainRuntimeExceptionIsGenericInEveryOperation() {
        for (operation in allOperations) {
            assertEquals(
                generic(operation),
                chatgptFailureMessage(operation, RuntimeException("whatever")),
            )
        }
    }

    @Test
    fun mobileExceptionTakesPrecedenceOverOperationRules() {
        assertEquals(
            "x",
            chatgptFailureMessage(ChatgptOperation.Browser, MobileException.Transport("x")),
        )
    }

    @Test
    fun otherThrowablesNeverLeakTheirMessage() {
        val secret = "Bearer eyJabc"
        val errors =
            listOf<Throwable>(
                IOException(secret),
                IllegalStateException(secret),
                RuntimeException(secret),
            )
        for (operation in allOperations) {
            for (error in errors) {
                val message = chatgptFailureMessage(operation, error)
                assertFalse(message.contains(secret))
                assertFalse(message.contains("eyJabc"))
            }
        }
    }
}
