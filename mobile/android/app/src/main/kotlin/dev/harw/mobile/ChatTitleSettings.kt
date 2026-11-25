package dev.harw.mobile

import android.content.Context
import android.content.SharedPreferences
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Switch
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
import kotlinx.coroutines.CancellationException
import org.json.JSONObject

internal enum class ChatTitleModelKind { Local, ConfiguredProvider, ChatgptAccount }

internal data class ChatTitleModelKey(
    val kind: ChatTitleModelKind,
    val provider: String,
    val model: String,
    /** Identifies the exact route/account, not a credential. Never display or log this field. */
    val binding: String,
)

internal data class ChatTitleModelOption(
    val key: ChatTitleModelKey,
    val providerLabel: String,
    val modelLabel: String,
    val ready: Boolean = true,
)

internal data class ChatTitleCatalog(
    val profile: String,
    val options: List<ChatTitleModelOption>,
)

internal data class ChatTitlePreferences(
    val enabled: Boolean = false,
    val model: ChatTitleModelKey? = null,
    val allowLoadedLocalAlternative: Boolean = true,
    val apiFallback: ChatTitleModelKey? = null,
)

private const val TitlePreferencePrefix = "session_title_model_"
private const val MaxSavedTitleSettingBytes = 4096

internal fun isChatTitlePreferenceKey(key: String?): Boolean = key?.startsWith(TitlePreferencePrefix) == true

internal fun mayHaveEnabledChatTitle(prefs: SharedPreferences): Boolean =
    prefs.all.any { (key, value) ->
        isChatTitlePreferenceKey(key) && (value as? String)?.let {
            it.length <= MaxSavedTitleSettingBytes && it.contains("\"enabled\":true")
        } == true
    }


private fun titlePreferenceKey(profile: String): String? = profile
    .takeIf { it.length in 1..128 && it.all { ch -> ch.isLetterOrDigit() || ch in "-_" } }
    ?.let { TitlePreferencePrefix + it }

internal fun readChatTitlePreferences(prefs: SharedPreferences, profile: String): ChatTitlePreferences {
    val key = titlePreferenceKey(profile) ?: return ChatTitlePreferences()
    val value = (prefs.all[key] as? String)?.takeIf { it.length <= MaxSavedTitleSettingBytes }
        ?: return ChatTitlePreferences()
    return try {
        val content = JSONObject(value)
        val model = readTitleModel(content) ?: return ChatTitlePreferences()
        ChatTitlePreferences(
            enabled = content.getBoolean("enabled"),
            model = model,
            allowLoadedLocalAlternative = content.optBoolean("allowLoadedLocal", true),
            apiFallback = content.optJSONObject("fallback")?.let(::readTitleModel)
                ?.takeUnless { it.kind == ChatTitleModelKind.Local || it == model },
        )
    } catch (_: org.json.JSONException) {
        ChatTitlePreferences()
    }
}

internal fun writeChatTitlePreferences(
    prefs: SharedPreferences,
    profile: String,
    value: ChatTitlePreferences,
) {
    val key = titlePreferenceKey(profile) ?: return
    val model = value.model
    if (model == null) {
        prefs.edit().remove(key).apply()
        return
    }
    val serialized = titleModelJson(model).put("enabled", value.enabled)
        .put("allowLoadedLocal", value.allowLoadedLocalAlternative)
        .apply { value.apiFallback?.let { put("fallback", titleModelJson(it)) } }.toString()
    if (serialized.length <= MaxSavedTitleSettingBytes) prefs.edit().putString(key, serialized).apply()
}

private fun readTitleModel(content: JSONObject): ChatTitleModelKey? {
    val kind = ChatTitleModelKind.entries.singleOrNull { it.name == content.getString("kind") } ?: return null
    return ChatTitleModelKey(kind, content.getString("provider"), content.getString("model"), content.getString("binding"))
}

