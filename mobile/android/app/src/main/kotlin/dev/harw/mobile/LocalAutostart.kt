package dev.harw.mobile

import android.content.Context
import android.net.ConnectivityManager
import android.net.NetworkCapabilities
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.delay
import kotlinx.coroutines.isActive
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import uniffi.harw_mobile_ffi.ProviderKind
import uniffi.harw_mobile_ffi.ProviderStore

/**
 * Haelt die On-Device-Laufzeit ("Lokal") am Leben, solange der aktive Provider nutzbar ist:
 * ein lokales GGUF-Modell ist aktiv, oder eine Netzwerk-Route ist aktiv und das Netz steht.
 * Ohne Konfiguration wird nichts gestartet. Ein manuelles Trennen pausiert die Automatik,
 * bis der Nutzer wieder verbindet.
 */
internal class LocalAutostart(private val context: Context, private val local: HarwConnection) {
    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.Default)
    private var job: Job? = null

    @Volatile
    private var suppressed = false

    fun start() {
        if (job?.isActive == true) return
        job = scope.launch {
            while (isActive) {
                tick()
                delay(PollMillis)
            }
        }
    }

    /** Nutzer hat Lokal bewusst getrennt. */
    fun suppress() {
        suppressed = true
    }

    /** Nutzer verbindet wieder (oder Konfiguration hat sich geaendert). */
    fun resume() {
        suppressed = false
    }

    private suspend fun tick() {
        if (suppressed) return
        val state = local.state.value
        if (state.hasConnection || state.stopping || !state.canSetUp) return
        val usable = try {
            withContext(Dispatchers.IO) { providerUsable() }
        } catch (error: Exception) {
            if (error is kotlinx.coroutines.CancellationException) throw error
            false
        }
        if (usable && !suppressed) withContext(Dispatchers.Main) { local.connectLocal() }
    }

    private fun providerUsable(): Boolean {
        val routes = ProviderStore(context.filesDir.path, AndroidPlatformKeystore(context)).routes()
        val active = routes.firstOrNull { it.active } ?: return false
        return active.kind == ProviderKind.LOCAL_GGUF || networkUp()
    }

    private fun networkUp(): Boolean {
        val manager = context.getSystemService(ConnectivityManager::class.java) ?: return false
        val caps = manager.getNetworkCapabilities(manager.activeNetwork ?: return false) ?: return false
        return caps.hasCapability(NetworkCapabilities.NET_CAPABILITY_INTERNET)
    }

    private companion object {
        const val PollMillis = 10_000L
    }
}
