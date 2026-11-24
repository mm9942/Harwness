package dev.harw.mobile

import android.app.LocaleManager
import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.content.IntentFilter
import android.content.SharedPreferences
import android.content.res.Configuration
import android.content.res.Resources
import android.os.Build
import android.os.LocaleList
import androidx.activity.ComponentActivity
import androidx.core.content.ContextCompat
import java.util.Locale

private const val PrefUiLanguage = "app_ui_language"

/** An installation-local display choice, never a system, chat, prompt, or model locale. */
internal enum class HarwUiLanguage(val code: String, val autonym: String) {
    English("en", "English"),
    German("de", "Deutsch"),
    French("fr", "Français"),
    Spanish("es", "Español");

    val resourceLocales: List<Locale>
        get() = when (this) {
            English -> listOf(Locale.ENGLISH)
            else -> listOf(Locale.forLanguageTag(code), Locale.ENGLISH)
        }
}

internal sealed interface HarwUiLanguagePreference {
    data object FollowDevice : HarwUiLanguagePreference
    data class Explicit(val language: HarwUiLanguage) : HarwUiLanguagePreference

    fun resolve(devicePrimary: Locale?): HarwUiLanguage = when (this) {
        FollowDevice -> HarwUiLanguage.entries.firstOrNull { it.code == devicePrimary?.language }
            ?: HarwUiLanguage.English
        is Explicit -> language
    }

    companion object {
        fun restored(value: Any?): HarwUiLanguagePreference =
            HarwUiLanguage.entries.firstOrNull { it.code == value }?.let(::Explicit) ?: FollowDevice
    }
}

internal fun harwUiLanguagePreference(context: Context): HarwUiLanguagePreference =
    HarwUiLanguagePreference.restored(
        context.getSharedPreferences(UI_PREFS, Context.MODE_PRIVATE).all[PrefUiLanguage],
    )

internal fun setHarwUiLanguagePreference(context: Context, choice: HarwUiLanguagePreference) {
    val prefs = context.getSharedPreferences(UI_PREFS, Context.MODE_PRIVATE)
    when (choice) {
        HarwUiLanguagePreference.FollowDevice -> if (prefs.contains(PrefUiLanguage)) {
            prefs.edit().remove(PrefUiLanguage).apply()
        }
        is HarwUiLanguagePreference.Explicit -> if (prefs.all[PrefUiLanguage] != choice.language.code) {
            // Persist explicitly even if the current device language happens to be identical.
            prefs.edit().putString(PrefUiLanguage, choice.language.code).apply()
        }
    }
}

internal fun effectiveHarwUiLanguage(context: Context): HarwUiLanguage {
    val system = if (Build.VERSION.SDK_INT >= 33) {
        context.getSystemService(LocaleManager::class.java)?.systemLocales
    } else null
    // Never read context.resources here: that may already carry this Activity's override.
    val locales = system ?: Resources.getSystem().configuration.locales
    val devicePrimary = if (locales.isEmpty) null else locales[0]
    return harwUiLanguagePreference(context).resolve(devicePrimary)
}

/** Shared by the main and additional session windows, including on Android 9–12. */
internal abstract class HarwLocaleActivity : ComponentActivity() {
    private var attachedLanguage = HarwUiLanguage.English
    private var changingLanguage = false
    private val languageListener = SharedPreferences.OnSharedPreferenceChangeListener { _, key ->
        if (key == null || key == PrefUiLanguage) applyLanguageChange()
    }
    private val deviceLanguageListener = object : BroadcastReceiver() {
        override fun onReceive(context: Context, intent: Intent) {
            if (intent.action == Intent.ACTION_LOCALE_CHANGED) applyLanguageChange()
        }
    }

    override fun attachBaseContext(newBase: Context) {
        attachedLanguage = effectiveHarwUiLanguage(newBase)
        // A delta, not a copy of the current device configuration: do not lock font
        // scale, night mode, density, or other non-language configuration.
        val displayLanguage = Configuration().apply {
            fontScale = 0f
            setLocales(LocaleList(*attachedLanguage.resourceLocales.toTypedArray()))
        }
        super.attachBaseContext(newBase.createConfigurationContext(displayLanguage))
    }

    override fun onStart() {
        super.onStart()
        getSharedPreferences(UI_PREFS, MODE_PRIVATE).registerOnSharedPreferenceChangeListener(languageListener)
        ContextCompat.registerReceiver(
            this, deviceLanguageListener, IntentFilter(Intent.ACTION_LOCALE_CHANGED),
            ContextCompat.RECEIVER_NOT_EXPORTED,
        )
        // A stopped second window was not listening while the choice changed.
        applyLanguageChange()
    }

    override fun onStop() {
        getSharedPreferences(UI_PREFS, MODE_PRIVATE).unregisterOnSharedPreferenceChangeListener(languageListener)
        unregisterReceiver(deviceLanguageListener)
        super.onStop()
    }

    private fun applyLanguageChange() {
        if (!changingLanguage && !isFinishing && !isDestroyed && attachedLanguage != effectiveHarwUiLanguage(this)) {
            changingLanguage = true
            recreate()
        }
    }
}
