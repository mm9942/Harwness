package dev.harw.mobile

import android.content.Context
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.padding
import androidx.compose.material3.AssistChip
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.produceState
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import uniffi.harw_mobile_ffi.UiaProfile
import uniffi.harw_mobile_ffi.UiaStore

/** Agent (UIA) pro Chat und pro Projekt: Zuordnung, Anzeige und Auswahl. */
private fun store(context: Context) = UiaStore(context.filesDir.path)

/** Neuer Chat im Projekt: die Standard-UIA des Projekts wird dem Chat zugeordnet (falls eine gesetzt ist). */
internal fun applyProjectAgent(context: Context, projectKey: String, sessionId: String) {
    runCatching {
        val uias = store(context)
        uias.projectDefaultUia(projectKey)?.let { uias.setSessionUia(sessionId, it.id) }
    }
}

/** Name des Agenten dieses Chats (zugeordnete UIA, sonst die aktive); `null` bei Cloud-Chats. */
@Composable
internal fun rememberChatAgentName(sessionId: String, local: Boolean, version: Int): String? {
    val context = LocalContext.current
    val name by produceState<String?>(null, sessionId, local, version) {
        value = if (!local) null else withContext(Dispatchers.IO) {
            runCatching {
                val uias = store(context)
                (uias.uiaForSession(sessionId) ?: uias.listUias().firstOrNull { it.active })?.name?.takeIf { it.isNotBlank() }
            }.getOrNull()
        }
    }
    return name
}

/** Chip mit Auswahl des Agenten dieses Chats; Eintrag zum Setzen als Projekt-Standard. */
@Composable
internal fun ChatAgentChip(sessionId: String, projectKey: String?, version: Int, onChanged: () -> Unit) {
    val context = LocalContext.current
    val scope = rememberCoroutineScope()
    var open by remember { mutableStateOf(false) }
    var reload by remember { mutableIntStateOf(0) }
    val data by produceState<Triple<List<UiaProfile>, UiaProfile?, UiaProfile?>?>(null, sessionId, version, reload) {
        value = withContext(Dispatchers.IO) {
            runCatching {
                val uias = store(context)
                val all = uias.listUias()
                val current = uias.uiaForSession(sessionId) ?: all.firstOrNull { it.active }
                val projectDefault = projectKey?.takeIf { it != NoProjectKey }?.let { uias.projectDefaultUia(it) }
                Triple(all, current, projectDefault)
            }.getOrNull()
        }
    }
    val (all, current, projectDefault) = data ?: return
    if (all.size < 2 && projectDefault == null) return
    Column(Modifier.padding(top = 2.dp)) {
        AssistChip(
            onClick = { open = true },
            label = { Text("Agent: ${current?.name ?: "–"} ▾", maxLines = 1, overflow = TextOverflow.Ellipsis) },
        )
        DropdownMenu(expanded = open, onDismissRequest = { open = false }) {
            all.forEach { uia ->
                DropdownMenuItem(
                    text = { Text(if (uia.id == current?.id) "✓ ${uia.name}" else uia.name) },
                    onClick = {
                        open = false
                        scope.launch(Dispatchers.IO) {
                            runCatching { store(context).setSessionUia(sessionId, uia.id) }
                            onChanged()
                        }
                    },
                )
            }
            val project = projectKey?.takeIf { it != NoProjectKey }
            if (project != null && current != null) {
                HorizontalDivider()
                DropdownMenuItem(
                    text = { Text("${current.name} als Standard für dieses Projekt") },
                    onClick = {
                        open = false
                        scope.launch(Dispatchers.IO) {
                            runCatching { store(context).setProjectDefaultUia(project, current.id) }
                            reload++
                        }
                    },
                )
                if (projectDefault != null) {
                    DropdownMenuItem(
                        text = { Text("Projekt-Standard (${projectDefault.name}) entfernen") },
                        onClick = {
                            open = false
                            scope.launch(Dispatchers.IO) {
                                runCatching { store(context).setProjectDefaultUia(project, null) }
                                reload++
                            }
                        },
                    )
                }
            }
        }
    }
}
