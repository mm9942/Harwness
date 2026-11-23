package dev.harw.mobile

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.ui.Modifier
import androidx.compose.ui.unit.dp

/**
 * Gedächtnis, Tagebuch, Workbench und Ziele des Servers (harwness): jeder Knopf führt den Befehl aus und zeigt das
 * Ergebnis als Karte. Nutzt dieselbe Befehlsschnittstelle wie "Befehle"; der On-Device-Host kennt diese Flächen nicht.
 */
private val Shortcuts = listOf(
    "Gedächtnis" to "/memory",
    "Tagebuch (Diary)" to "/diary",
    "Workbench" to "/workbench show",
    "Notizen der Workbench" to "/workbench show --scope=project",
    "Ziele" to "/goal",
    "Plan" to "/plan",
)

@Composable
internal fun KnowledgeScreen(state: WorkspaceState, connection: HarwConnection, onBack: () -> Unit) {
    val manage = state.manage
    LaunchedEffect(state.online, state.generation) { connection.refreshManageSupport() }
    val running = manage.running
    Column(Modifier.fillMaxSize()) {
        HarwHeader(title = "Gedächtnis & Workbench", onBack = onBack) {}
        val support = manage.support
        if (support != null && !support.manage) {
            StatusMessage(NO_MANAGE_TEXT, Modifier.padding(12.dp))
            return@Column
        }
        Column(
            Modifier.fillMaxSize().verticalScroll(rememberScrollState()).padding(12.dp),
            verticalArrangement = Arrangement.spacedBy(8.dp),
        ) {
            Text(
                "Zeigt, was der Server über deine Arbeit weiß. Die Ergebnisse stammen direkt aus harwness.",
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
            Shortcuts.forEach { (label, line) ->
                OutlinedButton(
                    onClick = { connection.runCommand(null, line) },
                    enabled = state.online && running == null,
                    modifier = Modifier.fillMaxWidth(),
                ) { Text(label) }
            }
            if (running != null) CircularProgressIndicator()
            manage.commandsError?.let { StatusMessage(it, error = true) }
            manage.card?.let { card ->
                CommandResultCard(card = card, onClose = connection::clearCommandCard, modifier = Modifier.fillMaxWidth())
            }
        }
    }
}
