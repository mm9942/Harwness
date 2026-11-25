package dev.harw.mobile

import android.content.Context
import android.content.SharedPreferences
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import java.security.MessageDigest
import java.util.Base64
import org.json.JSONArray

private const val HomePreferencesName = "harw_session_home"
internal const val SessionHomePinsLimit = 32
private const val SessionHomeIdLimit = 256
internal const val SessionHomeRecentKey = "recent_limit"
internal const val SessionHomeSystemKey = "show_system_sessions"
private const val LegacyDreamSessionsKey = "show_dream_sessions"
private val pinLock = Any()

internal fun sessionHomeScope(state: WorkspaceState): SessionHomeScope? {
    return if (state.local) {
        SessionHomeScope(SessionHomeBackend.Local, "device_default")
    } else {
        val profile = state.profile ?: return null
        val device = state.deviceNodeId ?: return null
        val identity = "${profile.windowKey()}\n$device"
        val digest = MessageDigest.getInstance("SHA-256").digest(identity.encodeToByteArray())
        SessionHomeScope(SessionHomeBackend.Cloud, Base64.getUrlEncoder().withoutPadding().encodeToString(digest))
    }
}

internal class SessionHomePreferences(context: Context) {
    val prefs = context.applicationContext.getSharedPreferences(HomePreferencesName, Context.MODE_PRIVATE)

    fun recentLimit(): Int = recentSessionHomeLimit(prefs.all[SessionHomeRecentKey] as? Int)

    fun showSystemSessions(): Boolean = prefs.all[SessionHomeSystemKey] == true

    fun setShowSystemSessions(value: Boolean) {
        prefs.edit().putBoolean(SessionHomeSystemKey, value).remove(LegacyDreamSessionsKey).apply()
    }


    fun setRecentLimit(value: Int) {
        if (value in MinimumRecentLimit..MaximumRecentLimit) prefs.edit().putInt(SessionHomeRecentKey, value).apply()
    }

    private fun directoryKey(scope: SessionHomeScope, field: String) =
        "directory_${scope.backend.name}_${scope.profileId}_$field"

    fun grouping(scope: SessionHomeScope): HomeGrouping =
        HomeGrouping.entries.firstOrNull { it.name == prefs.all[directoryKey(scope, "grouping")] }
            ?: HomeGrouping.Projects

    fun setGrouping(scope: SessionHomeScope, grouping: HomeGrouping) {
        prefs.edit().putString(directoryKey(scope, "grouping"), grouping.name).apply()
    }

    fun sort(scope: SessionHomeScope): HomeListSort =
        HomeListSort.entries.firstOrNull { it.name == prefs.all[directoryKey(scope, "sort")] }
            ?: HomeListSort.Recent

    fun setSort(scope: SessionHomeScope, sort: HomeListSort) {
        prefs.edit().putString(directoryKey(scope, "sort"), sort.name).apply()
    }

    fun pinKey(scope: SessionHomeScope) = "pins_${scope.backend.name}_${scope.profileId}"

    fun pins(scope: SessionHomeScope): List<SessionHomeId> {
        val stored = prefs.all[pinKey(scope)] as? String ?: return emptyList()
        if (stored.length > SessionHomePinsLimit * SessionHomeIdLimit * 6 + 128) return emptyList()
        return try {
            val values = JSONArray(stored)
            (0 until minOf(values.length(), SessionHomePinsLimit)).mapNotNull { index ->
                (values.opt(index) as? String)?.takeIf { it.length in 1..SessionHomeIdLimit }
                    ?.let { SessionHomeId(scope, it) }
            }.distinct()
        } catch (_: org.json.JSONException) {
            emptyList()
        }
    }

    /** A pin is a device-local UI preference, not a server mutation or an access grant. */
    fun togglePin(id: SessionHomeId): Boolean = synchronized(pinLock) {
        if (id.sessionId.length !in 1..SessionHomeIdLimit) return@synchronized false
        val current = pins(id.scope)
        val next = if (id in current) current - id else {
            if (current.size >= SessionHomePinsLimit) return@synchronized false
            current + id
        }
        prefs.edit().putString(pinKey(id.scope), JSONArray(next.map { it.sessionId }).toString()).apply()
        true
    }
}

@Composable
internal fun rememberShowSystemSessions(context: Context): Boolean {
    val store = remember(context) { SessionHomePreferences(context) }
    var visible by remember(store) { mutableStateOf(store.showSystemSessions()) }
    DisposableEffect(store) {
        val listener = SharedPreferences.OnSharedPreferenceChangeListener { _, key ->
            if (key == SessionHomeSystemKey) visible = store.showSystemSessions()
        }
        store.prefs.registerOnSharedPreferenceChangeListener(listener)
        visible = store.showSystemSessions()
        onDispose { store.prefs.unregisterOnSharedPreferenceChangeListener(listener) }
    }
    return visible
}
