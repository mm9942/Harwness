package dev.harw.mobile

import android.content.Context
import android.content.SharedPreferences
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.LinearProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.SegmentedButton
import androidx.compose.material3.SegmentedButtonDefaults
import androidx.compose.material3.SingleChoiceSegmentedButtonRow
import androidx.compose.material3.Slider
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.unit.dp
import kotlin.math.roundToInt
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import org.json.JSONObject
import uniffi.harw_mobile_ffi.ApiVisionChoice
import uniffi.harw_mobile_ffi.ProviderStore

internal enum class VisionDescriptionMode { Local, Api }
private const val VisionDescriptionModeKey = "vision_description_mode"
private const val ApiVisionDefaultPrefix = "vision_api_default_"
private const val ApiVisionFallbackPrefix = "vision_api_fallback_"
private const val ApiVisionFromLocalPrefix = "vision_local_api_fallback_"

internal fun visionDescriptionMode(prefs: SharedPreferences): VisionDescriptionMode =
    if (prefs.all[VisionDescriptionModeKey] == VisionDescriptionMode.Api.name) VisionDescriptionMode.Api
    else VisionDescriptionMode.Local

@Composable
internal fun rememberVisionDescriptionMode(context: Context): VisionDescriptionMode {
    val prefs = remember(context) { context.getSharedPreferences(UI_PREFS, Context.MODE_PRIVATE) }
    var mode by remember(prefs) { mutableStateOf(visionDescriptionMode(prefs)) }
    DisposableEffect(prefs) {
        val listener = SharedPreferences.OnSharedPreferenceChangeListener { _, key ->
            if (key == VisionDescriptionModeKey) mode = visionDescriptionMode(prefs)
        }
        prefs.registerOnSharedPreferenceChangeListener(listener)
        mode = visionDescriptionMode(prefs)
        onDispose { prefs.unregisterOnSharedPreferenceChangeListener(listener) }
    }
    return mode
}

internal data class AppApiVisionRouteSet(
    val store: ProviderStore,
    val profileKey: String,
    val choices: List<ApiVisionChoice>,
)

/** No Cloud/chat connection required. Reading choices makes no provider request. */
internal suspend fun appApiVisionRoutes(context: Context): AppApiVisionRouteSet = withContext(Dispatchers.IO) {
    val app = context.applicationContext
    val store = ProviderStore(app.filesDir.path, AndroidPlatformKeystore(app))
    AppApiVisionRouteSet(store, store.apiVisionProfileKey(), store.apiVisionChoices())
}

/** Settings never grant sending authority; choices must still exist in the native route list. */
internal fun readApiVisionDefault(prefs: SharedPreferences, routeSet: AppApiVisionRouteSet): ApiVisionChoice? =
    readSavedApiVisionChoice(prefs, routeSet, ApiVisionDefaultPrefix)

internal fun readApiVisionFallback(prefs: SharedPreferences, routeSet: AppApiVisionRouteSet): ApiVisionChoice? =
    readSavedApiVisionChoice(prefs, routeSet, ApiVisionFallbackPrefix)
        ?.takeUnless { sameVisionDestination(it, readApiVisionDefault(prefs, routeSet)) }

internal fun readLocalVisionApiFallback(prefs: SharedPreferences, routeSet: AppApiVisionRouteSet): ApiVisionChoice? =
    readSavedApiVisionChoice(prefs, routeSet, ApiVisionFromLocalPrefix)

internal fun sameVisionDestination(a: ApiVisionChoice, b: ApiVisionChoice?): Boolean =
    b != null && a.routeId == b.routeId && a.modelId == b.modelId

private fun readSavedApiVisionChoice(
    prefs: SharedPreferences,
    routeSet: AppApiVisionRouteSet,
    prefix: String,
): ApiVisionChoice? {
    val encoded = prefs.all[prefix + routeSet.profileKey] as? String ?: return null
    if (encoded.length > 4096) return null
    val saved = try { JSONObject(encoded) } catch (_: org.json.JSONException) { return null }
    return routeSet.choices.singleOrNull { choice ->
        saved.opt("route") == choice.routeId && saved.opt("model") == choice.modelId &&
            saved.opt("endpoint") == choice.endpoint && saved.opt("stream") == choice.stream
    }
}

