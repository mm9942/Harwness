package dev.harw.mobile

import android.app.Application

internal class HarwApplication : Application() {
    val connection by lazy { HarwConnection(applicationContext) }
}
