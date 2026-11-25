package dev.harw.mobile

import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test
import uniffi.harw_mobile_ffi.ManageStepUp

private fun stepUpFixture(
    target: String = "llm.example.test",
    address: String? = null,
    keyName: String? = null,
): ManageStepUp = ManageStepUp(
    stepupId = "sess-a1",
    action = "model.write",
    target = target,
    text = "harw step-up v1",
    nonce = "0123456789abcdef0123456789abcdef",
    hash = "0123456789abcdef0123456789abcdef",
    expiresAt = "2026-11-22T10:01:00Z",
    validForMs = 60000uL,
    lines = emptyList(),
    address = address,
    keyName = keyName,
)

class StepUpDialogTest {

    @Test
    fun countdownExpires() {
        // Validity 60 s, received at 1 s, now 61.001 s: already expired.
        val expiredMs = stepUpRemainingMs(60000L, 1000L, 61001L)
        assertTrue("remaining must be <= 0 after expiry, was $expiredMs", expiredMs <= 0L)
        assertEquals(0L, stepUpCountdownSeconds(expiredMs))

        // 30 s elapsed of 60 s validity leaves exactly 30 s.
        assertEquals(30000L, stepUpRemainingMs(60000L, 1000L, 31000L))

        // Countdown rounds up and never goes negative.
        assertEquals(1L, stepUpCountdownSeconds(1L))
        assertEquals(0L, stepUpCountdownSeconds(0L))
        assertEquals(0L, stepUpCountdownSeconds(-5L))
    }

    @Test
    fun highlightsAreFull() {
        val address = "https://llm.example.test/v1"
        val stepUp = stepUpFixture(
            target = "llm.example.test",
            address = address,
            keyName = "anthropic-key",
        )
        // Full values, never shortened.
        assertEquals(
            listOf(
                "Adresse: https://llm.example.test/v1",
                "Schlüssel: anthropic-key (nur schreiben)",
            ),
            stepUpHighlights(stepUp),
        )

        // Neither address nor key name: nothing to highlight.
        assertEquals(emptyList<String>(), stepUpHighlights(stepUpFixture(address = null, keyName = null)))
    }

    @Test
    fun authErrorCancelCodes() {
        // BiometricPrompt ERROR_CANCELED (5), ERROR_USER_CANCELED (10), ERROR_NEGATIVE_BUTTON (13).
        assertEquals(STEPUP_CANCELED_TEXT, stepUpAuthError(5, "irrelevant"))
        assertEquals(STEPUP_CANCELED_TEXT, stepUpAuthError(10, "irrelevant"))
        assertEquals(STEPUP_CANCELED_TEXT, stepUpAuthError(13, "irrelevant"))

        // Any other code is a real authentication failure and carries the message.
        val other = stepUpAuthError(7, "Sensor defekt")
        assertTrue("other codes must not read as cancel, was: $other", other.startsWith("Geräte-Authentifizierung"))
    }

    @Test
    fun promptPrefersAddress() {
        val withAddress = stepUpFixture(target = "llm.example.test", address = "https://llm.example.test/v1")
        assertEquals("https://llm.example.test/v1", stepUpPromptDescription(withAddress))

        // Without an address the target is shown.
        val withoutAddress = stepUpFixture(target = "llm.example.test", address = null)
        assertEquals("llm.example.test", stepUpPromptDescription(withoutAddress))
    }
}
