package dev.harw.mobile

import android.content.Context
import android.content.SharedPreferences
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.material3.Checkbox
import androidx.compose.material3.LinearProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.unit.dp
import kotlinx.coroutines.CancellationException
import uniffi.harw_mobile_ffi.ApiVisionChoice
import uniffi.harw_mobile_ffi.LocalVisionChoice
import uniffi.harw_mobile_ffi.ModelFileState

private const val LocalVisionPreferredKey = "vision_local_preferred_model"
private const val LocalVisionCompatibleKey = "vision_local_compatible_when_preferred_unavailable"
private const val NoLocalPreference = "__none__"
private const val MaxInstalledVisionCandidates = 32

internal data class LocalVisionPreference(val modelId: String?, val allowLoadedAlternative: Boolean)

internal fun readLocalVisionPreference(prefs: SharedPreferences): LocalVisionPreference = LocalVisionPreference(
    (prefs.all[LocalVisionPreferredKey] as? String)?.takeIf { it.length in 1..384 && it != NoLocalPreference },
    prefs.all[LocalVisionCompatibleKey] == true,
)

/** A preference never activates GGUF or selects a stale revision. Only native-enumerated loaded models can run. */
internal fun eligibleLoadedVisionModels(
    preference: LocalVisionPreference,
    loaded: List<LocalVisionChoice>,
): List<LocalVisionChoice> {
    val preferred = preference.modelId ?: return loaded
    val exact = loaded.filter { it.modelId == preferred }
    return when {
        exact.isNotEmpty() -> exact
        preference.allowLoadedAlternative -> loaded
        else -> emptyList()
    }
}

internal fun isLoadedVisionAlternative(preference: LocalVisionPreference, choice: LocalVisionChoice): Boolean =
    preference.modelId != null && preference.modelId != choice.modelId

private data class InstalledVisionOption(val modelId: String, val label: String, val loaded: Boolean)

@Composable
internal fun LocalVisionPreferenceSettings(
    context: Context,
    prefs: SharedPreferences,
) {
    val manager = remember(context) { ModelManager(ModelStore(context.applicationContext)) }
    val local = remember(context) { (context.applicationContext as? HarwApplication)?.localConnection }
    var configured by remember(prefs) { mutableStateOf(readLocalVisionPreference(prefs)) }
    var options by remember(manager) { mutableStateOf<List<InstalledVisionOption>?>(null) }
    var loaded by remember(local) { mutableStateOf<List<LocalVisionChoice>>(emptyList()) }
    var cannotReadStorage by remember { mutableStateOf(false) }
    var requested by remember(context) { mutableStateOf(readLocalImageConcurrency(context)) }
    DisposableEffect(context, prefs) {
        val listener = SharedPreferences.OnSharedPreferenceChangeListener { _, changed ->
            if (changed == PREF_LOCAL_IMAGE_CONCURRENCY) requested = readLocalImageConcurrency(context)
        }
        prefs.registerOnSharedPreferenceChangeListener(listener)
        onDispose { prefs.unregisterOnSharedPreferenceChangeListener(listener) }
    }
    LaunchedEffect(manager, local, requested) {
        val native = try {
            local?.localVisionChoices().orEmpty()
        } catch (cancelled: CancellationException) {
            throw cancelled
        } catch (_: Exception) {
            emptyList()
        }
        loaded = native
        try {
            val complete = manager.local().filter { it.state == ModelFileState.COMPLETE }
            options = complete.take(MaxInstalledVisionCandidates).mapNotNull { model ->
                val isLoaded = native.any { it.modelId == model.id }
                val compatible = if (isLoaded) true else try {
                    manager.projector(model.id)?.compatible == true
                } catch (cancelled: CancellationException) {
                    throw cancelled
                } catch (_: Exception) {
                    false
                }
                if (!compatible) null else InstalledVisionOption(model.id, model.label.ifBlank { model.id }, isLoaded)
            }
        } catch (cancelled: CancellationException) {
            throw cancelled
        } catch (_: Exception) {
            cannotReadStorage = true
            options = emptyList()
        }
    }

    Text(stringResource(R.string.harw_vision_settings_local_preferred_model), style = MaterialTheme.typography.bodyLarge)
    val candidates = options.orEmpty().map { model ->
        model.modelId to stringResource(
            if (model.loaded) R.string.harw_vision_settings_local_model_loaded
            else R.string.harw_vision_settings_local_model_installed,
            model.label,
        )
    }
    val current = candidates.firstOrNull { it.first == configured.modelId }
    when {
        options == null -> LinearProgressIndicator(Modifier.fillMaxWidth())
        else -> VisionSettingsDropdown(
            label = current?.second ?: configured.modelId?.let { stringResource(R.string.harw_vision_settings_local_model_unavailable, it) }
                ?: stringResource(R.string.harw_vision_settings_local_no_preference),
            options = listOf(NoLocalPreference to stringResource(R.string.harw_vision_settings_local_no_preference)) + candidates,
            onSelect = { id ->
                val edit = prefs.edit()
                if (id == NoLocalPreference) edit.remove(LocalVisionPreferredKey)
                else if (candidates.any { it.first == id }) edit.putString(LocalVisionPreferredKey, id)
                edit.apply()
                configured = readLocalVisionPreference(prefs)
            },
        )
    }
    if (cannotReadStorage) Text(stringResource(R.string.harw_vision_settings_local_models_unreadable))
    else if (options?.isEmpty() == true) Text(stringResource(R.string.harw_vision_settings_local_no_compatible_projector))
    Row(verticalAlignment = Alignment.CenterVertically) {
        Checkbox(
            checked = configured.allowLoadedAlternative,
            onCheckedChange = { allowed ->
                prefs.edit().putBoolean(LocalVisionCompatibleKey, allowed).apply()
                configured = readLocalVisionPreference(prefs)
            },
        )
        Text(stringResource(R.string.harw_vision_settings_local_offer_loaded_alternative))
    }
    val effective = eligibleLoadedVisionModels(configured, loaded).singleOrNull()
    Text(
        if (effective != null) stringResource(
            if (isLoadedVisionAlternative(configured, effective)) R.string.harw_vision_settings_local_alternative_effective
            else R.string.harw_vision_settings_local_current_effective,
            effective.modelId,
            effective.effectiveParallelism.toInt(),
        ) else stringResource(R.string.harw_vision_settings_local_effective_unavailable),
        style = MaterialTheme.typography.bodySmall,
    )
}

