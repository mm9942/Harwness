package dev.harw.mobile

import android.content.Context
import android.content.SharedPreferences
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue

private const val ChatVisionPreferencesName = "harw_chat_vision_policy"
private const val MaxChatVisionOverrides = 128
private val chatVisionOverrideLock = Any()

/** Local UI policy, not an authorization, session mutation or provider preference. */
internal class ChatVisionOverrides(context: Context) {
    internal val prefs = context.applicationContext.getSharedPreferences(ChatVisionPreferencesName, Context.MODE_PRIVATE)

    fun key(scope: SessionHomeScope?, sessionId: String): String? {
        scope ?: return null
        if (sessionId.length !in 1..256) return null
        return visionPolicyDigest(scope.backend.name, scope.profileId, sessionId)
    }

    fun read(key: String?): VisionDescriptionMode? {
        key ?: return null
        val stored = prefs.all[key] as? String ?: return null
        return VisionDescriptionMode.entries.firstOrNull { it.name == stored.substringBefore(':') }
    }

    fun write(key: String, mode: VisionDescriptionMode?) = synchronized(chatVisionOverrideLock) {
        val edit = prefs.edit()
        if (mode == null) {
            edit.remove(key)
        } else {
            val otherEntries = prefs.all.filterKeys { it != key }
            otherEntries.entries.sortedBy {
                (it.value as? String)?.substringAfter(':')?.toLongOrNull() ?: 0L
            }.take((otherEntries.size - MaxChatVisionOverrides + 1).coerceAtLeast(0)).forEach { edit.remove(it.key) }
            edit.putString(key, "${mode.name}:${System.currentTimeMillis()}")
        }
        edit.apply()
    }
}

@Composable
internal fun rememberChatVisionOverride(
    store: ChatVisionOverrides,
    chatKey: String?,
): VisionDescriptionMode? {
    var mode by remember(store, chatKey) { mutableStateOf(store.read(chatKey)) }
    DisposableEffect(store, chatKey) {
        val listener = SharedPreferences.OnSharedPreferenceChangeListener { _, changed ->
            if (changed == chatKey || changed == null) mode = store.read(chatKey)
        }
        store.prefs.registerOnSharedPreferenceChangeListener(listener)
        mode = store.read(chatKey)
        onDispose { store.prefs.unregisterOnSharedPreferenceChangeListener(listener) }
    }
    return mode
}

@Composable
internal fun ChatVisionOverrideDialog(
    global: VisionDescriptionMode,
    current: VisionDescriptionMode?,
    onSelect: (VisionDescriptionMode?) -> Unit,
    onClose: () -> Unit,
) {
    AlertDialog(
        onDismissRequest = onClose,
        title = { Text("Bildbeschreibung in diesem Chat") },
        text = {
            Text(
                "Standard: ${global.visionDisplayName()}. Dieser Chat nutzt " +
                    (current?.visionDisplayName() ?: "den Standard") +
                    ". Die Auswahl ändert nur den Dialog, nicht Anbieter, Bestätigung oder Chatmodell.",
            )
        },
        confirmButton = {
            TextButton(onClick = { onSelect(VisionDescriptionMode.Local) }) { Text("Lokal") }
            TextButton(onClick = { onSelect(VisionDescriptionMode.Api) }) { Text("API") }
        },
        dismissButton = { TextButton(onClick = { onSelect(null) }) { Text("Standard verwenden") } },
    )
}

internal fun VisionDescriptionMode.visionDisplayName(): String =
    if (this == VisionDescriptionMode.Local) "Lokal" else "API"
