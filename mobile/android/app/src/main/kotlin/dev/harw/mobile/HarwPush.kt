package dev.harw.mobile

import com.google.firebase.messaging.FirebaseMessaging
import com.google.firebase.messaging.FirebaseMessagingService
import com.google.firebase.messaging.RemoteMessage

private const val UpdateTopic = "updates"

/** Abonniert den Update-Kanal; ohne google-services.json oder Play-Dienste bleibt es still bei der Hintergrundprüfung. */
internal fun subscribeUpdatePush() {
    try {
        FirebaseMessaging.getInstance().subscribeToTopic(UpdateTopic)
    } catch (_: Throwable) {
    }
}

/**
 * Push trägt nur den Hinweis "neue Version" (data: type=update); die Details holt die App selbst vom eigenen
 * Update-Dienst, damit nichts Inhaltliches über Google läuft.
 */
internal class HarwPushService : FirebaseMessagingService() {
    override fun onMessageReceived(message: RemoteMessage) {
        if (message.data["type"] != "update") return
        Thread {
            val info = AppUpdates.fetchLatest() ?: return@Thread
            if (info.versionCode > AppUpdates.installedCode(this)) AppUpdates.notify(this, info)
        }.start()
    }

    override fun onNewToken(token: String) {
        subscribeUpdatePush()
    }
}
