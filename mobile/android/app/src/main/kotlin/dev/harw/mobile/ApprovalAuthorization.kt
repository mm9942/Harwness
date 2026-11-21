package dev.harw.mobile

import android.app.Activity
import android.app.KeyguardManager
import android.hardware.biometrics.BiometricManager
import android.hardware.biometrics.BiometricPrompt
import android.os.Build
import android.os.CancellationSignal
import androidx.activity.ComponentActivity
import androidx.activity.result.contract.ActivityResultContracts
import androidx.annotation.RequiresApi
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue

/** One platform authentication for one explicitly selected approval decision. */
internal class ApprovalAuthorization(private val activity: ComponentActivity) {
    private class Request(val result: (String?) -> Unit, val cancellation: CancellationSignal = CancellationSignal())

    private var pending by mutableStateOf<Request?>(null)
    var awaitingCredential by mutableStateOf(false)
        private set
    val busy: Boolean get() = pending != null || awaitingCredential

    private val credential = activity.registerForActivityResult(ActivityResultContracts.StartActivityForResult()) { result ->
        if (!awaitingCredential) return@registerForActivityResult
        awaitingCredential = false
        val request = pending ?: return@registerForActivityResult
        finish(request, if (result.resultCode == Activity.RESULT_OK) null else "Device authentication was canceled.")
    }

    fun restoreCredentialState(awaiting: Boolean) {
        awaitingCredential = awaiting
    }

    fun confirm(decisionLabel: String, result: (String?) -> Unit) {
        if (busy) return
        val request = Request(result)
        pending = request
        try {
            if (Build.VERSION.SDK_INT >= 30) {
                useBiometricPrompt(request, decisionLabel)
            } else {
                useDeviceCredential(request, decisionLabel)
            }
        } catch (_: Exception) {
            awaitingCredential = false
            finish(request, "Android could not start device authentication.")
        }
    }

    fun cancel() {
        val request = pending ?: return
        pending = null
        request.cancellation.cancel()
    }

    @RequiresApi(30)
    private fun useBiometricPrompt(request: Request, decisionLabel: String) {
        val authenticators = BiometricManager.Authenticators.BIOMETRIC_STRONG or
            BiometricManager.Authenticators.DEVICE_CREDENTIAL
        val biometrics = activity.getSystemService(BiometricManager::class.java)
        if (biometrics?.canAuthenticate(authenticators) != BiometricManager.BIOMETRIC_SUCCESS) {
            finish(request, "Set up biometrics or a secure screen lock to decide approvals.")
            return
        }
        val prompt = BiometricPrompt.Builder(activity)
            .setTitle("Confirm Harw approval decision")
            .setSubtitle(decisionLabel)
            .setAllowedAuthenticators(authenticators)
            .build()
        prompt.authenticate(request.cancellation, activity.mainExecutor, object : BiometricPrompt.AuthenticationCallback() {
            override fun onAuthenticationSucceeded(result: BiometricPrompt.AuthenticationResult) {
                finish(request, null)
            }

            override fun onAuthenticationError(errorCode: Int, errString: CharSequence) {
                finish(request, "Device authentication: $errString")
            }
        })
    }

    @Suppress("DEPRECATION") // The device-credential intent is the platform API on Android 9 and 10.
    private fun useDeviceCredential(request: Request, decisionLabel: String) {
        val lock = activity.getSystemService(KeyguardManager::class.java)
        val intent = lock?.takeIf { it.isDeviceSecure }
            ?.createConfirmDeviceCredentialIntent("Confirm Harw approval decision", decisionLabel)
        if (intent == null) {
            finish(request, "Set up a secure screen lock to decide approvals.")
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
