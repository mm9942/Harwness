package dev.harw.mobile

import android.app.Application

internal class HarwApplication : Application() {
    val connection by lazy { HarwConnection(applicationContext) }
    val localConnection by lazy { HarwConnection(applicationContext, localBackend = true) }

    override fun onCreate() {
        super.onCreate()
        // Der Codex-Provider liest $HOME/.codex/auth.json (siehe CodexDirect.kt).
        ensureCodexHome(this)
    }
}
