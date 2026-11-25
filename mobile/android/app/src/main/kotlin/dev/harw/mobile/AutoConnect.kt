package dev.harw.mobile

import android.content.Context

private const val PREF_AUTO_CONNECT = "auto_connect"
private const val PREF_MANUAL_DISCONNECT = "manual_disconnect"

/** Automatisch verbinden: Standard an; wer selbst "Trennen" drückt, wird bis zum nächsten Verbinden in Ruhe gelassen. */
internal fun readAutoConnect(context: Context): Boolean =
    context.getSharedPreferences(UI_PREFS, Context.MODE_PRIVATE).getBoolean(PREF_AUTO_CONNECT, true)

internal fun writeAutoConnect(context: Context, value: Boolean) {
    context.getSharedPreferences(UI_PREFS, Context.MODE_PRIVATE).edit().putBoolean(PREF_AUTO_CONNECT, value).apply()
}

internal fun readManualDisconnect(context: Context): Boolean =
    context.getSharedPreferences(UI_PREFS, Context.MODE_PRIVATE).getBoolean(PREF_MANUAL_DISCONNECT, false)

internal fun writeManualDisconnect(context: Context, value: Boolean) {
    context.getSharedPreferences(UI_PREFS, Context.MODE_PRIVATE).edit().putBoolean(PREF_MANUAL_DISCONNECT, value).apply()
}