private fun titleModelJson(model: ChatTitleModelKey): JSONObject = JSONObject()
    .put("kind", model.kind.name).put("provider", model.provider).put("model", model.model)
    .put("binding", model.binding)

@Composable
internal fun rememberChatTitlePreferences(context: Context, profile: String?): ChatTitlePreferences {
    val prefs = remember(context) { context.getSharedPreferences(UI_PREFS, Context.MODE_PRIVATE) }
    var value by remember(prefs, profile) {
        mutableStateOf(profile?.let { readChatTitlePreferences(prefs, it) } ?: ChatTitlePreferences())
    }
    DisposableEffect(prefs, profile) {
        val key = profile?.let(::titlePreferenceKey)
        val listener = SharedPreferences.OnSharedPreferenceChangeListener { _, changed ->
            if (profile != null && changed == key) value = readChatTitlePreferences(prefs, profile)
        }
        prefs.registerOnSharedPreferenceChangeListener(listener)
        if (profile != null) value = readChatTitlePreferences(prefs, profile)
        onDispose { prefs.unregisterOnSharedPreferenceChangeListener(listener) }
    }
    return value
}

@Composable
internal fun ChatTitleSettings(
    context: Context,
    loadCatalog: suspend () -> ChatTitleCatalog,
) {
    val prefs = remember(context) { context.getSharedPreferences(UI_PREFS, Context.MODE_PRIVATE) }
    var refresh by remember { mutableIntStateOf(0) }
    var catalog by remember { mutableStateOf<ChatTitleCatalog?>(null) }
    var loadFailed by remember { mutableStateOf(false) }
    LaunchedEffect(refresh) {
        try {
            loadFailed = false
            catalog = loadCatalog()
        } catch (cancelled: CancellationException) {
            throw cancelled
        } catch (_: Exception) {
            catalog = null
            loadFailed = true
        }
    }
    val current = catalog
    val saved = rememberChatTitlePreferences(context, current?.profile)
    val valid = current?.options?.singleOrNull { it.key == saved.model }
    var providerFilter by remember(current?.profile, saved.model?.provider) { mutableStateOf(saved.model?.provider) }
    var providersOpen by remember { mutableStateOf(false) }
    var modelsOpen by remember { mutableStateOf(false) }
    Column(Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 10.dp), verticalArrangement = Arrangement.spacedBy(6.dp)) {
        Text(stringResource(R.string.chat_title_heading), style = MaterialTheme.typography.titleSmall)
        Text(
            stringResource(R.string.chat_title_purpose),
            style = MaterialTheme.typography.bodySmall,
        )
        if (current == null) {
            Text(if (loadFailed) stringResource(R.string.chat_title_load_failed) else stringResource(R.string.chat_title_loading))
        } else {
            Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.SpaceBetween) {
                Text(if (saved.enabled) stringResource(R.string.chat_title_on) else stringResource(R.string.chat_title_off))
                Switch(
                    checked = saved.enabled,
                    enabled = valid != null || saved.enabled,
                    onCheckedChange = { on ->
                        if (!on || valid != null) writeChatTitlePreferences(prefs, current.profile, saved.copy(enabled = on))
                    },
                )
            }
            val providers = current.options.distinctBy { it.key.provider }
            Box {
                OutlinedButton(onClick = { providersOpen = true }, enabled = providers.isNotEmpty()) {
                    Text(providers.firstOrNull { it.key.provider == providerFilter }?.providerLabel ?: stringResource(R.string.chat_title_pick_provider))
                }
                DropdownMenu(expanded = providersOpen, onDismissRequest = { providersOpen = false }) {
                    for (option in providers) {
                        DropdownMenuItem(text = { Text(option.providerLabel) }, onClick = {
                            providerFilter = option.key.provider
                            providersOpen = false
                        })
                    }
                }
            }
            val models = current.options.filter { it.key.provider == providerFilter }
            Box {
                OutlinedButton(onClick = { modelsOpen = true }, enabled = models.isNotEmpty()) {
                    Text(valid?.takeIf { it.key.provider == providerFilter }?.modelLabel ?: stringResource(R.string.chat_title_pick_model))
                }
                DropdownMenu(expanded = modelsOpen, onDismissRequest = { modelsOpen = false }) {
                    for (option in models) {
                        DropdownMenuItem(text = { Text(option.modelLabel) }, onClick = {
                            // Switching an online destination always requires a fresh enable gesture.
                            writeChatTitlePreferences(prefs, current.profile, saved.copy(
                                enabled = false, model = option.key, apiFallback = saved.apiFallback?.takeUnless { it == option.key },
                            ))
                            modelsOpen = false
                        })
                    }
                }
            }
            if (valid != null && valid.key.kind != ChatTitleModelKind.Local) {
                Text(stringResource(R.string.chat_title_provider_consent, valid.providerLabel), style = MaterialTheme.typography.bodySmall)
            }
            if (saved.model != null && valid == null) Text(stringResource(R.string.chat_title_selected_missing))
            if (saved.model?.kind == ChatTitleModelKind.Local) {
                Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.SpaceBetween) {
                    Text(stringResource(R.string.chat_title_allow_loaded), Modifier.weight(1f))
                    Switch(
                        checked = saved.allowLoadedLocalAlternative,
                        onCheckedChange = { useLoaded ->
                            writeChatTitlePreferences(prefs, current.profile, saved.copy(allowLoadedLocalAlternative = useLoaded))
                        },
                    )
                }
                if (valid?.ready != true) {
                    val loaded = current.options.singleOrNull { it.key.kind == ChatTitleModelKind.Local && it.ready }
                    val detail = if (loaded != null && saved.allowLoadedLocalAlternative) {
                        stringResource(R.string.chat_title_can_use_loaded, loaded.modelLabel)
                    } else {
                        stringResource(R.string.chat_title_preferred_unloaded)
                    }
                    Text(detail, style = MaterialTheme.typography.bodySmall)
                }
                ChatTitleApiFallbackSetting(
                    current.options.filter { it.key.kind != ChatTitleModelKind.Local },
                    saved.apiFallback,
                ) { fallback ->
                    writeChatTitlePreferences(prefs, current.profile, saved.copy(apiFallback = fallback))
                }
            }
            if (providers.isEmpty()) Text(stringResource(R.string.chat_title_empty_catalog))
        }
        TextButton(onClick = { refresh++ }) { Text(stringResource(R.string.chat_title_refresh)) }
    }
}

