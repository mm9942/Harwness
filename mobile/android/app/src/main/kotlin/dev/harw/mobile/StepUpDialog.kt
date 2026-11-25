package dev.harw.mobile

import android.annotation.SuppressLint
import android.app.Activity
import android.app.KeyguardManager
import android.hardware.biometrics.BiometricManager
import android.hardware.biometrics.BiometricPrompt
import android.os.Build
import android.os.CancellationSignal
import android.os.SystemClock
import androidx.activity.ComponentActivity
import androidx.activity.result.contract.ActivityResultContracts
import androidx.annotation.RequiresApi
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.text.selection.SelectionContainer
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableLongStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.unit.dp
import androidx.compose.ui.window.DialogProperties
import kotlinx.coroutines.delay
import uniffi.harw_mobile_ffi.ManageStepUp

/**
 * Fresh device authentication for exactly one step-up. Nothing is cached and there is no time window: every
 * [confirm] builds a new prompt and a new cancellation signal. No CryptoObject is used; the proof that the
 * human saw the text is the hash the core re-verified plus this prompt.
 */
internal class StepUpAuthorization(private val activity: ComponentActivity) {
    private class Request(val result: (String?) -> Unit, val cancellation: CancellationSignal = CancellationSignal())

    private var pending by mutableStateOf<Request?>(null)
    private var awaitingCredential by mutableStateOf(false)
    val busy: Boolean get() = pending != null || awaitingCredential

    // MainActivity is a ComponentActivity, not a legacy FragmentActivity.
    @SuppressLint("InvalidFragmentVersionForActivityResult")
    private val credential = activity.registerForActivityResult(ActivityResultContracts.StartActivityForResult()) { result ->
        if (!awaitingCredential) return@registerForActivityResult
        awaitingCredential = false
        val request = pending ?: return@registerForActivityResult
        finish(request, if (result.resultCode == Activity.RESULT_OK) null else STEPUP_CANCELED_TEXT)
    }

    /** Starts a NEW prompt; `result(null)` means success, otherwise a German error text. */
    fun confirm(stepUp: ManageStepUp, result: (String?) -> Unit) {
        if (busy) return
        val request = Request(result)
        pending = request
        try {
            if (Build.VERSION.SDK_INT >= 30) {
                useBiometricPrompt(request, stepUp)
            } else {
                useDeviceCredential(request, stepUp)
            }
        } catch (_: Exception) {
            awaitingCredential = false
            finish(request, "Android konnte die Geräte-Authentifizierung nicht starten.")
        }
    }

    fun cancel() {
        val request = pending ?: return
        pending = null
        awaitingCredential = false
        request.cancellation.cancel()
    }

    @RequiresApi(30)
    private fun useBiometricPrompt(request: Request, stepUp: ManageStepUp) {
        val authenticators = BiometricManager.Authenticators.BIOMETRIC_STRONG or
            BiometricManager.Authenticators.DEVICE_CREDENTIAL
        val biometrics = activity.getSystemService(BiometricManager::class.java)
        if (biometrics?.canAuthenticate(authenticators) != BiometricManager.BIOMETRIC_SUCCESS) {
            finish(request, NO_SECURE_LOCK_TEXT)
            return
        }
        val prompt = BiometricPrompt.Builder(activity)
            .setTitle(STEPUP_PROMPT_TITLE)
            .setSubtitle(stepUp.action)
            .setDescription(stepUpPromptDescription(stepUp))
            .setAllowedAuthenticators(authenticators)
            .setConfirmationRequired(true)
            .build()
        prompt.authenticate(request.cancellation, activity.mainExecutor, object : BiometricPrompt.AuthenticationCallback() {
            override fun onAuthenticationSucceeded(result: BiometricPrompt.AuthenticationResult) {
                finish(request, null)
            }

            override fun onAuthenticationError(errorCode: Int, errString: CharSequence) {
                finish(request, stepUpAuthError(errorCode, errString.toString()))
            }
        })
    }

    @Suppress("DEPRECATION") // The device-credential intent is the platform API on Android 9 and 10.
    private fun useDeviceCredential(request: Request, stepUp: ManageStepUp) {
        val lock = activity.getSystemService(KeyguardManager::class.java)
        val intent = lock?.takeIf { it.isDeviceSecure }
            ?.createConfirmDeviceCredentialIntent(STEPUP_PROMPT_TITLE, stepUp.action)
        if (intent == null) {
            finish(request, NO_SECURE_LOCK_TEXT)
        } else {
            awaitingCredential = true
            credential.launch(intent)
        }
    }

