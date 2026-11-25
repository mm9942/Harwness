package dev.harw.mobile

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.BoxWithConstraints
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.RowScope
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.navigationBarsPadding
import androidx.compose.foundation.layout.padding
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.outlined.Add
import androidx.compose.material.icons.outlined.Bookmarks
import androidx.compose.material.icons.outlined.SmartToy
import androidx.compose.material.icons.outlined.Settings
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Button
import androidx.compose.material3.ButtonDefaults
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.ModalBottomSheet
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.SegmentedButton
import androidx.compose.material3.SegmentedButtonDefaults
import androidx.compose.material3.SingleChoiceSegmentedButtonRow
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.unit.dp
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.imePadding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.AssistChip
import androidx.compose.material3.rememberModalBottomSheetState
import androidx.compose.runtime.produceState
import androidx.compose.ui.Alignment
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.text.style.TextOverflow
import java.text.Normalizer
import uniffi.harw_mobile_ffi.MobileProject

internal const val NoProjectsSupportText =
    "Dieser Host kennt keine Projekte. Die Session bleibt ohne Projekt, der Titel wird aus der Nachricht abgeleitet."

internal const val ServerCreatesNoProjectsText =
    "Der Server legt Projekte nicht selbst an: Ein Projekt entsteht aus einem vorhandenen Ordner mit " +
        "Projektwurzel auf dem Server. Die Session bleibt ohne Projekt."

/** Ordner-/Projektname aus freiem Text: Kleinbuchstaben, Umlaute aufgeloest, Rest als `-`, max. 40 Zeichen. */
internal fun slugify(text: String): String {
    val base = text.trim().lowercase()
        .replace("ä", "ae").replace("ö", "oe").replace("ü", "ue").replace("ß", "ss")
    val plain = Normalizer.normalize(base, Normalizer.Form.NFD).replace(Regex("\\p{M}+"), "")
    return plain.replace(Regex("[^a-z0-9]+"), "-").trim('-').take(40).trim('-')
}

/** Umschalter "Lokal | Cloud" am Kopf der Startliste. */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
internal fun BackendSwitch(local: Boolean, enabled: Boolean, onSelect: (Boolean) -> Unit, modifier: Modifier = Modifier) {
    SingleChoiceSegmentedButtonRow(modifier.fillMaxWidth().padding(horizontal = 12.dp, vertical = 4.dp)) {
        SegmentedButton(
            selected = local,
            onClick = { onSelect(true) },
            enabled = enabled,
            shape = SegmentedButtonDefaults.itemShape(0, 2),
        ) { Text(stringResource(R.string.directory_local)) }
        SegmentedButton(
            selected = !local,
            onClick = { onSelect(false) },
            enabled = enabled,
            shape = SegmentedButtonDefaults.itemShape(1, 2),
        ) { Text(stringResource(R.string.directory_cloud)) }
    }
}

/**
 * Float shortcuts over the full-height directory and its project subviews.
 */
@Composable
internal fun HomeShortcutLayout(
    shortcuts: List<HomeShortcut>,
    onSettings: () -> Unit,
    onNewSession: () -> Unit,
    onAssistant: () -> Unit = {},
    onCloudAssistant: () -> Unit = {},
    onLibrary: () -> Unit = {},
    onCloudLibrary: () -> Unit = {},
    overlayShortcuts: Boolean = true,
    content: @Composable (Modifier) -> Unit,
) {
    if (overlayShortcuts) {
        Box(Modifier.fillMaxSize()) {
            content(Modifier.fillMaxSize())
            if (shortcuts.isNotEmpty()) {
                HomeShortcutStack(
                    shortcuts, onSettings, onNewSession, onAssistant, onCloudAssistant,
                    onLibrary, onCloudLibrary, Modifier.align(Alignment.BottomEnd).padding(end = 12.dp),
                )
            }
        }
        return
    }
    BoxWithConstraints(Modifier.fillMaxSize()) {
        val useSideRail = maxWidth >= 600.dp || (maxWidth >= 440.dp && maxHeight < 440.dp)
        if (shortcuts.isEmpty()) {
            content(Modifier.fillMaxSize())
        } else if (useSideRail) {
            Row(Modifier.fillMaxSize(), verticalAlignment = Alignment.Bottom) {
                content(Modifier.weight(1f))
                HomeShortcutStack(shortcuts, onSettings, onNewSession, onAssistant, onCloudAssistant, onLibrary, onCloudLibrary, Modifier.padding(end = 12.dp, bottom = 8.dp))
            }
        } else {
            Column(Modifier.fillMaxSize()) {
                content(Modifier.weight(1f))
                HomeShortcutStack(shortcuts, onSettings, onNewSession, onAssistant, onCloudAssistant, onLibrary, onCloudLibrary, Modifier.align(Alignment.End).padding(end = 12.dp, bottom = 8.dp))
            }
        }
    }
}