/** No input, image URI, result, or permission to dispatch travels with the destination. */
internal data class ConfiguredVisionFallbackInvite(val profileKey: String, val choice: ApiVisionChoice)

@Composable
internal fun LocalVisionApiAlternative(
    onConfiguredApi: (ConfiguredVisionFallbackInvite) -> Unit,
    onManualApi: () -> Unit,
) {
    val context = androidx.compose.ui.platform.LocalContext.current
    val prefs = remember(context) { context.getSharedPreferences(UI_PREFS, Context.MODE_PRIVATE) }
    var checked by remember(context) { mutableStateOf(false) }
    var invite by remember(context) { mutableStateOf<ConfiguredVisionFallbackInvite?>(null) }
    LaunchedEffect(context) {
        try {
            val routes = appApiVisionRoutes(context)
            invite = readLocalVisionApiFallback(prefs, routes)?.let { ConfiguredVisionFallbackInvite(routes.profileKey, it) }
        } catch (cancelled: CancellationException) {
            throw cancelled
        } catch (_: Exception) {
            invite = null
        } finally {
            checked = true
        }
    }
    val current = invite
    Column(verticalArrangement = Arrangement.spacedBy(4.dp)) {
        if (!checked) {
            Text(stringResource(R.string.harw_vision_settings_api_alternative_checking), style = MaterialTheme.typography.bodySmall)
        } else if (current != null) {
            val choice = current.choice
            Text(stringResource(R.string.harw_vision_settings_api_alternative_saved, choice.routeId, choice.modelId))
            Text(choice.endpoint, style = MaterialTheme.typography.bodySmall)
            TextButton(onClick = { onConfiguredApi(current) }) { Text(stringResource(R.string.harw_vision_settings_api_alternative_new_selection)) }
        } else {
            TextButton(onClick = onManualApi) { Text(stringResource(R.string.harw_vision_settings_api_choose_manually)) }
        }
        Text(stringResource(R.string.harw_vision_settings_api_alternative_privacy), style = MaterialTheme.typography.bodySmall)
    }
}
