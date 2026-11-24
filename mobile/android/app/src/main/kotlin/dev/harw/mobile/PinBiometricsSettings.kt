package dev.harw.mobile

import android.app.Activity
import android.content.Context
import android.content.ContextWrapper
import androidx.activity.compose.BackHandler
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Button
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.produceState
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.unit.dp
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.LifecycleOwner
import kotlinx.coroutines.flow.first

internal const val PinPrefs = "harw_pin_settings"
internal const val AppPinProfile = "app_pin_profile"

internal fun readAppPinProfile(context: Context): LocalUiaProfile? =
    LocalUiaProfile.parse(context.getSharedPreferences(PinPrefs, Context.MODE_PRIVATE).getString(AppPinProfile, null))

internal fun Context.findVaultActivity(): Activity? = when (this) {
    is Activity -> this
    is ContextWrapper -> baseContext.findVaultActivity()
    else -> null
}

@Composable
internal fun VaultAuthenticationScreen(
    session: LocalUiaDevSession,
    activity: Activity,
    title: String,
    onCancel: () -> Unit,
    onAuthorized: () -> Unit,
) {
    val view by session.view.collectAsState()
    AutoVaultBiometric(session, view, activity)
    BackHandler(onBack = onCancel)
    LaunchedEffect(view) {
        if (view == DevUiaView.Authorized) {
            (activity as? LifecycleOwner)?.lifecycle?.currentStateFlow?.first { it == Lifecycle.State.RESUMED }
            onAuthorized()
        }
    }
    Column(Modifier.fillMaxSize()) {
        HarwHeader(title = title, onBack = onCancel) {}
        when (val current = view) {
            is DevUiaView.Gate -> PinPad(session, current, activity, onCancel = onCancel)
            is DevUiaView.Stop -> Text(current.text, Modifier.padding(24.dp))
            DevUiaView.Authorized -> Unit
            else -> Text(stringResource(R.string.pin_checking), Modifier.padding(24.dp))
        }
    }
}

private data class PinSettingsState(
    val enrolled: Boolean,
    val biometrics: VaultBiometricAvailability,
)
private enum class PinSettingsAction { Enable, Remove, ChangeScope }