@Composable
private fun HomeShortcutStack(
    shortcuts: List<HomeShortcut>,
    onSettings: () -> Unit,
    onNewSession: () -> Unit,
    onAssistant: () -> Unit,
    onCloudAssistant: () -> Unit,
    onLibrary: () -> Unit,
    onCloudLibrary: () -> Unit,
    modifier: Modifier,
) {
    Column(modifier, horizontalAlignment = Alignment.End) {
        for (shortcut in shortcuts) {
            when (shortcut.action) {
                HomeAction.Assistant -> HomeAccentShortcut(onClick = onAssistant, enabled = shortcut.enabled) {
                    HomeShortcutLabel(Icons.Outlined.SmartToy, stringResource(R.string.directory_assistant), if (shortcut.enabled) stringResource(R.string.directory_local_only) else stringResource(R.string.directory_unavailable))
                }
                HomeAction.CloudAssistant -> HomeAccentShortcut(onClick = onCloudAssistant, enabled = shortcut.enabled) {
                    HomeShortcutLabel(Icons.Outlined.SmartToy, stringResource(R.string.directory_cloud_assistant), cloudShortcutDetail(shortcut, R.string.directory_on_server))
                }
                HomeAction.Settings -> HomeAccentShortcut(onClick = onSettings, enabled = shortcut.enabled) {
                    HomeShortcutLabel(Icons.Outlined.Settings, stringResource(R.string.directory_settings))
                }
                HomeAction.Library -> HomeAccentShortcut(onClick = onLibrary, enabled = shortcut.enabled) {
                    HomeShortcutLabel(Icons.Outlined.Bookmarks, stringResource(R.string.directory_library), if (shortcut.enabled) stringResource(R.string.directory_local_encrypted) else stringResource(R.string.directory_local_profile_only))
                }
                HomeAction.CloudLibrary -> HomeAccentShortcut(onClick = onCloudLibrary, enabled = shortcut.enabled) {
                    HomeShortcutLabel(Icons.Outlined.Bookmarks, stringResource(R.string.directory_cloud_library), cloudShortcutDetail(shortcut, R.string.directory_on_gateway))
                }
                HomeAction.NewSession -> HomeAccentShortcut(onClick = onNewSession, enabled = shortcut.enabled) {
                    HomeShortcutLabel(Icons.Outlined.Add, stringResource(R.string.directory_new_session))
                }
            }
        }
    }
}

@Composable
private fun HomeAccentShortcut(
    onClick: () -> Unit,
    enabled: Boolean,
    content: @Composable RowScope.() -> Unit,
) {
    val colors = MaterialTheme.colorScheme
    Button(
        onClick = onClick,
        enabled = enabled,
        colors = ButtonDefaults.buttonColors(
            containerColor = colors.harwAccentButtonContainer(true),
            contentColor = colors.harwAccentButtonContent(true),
            disabledContainerColor = colors.harwAccentButtonContainer(false),
            disabledContentColor = colors.harwAccentButtonContent(false),
        ),
        content = content,
    )
}

@Composable
private fun cloudShortcutDetail(shortcut: HomeShortcut, ready: Int): String = stringResource(
    when (shortcut.cloudState) {
        CloudShortcutState.Disconnected -> R.string.directory_gateway_disconnected
        CloudShortcutState.Unsupported -> R.string.directory_gateway_feature_unavailable
        CloudShortcutState.Available, null -> ready
    },
)

