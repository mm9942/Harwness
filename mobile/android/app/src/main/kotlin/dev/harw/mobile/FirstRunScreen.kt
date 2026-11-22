package dev.harw.mobile

import android.content.Context
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.Button
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.unit.dp

private const val FIRST_RUN_PREFS = "harw_first_run"
private const val FIRST_RUN_KEY = "done"

internal fun readFirstRunDone(context: Context): Boolean = try {
    context.getSharedPreferences(FIRST_RUN_PREFS, Context.MODE_PRIVATE).getBoolean(FIRST_RUN_KEY, false)
} catch (_: Exception) {
    true
}

internal fun markFirstRunDone(context: Context) {
    try {
        context.getSharedPreferences(FIRST_RUN_PREFS, Context.MODE_PRIVATE).edit().putBoolean(FIRST_RUN_KEY, true).apply()
    } catch (_: Exception) {
        // Ohne Speicher erscheint die Einrichtung beim nächsten Start erneut.
    }
}

/** Erststart-Einrichtung (Kit: GuidedOnboarding): Weg zu Rechner, eigenem Provider oder lokalem Modell. */
@Composable
internal fun FirstRunScreen(
    onConnect: () -> Unit,
    onProviders: () -> Unit,
    onLocalModels: () -> Unit,
    onSkip: () -> Unit,
) {
    Surface(Modifier.fillMaxSize(), color = MaterialTheme.colorScheme.background) {
        Column(
            Modifier.fillMaxSize().verticalScroll(rememberScrollState()).padding(20.dp),
            verticalArrangement = Arrangement.spacedBy(12.dp),
        ) {
            Text("Willkommen bei harw", style = MaterialTheme.typography.headlineSmall)
            Text(
                "Wähle, wie du starten willst. Du kannst die Einrichtung jederzeit überspringen und später in den " +
                    "Einstellungen fortsetzen.",
                style = MaterialTheme.typography.bodyMedium,
            )
            Text("Mit deinem Rechner oder Server verbinden", style = MaterialTheme.typography.titleSmall)
            Text(
                "Koppelt die App mit einer laufenden harw-Instanz (QR-Code oder Profil). Dort laufen Agenten, " +
                    "Jobs, Codex und Claude.",
                style = MaterialTheme.typography.bodySmall,
            )
            Button(onClick = onConnect, modifier = Modifier.fillMaxWidth()) { Text("Verbindung einrichten") }
            Text("Eigenen Provider nutzen", style = MaterialTheme.typography.titleSmall)
            Text(
                "API-Schlüssel für OpenAI, Anthropic, OpenRouter und weitere Anbieter. Der Schlüssel wird " +
                    "verschlüsselt auf diesem Gerät gespeichert.",
                style = MaterialTheme.typography.bodySmall,
            )
            OutlinedButton(onClick = onProviders, modifier = Modifier.fillMaxWidth()) { Text("Provider einrichten") }
            Text("Lokales Modell laden", style = MaterialTheme.typography.titleSmall)
            Text(
                "GGUF-Modell herunterladen oder importieren, damit Verläufe und Modelle auf dem Gerät bleiben.",
                style = MaterialTheme.typography.bodySmall,
            )
            OutlinedButton(onClick = onLocalModels, modifier = Modifier.fillMaxWidth()) { Text("Lokale Modelle öffnen") }
            TextButton(onClick = onSkip) { Text("Überspringen") }
        }
    }
}