@Composable
internal fun PinBiometricsSettings(
    app: HarwApplication,
    activity: Activity,
    profile: LocalUiaProfile?,
    onBack: () -> Unit,
    onSetupVault: () -> Unit,
) {
    var revision by remember(profile) { mutableStateOf(0) }
    var appPin by remember { mutableStateOf(readAppPinProfile(app)) }
    val state by produceState<PinSettingsState?>(null, profile, revision) {
        value = withContext(Dispatchers.IO) {
            val disk = runCatching {
                profile?.let { AndroidVaultPinStateStore.localProfiles(app).lookup(it) }
                    ?.let { AndroidVaultPinStateStore.open(app, it) }
            }.getOrNull()
            PinSettingsState(
                runCatching { disk?.read()?.registration is VaultPinRegistration.Ready }.getOrDefault(false),
                disk?.let { AndroidVaultBiometricSlot.forSelectedStore(app, it).use { slot -> slot.availability() } }
                    ?: VaultBiometricAvailability.Unavailable,
            )
        }
    }
    var action by remember(profile) { mutableStateOf<PinSettingsAction?>(null) }
    var auth by remember(profile) { mutableStateOf<LocalUiaDevSession?>(null) }
    var resetConfirm by remember(profile) { mutableStateOf(false) }
    var resetting by remember(profile) { mutableStateOf(false) }
    var message by remember(profile) { mutableStateOf<Int?>(null) }
    val coroutine = rememberCoroutineScope()

    fun endAuth() {
        auth?.close()
        auth = null
        action = null
    }
    fun authenticate(next: PinSettingsAction, access: VaultAccess) {
        val target = if (next == PinSettingsAction.ChangeScope) appPin ?: profile else profile
        if (target == null) return
        auth = LocalUiaVaultReset.open(app, target, access)
        if (auth != null) action = next
    }
    val current = auth
    if (current != null) {
        DisposableEffect(current) { onDispose { current.close() } }
        VaultAuthenticationScreen(current, activity, stringResource(R.string.pin_biometrics_title), ::endAuth) {
            val completed = action
            if (completed == PinSettingsAction.ChangeScope) {
                val desired = if (appPin == null) current.profile else null
                if (LocalUiaVaultReset.setAppPin(app, desired, current.profile)) appPin = desired
                else message = R.string.pin_settings_save_failed
            }
            endAuth()
            revision++
        }
        return
    }
    Column(Modifier.fillMaxSize()) {
        HarwHeader(title = stringResource(R.string.pin_biometrics_title), onBack = onBack) {}
        Column(
            Modifier.verticalScroll(rememberScrollState()).padding(24.dp),
            verticalArrangement = Arrangement.spacedBy(12.dp),
        ) {
            if (profile == null && appPin == null) Text(stringResource(R.string.pin_settings_local_required))
            else if (state?.enrolled == false && profile != null && appPin == null) {
                Text(stringResource(R.string.pin_settings_requires_vault))
                Button(onClick = onSetupVault, enabled = !resetting) { Text(stringResource(R.string.pin_setup_vault)) }
            }

            if (appPin != null || state?.enrolled == true) {
                Text(stringResource(if (appPin == null) R.string.pin_scope_vault else R.string.pin_scope_app))
                Button(
                    enabled = !resetting,
                    onClick = { authenticate(PinSettingsAction.ChangeScope, VaultAccess.ConfirmPin) },
                ) {
                    Text(stringResource(if (appPin == null) R.string.pin_protect_app else R.string.pin_protect_vault_only))
                }
            }

            when (state?.biometrics) {
                VaultBiometricAvailability.Eligible -> if (AndroidVaultBiometricSlot.supported(app)) {
                    Button(
                        enabled = !resetting,
                        onClick = { authenticate(PinSettingsAction.Enable, VaultAccess.EnableBiometrics) },
                    ) { Text(stringResource(R.string.pin_biometric_setup)) }
                }
                VaultBiometricAvailability.Enrolled, VaultBiometricAvailability.Interrupted,
                VaultBiometricAvailability.RevocationPending, VaultBiometricAvailability.Revoked -> {
                    val enrolled = state?.biometrics == VaultBiometricAvailability.Enrolled
                    if (!enrolled) Text(stringResource(R.string.pin_biometric_repair_detail))
                    Button(
                        enabled = !resetting,
                        onClick = { authenticate(PinSettingsAction.Remove, VaultAccess.DisableBiometrics) },
                    ) {
                        Text(stringResource(if (enrolled) R.string.pin_biometric_remove else R.string.pin_biometric_reset))
                    }
                }
                else -> Unit
            }

            if (profile != null) {
                TextButton(enabled = !resetting && appPin != profile, onClick = { resetConfirm = true }) {
                    Text(stringResource(R.string.pin_vault_reset))
                }
                if (appPin == profile) Text(stringResource(R.string.pin_vault_reset_scope))
            }
            message?.let { Text(stringResource(it)) }
        }
    }
    if (resetConfirm && profile != null) {
        AlertDialog(
            onDismissRequest = { resetConfirm = false },
            title = { Text(stringResource(R.string.pin_vault_reset)) },
            text = { Text(stringResource(R.string.pin_vault_reset_detail)) },
            confirmButton = {
                TextButton(onClick = {
                    resetConfirm = false
                    resetting = true
                    coroutine.launch {
                        try {
                            message = if (LocalUiaVaultReset.erase(app, profile)) R.string.pin_vault_reset_done
                                else R.string.pin_settings_save_failed
                            revision++
                        } finally { resetting = false }
                    }
                }) { Text(stringResource(R.string.pin_vault_reset)) }
            },
            dismissButton = { TextButton(onClick = { resetConfirm = false }) { Text(stringResource(android.R.string.cancel)) } },
        )
    }
}