@Composable
private fun HomeShortcutLabel(
    icon: androidx.compose.ui.graphics.vector.ImageVector,
    label: String,
    detail: String? = null,
) {
    Row(
        horizontalArrangement = Arrangement.spacedBy(8.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Icon(icon, contentDescription = null)
        Column {
            Text(label)
            detail?.let { Text(it, style = MaterialTheme.typography.labelSmall) }
        }
    }
}

/** Entwurf eines noch nicht gesendeten neuen Chats; die Session entsteht erst mit der ersten Nachricht. */
internal data class NewChatDraft(
    val local: Boolean,
    /** `null` = neues Projekt zum Thema der ersten Nachricht. */
    val projectKey: String? = null,
    /** Selected route for this draft only; null inherits the host/profile default. */
    val model: String? = null,
    val uiaId: String? = null,
    val text: String = "",
    val images: List<DraftImage> = emptyList(),
    val error: String? = null,
)

/**
 * Neuer Chat: die normale Unterhaltungsansicht als Entwurf. Oben Lokal|Cloud und Modell, in der Mitte die
 * Projektwahl, unten der gewohnte Eingabebereich. Die Session wird erst beim ersten Senden angelegt.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
internal fun NewChatScreen(
    draft: NewChatDraft,
    targetState: WorkspaceState,
    cloudState: WorkspaceState,
    connection: HarwConnection,
    onBack: () -> Unit,
    onSwitch: (Boolean) -> Unit,
    onProject: (String?) -> Unit,
    onModel: (String?) -> Unit,
    onUia: (String?) -> Unit,
    onEdit: (String) -> Unit,
    onAttach: (DraftImage) -> Unit,
    onDetach: (Int) -> Unit,
    onImageError: (String) -> Unit,
    onSend: () -> Unit,
) {
    var menu by rememberSaveable { mutableStateOf(false) }
    var modelsOpen by rememberSaveable { mutableStateOf(false) }
    var agentsOpen by rememberSaveable { mutableStateOf(false) }
    val context = androidx.compose.ui.platform.LocalContext.current
    val busy = targetState.creating
    val localLabel by rememberLocalModelLabel(0, pendingSelection = draft.model.takeIf { draft.local })
    val agents by produceState<Pair<List<uniffi.harw_mobile_ffi.UiaProfile>, String?>?>(null, draft.local, draft.projectKey, targetState.generation) {
        value = if (draft.local) kotlinx.coroutines.withContext(kotlinx.coroutines.Dispatchers.IO) {
            runCatching {
                val store = uniffi.harw_mobile_ffi.UiaStore(context.filesDir.path)
                val profiles = store.listUias()
                val inherited = draft.projectKey?.takeIf { it != NoProjectKey }
                    ?.let { store.projectDefaultUia(it)?.id } ?: profiles.firstOrNull { it.active }?.id
                profiles to inherited
            }.getOrNull()
        } else null
    }
    val real = targetState.projects.filter { it.projectKey != NoProjectKey }
    val choice = draft.projectKey
    val topicMode = choice == null
    val imagesSupported by produceState(false, targetState.online, targetState.generation, draft.local) {
        value = targetState.online && connection.supportsImages()
    }
    val manage = cloudState.manage
    Column(Modifier.fillMaxSize().imePadding()) {
        HarwHeader(title = stringResource(R.string.directory_new_chat), onBack = onBack) { ConnectionBadge(targetState) }
        BackendSwitch(local = draft.local, enabled = !busy, onSelect = onSwitch)
        Column(Modifier.fillMaxWidth().padding(horizontal = 15.dp, vertical = 2.dp)) {
            if (draft.local) {
                AssistChip(
                    onClick = { modelsOpen = true },
                    enabled = !busy,
                    label = { Text(localLabel, maxLines = 1, overflow = TextOverflow.Ellipsis) },
                )
                val selectedAgent = agents?.first?.firstOrNull { it.id == (draft.uiaId ?: agents?.second) }
                Box {
                    AssistChip(
                        onClick = { agentsOpen = true },
                        enabled = !busy && agents?.first?.isNotEmpty() == true,
                        label = { Text(selectedAgent?.name?.let { "UIA · $it" } ?: "UIA") },
                    )
                    DropdownMenu(expanded = agentsOpen && draft.local, onDismissRequest = { agentsOpen = false }) {
                        DropdownMenuItem(text = { Text("Projekt-/Profil-Standard") }, onClick = {
                            onUia(null)
                            agentsOpen = false
                        })
                        agents?.first.orEmpty().forEach { agent ->
                            DropdownMenuItem(text = { Text(agent.name) }, onClick = {
                                onUia(agent.id)
                                agentsOpen = false
                            })
                        }
                    }
                }
            } else {
                val block = modelChipBlock(manage.support, true)
                AssistChip(
                    onClick = { modelsOpen = true },
                    enabled = block == null && !busy,
                    label = {
                        Text(
                            draft.model ?: modelChipLabel(null, manage.catalog?.liveModel, manage.catalog?.liveProvider),
                            maxLines = 1, overflow = TextOverflow.Ellipsis,
                        )
                    },
                )
                block?.let {
                    Text(it, style = MaterialTheme.typography.labelSmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
                }
            }
        }
        if (modelsOpen) {
            ModalBottomSheet(
                onDismissRequest = { modelsOpen = false },
                sheetState = rememberModalBottomSheetState(skipPartiallyExpanded = true),
            ) {
                if (draft.local) {
                    LocalModelSheet(
                        onChanged = {},
                        canChange = { !connection.state.value.creating && connection.state.value.local },
                        pendingSelection = draft.model,
                        onPick = { onModel(it); modelsOpen = false },
                    )
                } else {
                    ModelSwitchSheet(
                        cloudState, connection, "", true,
                        pending = draft.model,
                        onPick = { onModel(it); modelsOpen = false },
                    )
                }
            }
        }
        Column(
            Modifier.weight(1f).fillMaxWidth().verticalScroll(rememberScrollState()).padding(horizontal = 24.dp, vertical = 12.dp),
            horizontalAlignment = Alignment.CenterHorizontally,
            verticalArrangement = Arrangement.spacedBy(10.dp, Alignment.CenterVertically),
        ) {
            Text(stringResource(R.string.directory_project), style = MaterialTheme.typography.labelLarge)
            Box {
                OutlinedButton(onClick = { menu = true }, enabled = !busy) {
                    Text(real.firstOrNull { it.projectKey == choice }?.displayLabel() ?: stringResource(R.string.directory_topic_project))
                }
                DropdownMenu(expanded = menu, onDismissRequest = { menu = false }) {
                    DropdownMenuItem(
                        text = { Text(stringResource(R.string.directory_topic_project)) },
                        onClick = { onProject(null); menu = false },
                    )
                    for (project in real) {
                        DropdownMenuItem(
                            text = { Text(project.displayLabel()) },
                            onClick = { onProject(project.projectKey); menu = false },
                        )
                    }
                }
            }
            Text(
                if (topicMode) {
                    stringResource(R.string.directory_topic_explain)
                } else {
                    stringResource(R.string.directory_chosen_explain)
                },
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                textAlign = TextAlign.Center,
            )
            if (targetState.projectsLoaded && !targetState.projectsSupported) {
                Text(
                    stringResource(R.string.directory_projects_unsupported),
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                    textAlign = TextAlign.Center,
                )
            }
            if (!targetState.online) {
                StatusMessage(if (draft.local) stringResource(R.string.directory_local_offline) else stringResource(R.string.directory_cloud_offline), error = true)
            }
        }
        PromptComposer(
            draft = SessionDraft(text = draft.text, images = draft.images, error = draft.error, sending = busy),
            enabled = !busy,
            canSend = targetState.online && targetState.canCreate && !busy,
            busy = busy,
            hint = stringResource(R.string.directory_first_message),
            onEdit = onEdit,
            onReview = {},
            onSend = onSend,
            onDiscard = {},
            onAttachImage = if (imagesSupported) onAttach else null,
            onDetachImage = onDetach,
            onImageError = onImageError,
        )
    }
}

/** Dialog stringResource(R.string.directory_new_project): fragt einen Namen, legt ihn an oder erklaert, warum der Server das nicht kann. */
@Composable
internal fun NewProjectDialog(
    busy: Boolean,
    onDismiss: () -> Unit,
    onCreate: (String, (String?) -> Unit) -> Unit,
) {
    var name by rememberSaveable { mutableStateOf("") }
    var error by rememberSaveable { mutableStateOf<String?>(null) }
    var working by rememberSaveable { mutableStateOf(false) }
    AlertDialog(
        onDismissRequest = onDismiss,
        title = { Text(stringResource(R.string.directory_new_project)) },
        text = {
            Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
                OutlinedTextField(
                    value = name,
                    onValueChange = { name = it; error = null },
                    singleLine = true,
                    label = { Text(stringResource(R.string.directory_project_name)) },
                    supportingText = { Text(stringResource(R.string.directory_project_key, slugify(name).ifBlank { "–" })) },
                    modifier = Modifier.fillMaxWidth(),
                )
                error?.let { Text(it, color = MaterialTheme.colorScheme.error, style = MaterialTheme.typography.bodySmall) }
            }
        },
        dismissButton = { TextButton(onClick = onDismiss) { Text(stringResource(R.string.directory_cancel)) } },
        confirmButton = {
            Button(
                enabled = !busy && !working && slugify(name).isNotBlank(),
                onClick = {
                    working = true
                    onCreate(name) { failure ->
                        working = false
                        if (failure == null) onDismiss() else error = failure
                    }
                },
            ) { Text(stringResource(R.string.directory_create)) }
        },
    )
}