private fun saveApiVisionChoice(
    prefs: SharedPreferences,
    routeSet: AppApiVisionRouteSet,
    prefix: String,
    choice: ApiVisionChoice?,
) {
    val key = prefix + routeSet.profileKey
    val edit = prefs.edit()
    if (choice == null) {
        edit.remove(key)
    } else {
        if (choice !in routeSet.choices) return
        edit.putString(key, JSONObject().put("route", choice.routeId).put("model", choice.modelId)
            .put("endpoint", choice.endpoint).put("stream", choice.stream).toString())
    }
    edit.apply()
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
internal fun VisionDescriptionSettings(
    context: Context,
    onConfigureProviders: () -> Unit,
    localParallelism: @Composable () -> Unit,
) {
    val prefs = remember(context) { context.getSharedPreferences(UI_PREFS, Context.MODE_PRIVATE) }
    val mode = rememberVisionDescriptionMode(context)
    var showOptionalApiSetup by remember { mutableStateOf(false) }
    Column(Modifier.fillMaxWidth().padding(horizontal = 16.dp), verticalArrangement = Arrangement.spacedBy(5.dp)) {
        Text(stringResource(R.string.harw_vision_settings_default_title), style = MaterialTheme.typography.bodyLarge)
        SingleChoiceSegmentedButtonRow(Modifier.fillMaxWidth()) {
            for ((index, option) in VisionDescriptionMode.entries.withIndex()) {
                SegmentedButton(
                    selected = mode == option,
                    onClick = { prefs.edit().putString(VisionDescriptionModeKey, option.name).apply() },
                    shape = SegmentedButtonDefaults.itemShape(index, 2),
                ) { Text(stringResource(
                    if (option == VisionDescriptionMode.Local) R.string.harw_vision_settings_mode_local
                    else R.string.harw_vision_settings_mode_api,
                )) }
            }
        }
        Text(
            stringResource(R.string.harw_vision_settings_default_explanation),
            style = MaterialTheme.typography.bodySmall,
        )
        if (mode == VisionDescriptionMode.Local) {
            LocalVisionPreferenceSettings(context, prefs)
            TextButton(onClick = { showOptionalApiSetup = !showOptionalApiSetup }) {
                Text(if (showOptionalApiSetup) stringResource(R.string.harw_vision_settings_hide_api_settings) else stringResource(R.string.harw_vision_settings_show_api_settings))
            }
        }
        if (mode == VisionDescriptionMode.Api || showOptionalApiSetup) {
            ApiVisionDefaultSettings(context, prefs, onConfigureProviders, fallbackFromLocal = mode == VisionDescriptionMode.Local)
        }
    }
    if (mode == VisionDescriptionMode.Local) localParallelism()
}

@Composable
private fun ApiVisionDefaultSettings(
    context: Context,
    prefs: SharedPreferences,
    onConfigureProviders: () -> Unit,
    fallbackFromLocal: Boolean,
) {
    val fallbackPrefix = if (fallbackFromLocal) ApiVisionFromLocalPrefix else ApiVisionFallbackPrefix
    val evidenceStore = remember(context) { ApiVisionEvidenceStore(context) }
    var routeSet by remember { mutableStateOf<AppApiVisionRouteSet?>(null) }
    var failed by remember { mutableStateOf(false) }
    var routeId by remember { mutableStateOf<String?>(null) }
    var selected by remember { mutableStateOf<ApiVisionChoice?>(null) }
    var fallbackRouteId by remember(fallbackFromLocal) { mutableStateOf<String?>(null) }
    var fallback by remember(fallbackFromLocal) { mutableStateOf<ApiVisionChoice?>(null) }
    var parallelism by remember { mutableIntStateOf(apiVisionConfiguredParallelism(prefs.all[ApiVisionParallelismPref])) }
    LaunchedEffect(context, fallbackFromLocal) {
        failed = false
        routeSet = null
        try {
            val found = appApiVisionRoutes(context)
            selected = readApiVisionDefault(prefs, found)
            routeId = selected?.routeId
            fallback = if (fallbackFromLocal) readLocalVisionApiFallback(prefs, found) else readApiVisionFallback(prefs, found)
            fallbackRouteId = fallback?.routeId
            routeSet = found
        } catch (cancelled: CancellationException) {
            throw cancelled
        } catch (_: Exception) {
            failed = true
        }
    }

    val loaded = routeSet
    when {
        failed -> Text(stringResource(R.string.harw_vision_settings_api_routes_failed))
        loaded == null -> LinearProgressIndicator(Modifier.fillMaxWidth())
        loaded.choices.isEmpty() -> Text(stringResource(R.string.harw_vision_settings_api_routes_empty))
        else -> {
            val routes = loaded.choices.distinctBy { it.routeId }
            VisionSettingsDropdown(
                label = routeId ?: stringResource(R.string.harw_vision_settings_choose_api_route),
                options = routes.map { it.routeId to "${it.routeId} · ${it.endpoint}" },
                onSelect = { next ->
                    if (routeId != next) {
                        routeId = next
                    }
                },
            )
            val models = loaded.choices.filter { it.routeId == routeId }
            VisionSettingsDropdown(
                label = selected?.takeIf { it.routeId == routeId }?.modelId
                    ?: stringResource(R.string.harw_vision_settings_choose_api_model),
                options = models.map { it.modelId to it.modelId },
                onSelect = { id ->
                    models.singleOrNull { it.modelId == id }?.let { choice ->
                        selected = choice
                        saveApiVisionChoice(prefs, loaded, ApiVisionDefaultPrefix, choice)
                        // Local and API fallback are distinct preferences. An API
                        // fallback identical to the primary is temporarily ineligible,
                        // not deleted if the primary changes back to a different one.
                        if (!fallbackFromLocal) {
                            fallback = readApiVisionFallback(prefs, loaded)
                            fallbackRouteId = fallback?.routeId
                        }
                    }
                },
            )
            Text(
                selected?.let {
                    stringResource(R.string.harw_vision_settings_api_preselection, it.routeId, it.modelId, it.endpoint)
                }
                    ?: stringResource(R.string.harw_vision_settings_no_preselection),
                style = MaterialTheme.typography.bodySmall,
            )
            Text(
                if (fallbackFromLocal) stringResource(R.string.harw_vision_settings_local_api_alternative_title) else stringResource(R.string.harw_vision_settings_api_failure_alternative_title),
                style = MaterialTheme.typography.bodyLarge,
            )
            Text(
                stringResource(R.string.harw_vision_settings_confirm_new_selection),
                style = MaterialTheme.typography.bodySmall,
            )
            val alternatives = if (fallbackFromLocal) loaded.choices else loaded.choices.filterNot { sameVisionDestination(it, selected) }
            VisionSettingsDropdown(
                label = fallbackRouteId ?: stringResource(R.string.harw_vision_settings_choose_alternative_route),
                options = alternatives.distinctBy { it.routeId }.map { it.routeId to (it.routeId + " · " + it.endpoint) },
                onSelect = { next ->
                    if (fallbackRouteId != next) {
                        fallbackRouteId = next
                    }
                },
            )
            val alternateModels = alternatives.filter { it.routeId == fallbackRouteId }
            VisionSettingsDropdown(
                label = fallback?.takeIf { it.routeId == fallbackRouteId }?.modelId
                    ?: stringResource(R.string.harw_vision_settings_choose_alternative_model),
                options = alternateModels.map { it.modelId to it.modelId },
                onSelect = { id ->
                    alternateModels.singleOrNull { it.modelId == id }?.let { choice ->
                        fallback = choice
                        saveApiVisionChoice(prefs, loaded, fallbackPrefix, choice)
                    }
                },
            )
            fallback?.let { alternate ->
                Text(
                    alternate.routeId + " · " + alternate.modelId + "\n" + alternate.endpoint,
                    style = MaterialTheme.typography.bodySmall,
                )
            }
        }
    }
    if (loaded != null && (prefs.contains(fallbackPrefix + loaded.profileKey) || fallbackRouteId != null)) {
        TextButton(onClick = {
            saveApiVisionChoice(prefs, loaded, fallbackPrefix, null)
            fallback = null
            fallbackRouteId = null
        }) { Text(stringResource(R.string.harw_vision_settings_remove_api_alternative)) }
    }
    TextButton(onClick = onConfigureProviders) { Text(stringResource(R.string.harw_vision_settings_manage_device_providers)) }
    Text(stringResource(R.string.harw_vision_settings_api_parallelism, parallelism), style = MaterialTheme.typography.bodyLarge)
    Slider(
        value = parallelism.toFloat(),
        onValueChange = { parallelism = it.roundToInt() },
        onValueChangeFinished = { prefs.edit().putInt(ApiVisionParallelismPref, parallelism).apply() },
        valueRange = ApiVisionMinParallelism.toFloat()..ApiVisionMaxRefs.toFloat(),
        steps = ApiVisionMaxRefs - ApiVisionMinParallelism - 1,
    )
    Text(stringResource(R.string.harw_vision_settings_api_parallelism_detail), style = MaterialTheme.typography.bodySmall)
    if (!failed && loaded != null) {
        val targets = if (fallbackFromLocal) listOfNotNull(fallback) else listOfNotNull(selected, fallback)
        for (target in targets) {
            val offered = evidenceStore.recommendation(loaded.profileKey, target) ?: continue
            Text(
                stringResource(R.string.harw_vision_settings_observed_parallelism, target.routeId, target.modelId, offered),
                style = MaterialTheme.typography.bodySmall,
            )
            if (offered != parallelism) {
                TextButton(onClick = {
                    // Re-read the scoped observation on click: it can expire or
                    // be invalidated while Settings is open.
                    if (evidenceStore.recommendation(loaded.profileKey, target) == offered) {
                        parallelism = offered
                        prefs.edit().putInt(ApiVisionParallelismPref, offered).apply()
                    }
                }) {
                    Text(stringResource(R.string.harw_vision_settings_apply_observed_parallelism, offered))
                }
            }
        }
    }
}

@Composable
internal fun VisionSettingsDropdown(
    label: String,
    options: List<Pair<String, String>>,
    onSelect: (String) -> Unit,
) {
    var expanded by remember { mutableStateOf(false) }
    Box {
        OutlinedButton(onClick = { expanded = true }, enabled = options.isNotEmpty()) { Text(label) }
        DropdownMenu(expanded = expanded, onDismissRequest = { expanded = false }) {
            for ((key, text) in options) {
                DropdownMenuItem(text = { Text(text) }, onClick = { expanded = false; onSelect(key) })
            }
        }
    }
}