    private fun finish(request: Request, error: String?) {
        if (pending !== request) return
        pending = null
        request.result(error)
    }
}

internal const val STEPUP_PROMPT_TITLE = "Harw-Änderung bestätigen"
internal const val STEPUP_CANCELED_TEXT = "Bestätigung abgebrochen."
internal const val NO_SECURE_LOCK_TEXT =
    "Biometrie oder sichere Displaysperre einrichten, um Änderungen zu bestätigen."

/** BiometricPrompt error codes ERROR_CANCELED (5), ERROR_USER_CANCELED (10), ERROR_NEGATIVE_BUTTON (13). */
internal fun stepUpAuthError(code: Int, message: String): String =
    if (code == 5 || code == 10 || code == 13) STEPUP_CANCELED_TEXT else "Geräte-Authentifizierung: $message"

/** The system prompt shows the full address when there is one, otherwise the target. */
internal fun stepUpPromptDescription(stepUp: ManageStepUp): String = stepUp.address ?: stepUp.target

/** Remaining validity in ms; may be negative. Pure so it can be tested without a clock. */
internal fun stepUpRemainingMs(validForMs: Long, receivedAt: Long, now: Long): Long =
    validForMs - (now - receivedAt)

/** Seconds shown to the human, rounded up, never negative. */
internal fun stepUpCountdownSeconds(remainingMs: Long): Long =
    if (remainingMs <= 0L) 0L else (remainingMs + 999L) / 1000L

/** The highlighted lines. Full values, never shortened. */
internal fun stepUpHighlights(stepUp: ManageStepUp): List<String> = buildList {
    stepUp.address?.let { add("Adresse: $it") }
    stepUp.keyName?.let { add("Schlüssel: $it (nur schreiben)") }
}

@Composable
internal fun StepUpDialog(
    stepUp: ManageStepUp,
    receivedAt: Long,
    authorizing: Boolean,
    error: String?,
    onConfirm: () -> Unit,
    onCancel: (String?) -> Unit,
) {
    val validFor = stepUp.validForMs.toLong()
    var now by remember(stepUp.stepupId) { mutableLongStateOf(SystemClock.elapsedRealtime()) }
    val remaining = stepUpRemainingMs(validFor, receivedAt, now)
    val expired = remaining <= 0L
    LaunchedEffect(stepUp.stepupId, receivedAt) {
        while (true) {
            now = SystemClock.elapsedRealtime()
            if (stepUpRemainingMs(validFor, receivedAt, now) <= 0L) {
                onCancel("expired")
                return@LaunchedEffect
            }
            delay(250)
        }
    }
    AlertDialog(
        onDismissRequest = { onCancel(null) },
        properties = DialogProperties(dismissOnClickOutside = false),
        title = {
            Column {
                Text("Änderung bestätigen")
                Text(
                    listOf(stepUp.action, stepUp.target).filter { it.isNotBlank() }.joinToString(" "),
                    style = MaterialTheme.typography.bodyMedium,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
        },
        text = {
            Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
                SelectionContainer {
                    Column(verticalArrangement = Arrangement.spacedBy(4.dp)) {
                        for (line in stepUpHighlights(stepUp)) {
                            Text(line, style = MaterialTheme.typography.titleSmall, fontFamily = FontFamily.Monospace)
                        }
                    }
                }
                SelectionContainer {
                    Column(
                        Modifier
                            .fillMaxWidth()
                            .heightIn(max = 280.dp)
                            .verticalScroll(rememberScrollState()),
                    ) {
                        Text(stepUp.text, style = MaterialTheme.typography.bodySmall, fontFamily = FontFamily.Monospace)
                    }
                }
                Text(
                    if (expired) "abgelaufen" else "noch ${stepUpCountdownSeconds(remaining)} s",
                    style = MaterialTheme.typography.labelMedium,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
                if (error != null) StatusMessage(error, error = true)
            }
        },
        confirmButton = {
            TextButton(onClick = onConfirm, enabled = !authorizing && !expired) { Text("Bestätigen") }
        },
        dismissButton = {
            TextButton(onClick = { onCancel(null) }) { Text("Abbrechen") }
        },
    )
}
