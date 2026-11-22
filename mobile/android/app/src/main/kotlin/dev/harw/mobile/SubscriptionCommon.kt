package dev.harw.mobile

import android.content.ActivityNotFoundException
import android.content.Context
import android.content.Intent
import android.net.Uri
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.ExperimentalLayoutApi
import androidx.compose.foundation.layout.FlowRow
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.material3.FilterChip
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.unit.dp
import androidx.compose.foundation.layout.Column

private const val MODEL_LIMIT = 40

/** Öffnet `url` im Browser; false, wenn kein Browser vorhanden ist. */
internal fun openInBrowser(context: Context, url: String): Boolean = try {
    context.startActivity(
        Intent(Intent.ACTION_VIEW, Uri.parse(url))
            .addCategory(Intent.CATEGORY_BROWSABLE)
            .addFlags(Intent.FLAG_ACTIVITY_NEW_TASK),
    )
    true
} catch (_: ActivityNotFoundException) {
    false
}

/** Modell-Suche und -Auswahl der Abo-Karten (Codex, Claude): Chips plus freie Modell-ID. */
@OptIn(ExperimentalLayoutApi::class)
@Composable
internal fun SubscriptionModelPicker(
    models: List<String>,
    query: String,
    onQueryChange: (String) -> Unit,
    activeModel: String?,
    enabled: Boolean,
    onPick: (String) -> Unit,
) {
    Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
        OutlinedTextField(
            value = query,
            onValueChange = { onQueryChange(it.trim().take(256)) },
            label = { Text(if (models.isEmpty()) "Modell-ID" else "Modelle suchen oder Modell-ID eintragen") },
            singleLine = true,
            modifier = Modifier.fillMaxWidth(),
        )
        val shown = models.filter { it.contains(query, ignoreCase = true) }
        if (models.isNotEmpty()) {
            Text("${shown.size} von ${models.size} Modellen", style = MaterialTheme.typography.labelMedium)
        }
        FlowRow(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
            shown.take(MODEL_LIMIT).forEach { id ->
                FilterChip(selected = activeModel == id, enabled = enabled, onClick = { onPick(id) }, label = { Text(id) })
            }
        }
        if (query.isNotEmpty() && query !in models) {
            OutlinedButton(enabled = enabled, onClick = { onPick(query) }) { Text("Modell „$query“ verwenden") }
        }
    }
}
