package dev.harw.mobile

import android.app.Application

internal class HarwApplication : Application() {
    val connection by lazy { HarwConnection(applicationContext) }
    val localConnection by lazy { HarwConnection(applicationContext, localBackend = true) }

    val localAutostart by lazy { LocalAutostart(applicationContext, localConnection) }

    override fun onCreate() {
        super.onCreate()
        // Der Codex-Provider liest $HOME/.codex/auth.json (siehe CodexDirect.kt).
        ensureCodexHome(this)
        applyLocalImageConcurrency(readLocalImageConcurrency(this))
        // Lokal startet von selbst, sobald ein Modell oder eine Netzwerk-Route nutzbar ist.
        localAutostart.start()
        AppUpdates.schedule(this)
        subscribeUpdatePush()
    }
}