@Composable
private fun ChatTitleApiFallbackSetting(
    options: List<ChatTitleModelOption>,
    selected: ChatTitleModelKey?,
    onPick: (ChatTitleModelKey?) -> Unit,
) {
    var open by remember { mutableStateOf(false) }
    val current = options.singleOrNull { it.key == selected }
    Text(stringResource(R.string.chat_title_fallback_heading), style = MaterialTheme.typography.bodyMedium)
    Box {
        OutlinedButton(onClick = { open = true }, enabled = options.isNotEmpty() || selected != null) {
            Text(current?.let { "${it.providerLabel} · ${it.modelLabel}" } ?: stringResource(R.string.chat_title_pick_fallback))
        }
        DropdownMenu(open, onDismissRequest = { open = false }) {
            if (selected != null) {
                DropdownMenuItem(text = { Text(stringResource(R.string.chat_title_no_fallback)) }, onClick = {
                    onPick(null); open = false
                })
            }
            for (option in options) {
                DropdownMenuItem(text = { Text("${option.providerLabel} · ${option.modelLabel}") }, onClick = {
                    onPick(option.key); open = false
                })
            }
        }
    }
    if (selected != null && current == null) Text(stringResource(R.string.chat_title_fallback_missing))
    Text(stringResource(R.string.chat_title_fallback_consent), style = MaterialTheme.typography.bodySmall)
}
