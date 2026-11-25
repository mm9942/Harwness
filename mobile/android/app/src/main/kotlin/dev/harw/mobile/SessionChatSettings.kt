package dev.harw.mobile

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.outlined.Tune
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.AssistChip
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.ModalBottomSheet
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.rememberModalBottomSheetState
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.SideEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.key
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.semantics.stateDescription
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import uniffi.harw_mobile_ffi.MobileSession

private enum class ChatSettingsPage { Overview, Agents, Models }

internal data class ChatQuickChoice(
    val name: String? = null,
    val enabled: Boolean = false,
    val whyDisabled: String? = null,
)

/** Only presentation state and callbacks. Both buttons use the existing picker pages. */
internal class ChatQuickPickers {
    var agent by mutableStateOf(ChatQuickChoice())
        private set
    var model by mutableStateOf(ChatQuickChoice())
        private set
    private var showAgent: () -> Unit = {}
    private var showModel: () -> Unit = {}

    fun publish(agent: ChatQuickChoice, model: ChatQuickChoice, onAgent: () -> Unit, onModel: () -> Unit) {
        showAgent = onAgent
        showModel = onModel
        this.agent = agent
        this.model = model
    }

    var openSettings: (() -> Unit)? by mutableStateOf(null)

    fun openAgent() { if (agent.enabled) showAgent() }
    fun openModel() { if (model.enabled) showModel() }
}

@Composable
internal fun ChatQuickPickerRow(pickers: ChatQuickPickers, local: Boolean) {
    Box(Modifier.fillMaxWidth()) {
        Row(
            Modifier.fillMaxWidth(),
            horizontalArrangement = Arrangement.spacedBy(6.dp),
        ) {
            if (local) ChatQuickPickerButton("UIA", pickers.agent, pickers::openAgent, Modifier.weight(1f))
            ChatQuickPickerButton(stringResource(R.string.chat_quick_picker_model),
                pickers.model, pickers::openModel, Modifier.weight(1f))
            val open = pickers.openSettings
            ChatQuickPickerButton(
                stringResource(R.string.chat_quick_picker_settings), ChatQuickChoice(enabled = open != null),
                { open?.invoke() }, Modifier.weight(1f),
            )

        }
    }
}

@Composable
private fun ChatQuickPickerButton(
    title: String,
    choice: ChatQuickChoice,
    onClick: () -> Unit,
    modifier: Modifier,
) {
    val label = choice.name?.takeIf { it.isNotBlank() }?.let { "$title · $it" } ?: title
    val colors = MaterialTheme.colorScheme
    Surface(
        color = colors.harwAccentButtonContainer(choice.enabled),
        contentColor = colors.harwAccentButtonContent(choice.enabled),
        shape = RoundedCornerShape(9.dp),
        modifier = modifier.heightIn(min = 34.dp)
            .clickable(enabled = choice.enabled, role = Role.Button, onClick = onClick)
            .semantics(mergeDescendants = true) {
                if (!choice.enabled && choice.whyDisabled != null) stateDescription = choice.whyDisabled
            },
    ) {
        Box(Modifier.padding(horizontal = 8.dp, vertical = 5.dp), contentAlignment = Alignment.CenterStart) {
            Text(label, style = MaterialTheme.typography.labelMedium, maxLines = 1,
                softWrap = false, overflow = TextOverflow.Ellipsis)
        }
    }
}

/** Presentation-only bridge; the conversation still performs every exact archive check. */
internal class ChatArchiveMenuEntry {
    val quickPickers = ChatQuickPickers()
    var available by mutableStateOf(false)
        private set
    var enabled by mutableStateOf(false)
        private set
    private var action: (() -> Unit)? = null

    fun publish(action: (() -> Unit)?, opening: Boolean = false) {
        this.action = action
        available = action != null
        enabled = available && !opening
    }

    fun open() {
        if (enabled) action?.invoke()
    }
}

@Composable
internal fun SessionChatSettings(
    state: WorkspaceState,
    window: HarwWindowModel,
    sessionId: String,
    session: MobileSession?,
    connection: HarwConnection,
    agentVersion: Int,
    archiveMenu: ChatArchiveMenuEntry,
    onAgentChanged: () -> Unit,
) {
    key(connection, sessionId, sessionHomeScope(state), state.generation) {
        ScopedChatSettings(state, window, sessionId, session, connection, agentVersion, archiveMenu, onAgentChanged)
    }
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun ScopedChatSettings(
    state: WorkspaceState,
    window: HarwWindowModel,
    sessionId: String,
    session: MobileSession?,
    connection: HarwConnection,
    agentVersion: Int,
    archiveMenu: ChatArchiveMenuEntry,
    onAgentChanged: () -> Unit,
) {
    val ctx = LocalContext.current
    val chatWindow = state.windows[sessionId]
    val generation = chatWindow?.generation?.takeIf { state.online && it == state.generation }
    val active = generation != null && chatWindow?.snapshot?.online == true
    val ownerStatusTarget = remember(
        connection, sessionId, state.profile, state.deviceNodeId, state.activeRoute, state.sessionResetEpoch, generation, active,
    ) { if (active) connection.ownerStatusTarget(sessionId) else null }
    val chatVisionScope = sessionHomeScope(state)
    val chatVisionOverrides = remember(ctx) { ChatVisionOverrides(ctx) }
    val chatVisionKey = chatVisionOverrides.key(chatVisionScope, sessionId)
    val chatVisionOverride = rememberChatVisionOverride(chatVisionOverrides, chatVisionKey)
    var visionPolicyOpen by remember(connection, chatVisionKey, state.generation) { mutableStateOf(false) }
    var visionDialog by remember(connection, sessionId, chatVisionScope, state.generation) {
        mutableStateOf<VisionDescriptionMode?>(null)
    }
    var requestedVisionFallback by remember(connection, sessionId, chatVisionScope, state.generation) {
        mutableStateOf<ConfiguredVisionFallbackInvite?>(null)
    }
    val openManualVisionApi: () -> Unit = {
        requestedVisionFallback = null
        visionDialog = VisionDescriptionMode.Api
    }
    val openConfiguredVisionApi: (ConfiguredVisionFallbackInvite) -> Unit = { invite ->
        requestedVisionFallback = invite
        visionDialog = VisionDescriptionMode.Api
    }
    var apiImageEditOpen by remember(connection, sessionId, chatVisionScope, state.generation) {
        mutableStateOf(false)
    }
    val isCurrentImageEditChat = {
        val latest = connection.state.value
        val latestWindow = latest.windows[sessionId]
        active && chatVisionScope != null && generation != null && window.sessionId == sessionId &&
            latest.online && latest.generation == generation && sessionHomeScope(latest) == chatVisionScope &&
            latestWindow?.generation == generation && latestWindow?.snapshot?.online == true
    }
    LaunchedEffect(active) {
        if (!active) apiImageEditOpen = false
    }
    val globalVisionMode = rememberVisionDescriptionMode(ctx)
    val visionMode = chatVisionOverride ?: globalVisionMode
    val deviceVision = remember(ctx) { (ctx.applicationContext as? HarwApplication)?.localConnection }
    val deviceVisionState = deviceVision?.state?.collectAsState()?.value
    val localVisionReady = deviceVisionState?.online == true
    var page by remember { mutableStateOf<ChatSettingsPage?>(null) }
    val openOverview = remember { { page = ChatSettingsPage.Overview } }
    var localModelRefresh by remember { mutableIntStateOf(0) }
    val pickerProject = window.projectKey
    val currentPickerTarget = {
        val latest = connection.state.value
        window.sessionId == sessionId && window.projectKey == pickerProject &&
            latest.local == state.local && !latest.stopping &&
            latest.generation == state.generation && latest.profile == state.profile &&
            latest.deviceNodeId == state.deviceNodeId && sessionHomeScope(latest) == chatVisionScope
    }
    val localAgent = if (state.local) {
        rememberChatAgentControl(sessionId, window.projectKey, agentVersion, currentPickerTarget, onAgentChanged)
    } else null
    val localModelLabel = if (state.local) rememberLocalModelLabel(localModelRefresh, sessionId = sessionId).value else null
    val sessionCatalog = state.manage.catalog?.takeIf { state.manage.catalogSessionId == sessionId }
    val cloudModelLabel = modelChipLabel(session?.model, sessionCatalog?.liveModel, sessionCatalog?.liveProvider)
    val modelBlock = if (state.local) null else modelChipBlock(state.manage.support, active)
    val openAgents = {
        if (currentPickerTarget() && localAgent?.canChoose == true) page = ChatSettingsPage.Agents
    }
    val canChangeModel = {
        val latest = connection.state.value
        val view = latest.windows[sessionId]
        val live = latest.online && latest.generation != null && view?.generation == latest.generation &&
            view?.snapshot?.online == true
        currentPickerTarget() && (latest.local || modelChipBlock(latest.manage.support, live) == null)
    }
    val openModels = { if (canChangeModel()) page = ChatSettingsPage.Models }
    val blockedAgent = stringResource(
        if (state.local) R.string.chat_quick_picker_uia_unavailable else R.string.chat_quick_picker_uia_local_only,
    )
    val blockedModel = stringResource(R.string.chat_quick_picker_model_unavailable)
    SideEffect {
        archiveMenu.quickPickers.openSettings = openOverview
        archiveMenu.quickPickers.publish(
            ChatQuickChoice(localAgent?.selectedName, localAgent?.canChoose == true, blockedAgent),
            ChatQuickChoice((localModelLabel ?: cloudModelLabel).takeUnless { it == "Modell" },
                modelBlock == null, modelBlock ?: blockedModel),
            openAgents, openModels,
        )
    }
    DisposableEffect(archiveMenu.quickPickers) {
        onDispose {
            archiveMenu.quickPickers.openSettings = null
            archiveMenu.quickPickers.publish(ChatQuickChoice(), ChatQuickChoice(), {}, {})
        }
    }
    // Keep launcher and download completion alive when the settings sheet is hidden.
    val localVisionHint = if (state.local) {
        rememberLocalVisionHint(window.draft(sessionId).images.isNotEmpty(), localModelRefresh, sessionId = sessionId) { localModelRefresh++ }
    } else null

    IconButton(onClick = { page = ChatSettingsPage.Overview }) {
        Icon(Icons.Outlined.Tune, contentDescription = "Chat-Einstellungen")
    }
    if (page != null) {
        ModalBottomSheet(
            onDismissRequest = { page = null },
            sheetState = rememberModalBottomSheetState(skipPartiallyExpanded = true),
        ) {
            if (page == ChatSettingsPage.Models) {
                Row(
                    Modifier.fillMaxWidth().padding(horizontal = 15.dp),
                    horizontalArrangement = Arrangement.SpaceBetween,
                    verticalAlignment = Alignment.CenterVertically,
                ) {
                    Text("Chat-Einstellungen", style = MaterialTheme.typography.titleMedium)
                    TextButton(onClick = { page = ChatSettingsPage.Overview }) { Text("Übersicht") }
                }
                if (state.local) LocalModelSheet(onChanged = { localModelRefresh++ }, canChange = canChangeModel, sessionId = sessionId)
                else ModelSwitchSheet(state, connection, sessionId, active, canChange = canChangeModel)
            } else if (page == ChatSettingsPage.Agents) {
                Column(Modifier.fillMaxWidth().verticalScroll(rememberScrollState()).padding(horizontal = 15.dp)) {
                    Row(verticalAlignment = Alignment.CenterVertically) {
                        Text("UIA", Modifier.weight(1f), style = MaterialTheme.typography.titleMedium)
                        TextButton(onClick = { page = ChatSettingsPage.Overview }) { Text("Übersicht") }
                    }
                    if (localAgent?.canChoose == true) localAgent.choices { page = null }
                }
            } else {
                Column(
                    Modifier.fillMaxWidth().verticalScroll(rememberScrollState()).padding(horizontal = 16.dp, vertical = 8.dp),
                    verticalArrangement = Arrangement.spacedBy(4.dp),
                ) {
                    Text("Chat-Einstellungen", style = MaterialTheme.typography.titleLarge)
                    if (state.local) {
                        AssistChip(
                            onClick = openModels,
                            label = { Text(localModelLabel ?: "Modell", maxLines = 1, overflow = TextOverflow.Ellipsis) },
                        )
                        if (localAgent?.canChoose == true) {
                            AssistChip(
                                onClick = openAgents,
                                label = { Text(localAgent.selectedName?.let { "UIA · $it" } ?: "UIA",
                                    maxLines = 1, overflow = TextOverflow.Ellipsis) },
                            )
                        }
                        localVisionHint?.invoke()
                    } else if (state.manage.support != null) {
                        AssistChip(
                            onClick = openModels,
                            enabled = modelBlock == null,
                            label = {
                                Text(
                                    cloudModelLabel,
                                    maxLines = 1, overflow = TextOverflow.Ellipsis,
                                )
                            },
                        )
                        modelBlock?.let {
                            Text(it, style = MaterialTheme.typography.labelSmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
                        }
                    }
                    HorizontalDivider()
                    TextButton(onClick = {
                        if (currentPickerTarget()) {
                            page = null
                            requestedVisionFallback = null
                            visionDialog = visionMode
                        }
                    }) { Text("Bildbeschreibung · ${visionMode.visionDisplayName()}") }
                    if (chatVisionKey != null) {
                        TextButton(onClick = {
                            page = null
                            visionPolicyOpen = true
                        }) { Text("Bildmodus für diesen Chat") }
                    }
                    TextButton(
                        enabled = active && chatVisionScope != null,
                        onClick = {
                            page = null
                            if (isCurrentImageEditChat()) apiImageEditOpen = true
                        },
                    ) { Text("Bild bearbeiten · Geräte-API") }
                    if (ownerStatusTarget != null) {
                        HorizontalDivider()
                        key(ownerStatusTarget) {
                            OwnerStatusAction(ownerStatusTarget, connection) { window.sessionId == sessionId }
                        }
                    }
                    if (archiveMenu.available) {
                        HorizontalDivider()
                        TextButton(enabled = archiveMenu.enabled, onClick = {
                            page = null
                            archiveMenu.open()
                        }) { Text(stringResource(R.string.chat_history_archive)) }
                    }
                }
            }
        }
    }
    if (visionPolicyOpen && chatVisionKey != null) {
        ChatVisionOverrideDialog(
            global = globalVisionMode,
            current = chatVisionOverride,
            onSelect = { mode ->
                if (window.sessionId == sessionId &&
                    sessionHomeScope(connection.state.value) == chatVisionScope &&
                    connection.state.value.generation == state.generation
                ) {
                    chatVisionOverrides.write(chatVisionKey, mode)
                }
                visionPolicyOpen = false
            },
            onClose = { visionPolicyOpen = false },
        )
    }
    when (visionDialog) {
        VisionDescriptionMode.Api -> ApiVisionDialog(
            isCurrent = { visionDialog == VisionDescriptionMode.Api && currentPickerTarget() },
            onClose = { visionDialog = null },
            onUseLocal = {
                requestedVisionFallback = null
                visionDialog = VisionDescriptionMode.Local
            },
            fallbackInvite = requestedVisionFallback,
        )
        VisionDescriptionMode.Local -> {
            if (deviceVision != null && localVisionReady) {
                key(deviceVisionState?.generation) {
                    LocalVisionDialog(
                        deviceVision,
                        onClose = { visionDialog = null },
                        onUseApi = openManualVisionApi,
                        onConfiguredApi = openConfiguredVisionApi,
                    )
                }
            } else {
                AlertDialog(
                    onDismissRequest = { visionDialog = null },
                    title = { Text("Lokale Modellanalyse nicht bereit") },
                    text = {
                        Column(verticalArrangement = Arrangement.spacedBy(6.dp)) {
                            Text("Der lokale Modellbetrieb ist nicht verbunden. Harw startet ihn nicht automatisch.")
                            LocalVisionApiAlternative(openConfiguredVisionApi, openManualVisionApi)
                        }
                    },
                    confirmButton = { TextButton(onClick = { visionDialog = null }) { Text("Schließen") } },
                )
            }
        }
        null -> Unit
    }
    if (active && apiImageEditOpen && chatVisionScope != null) {
        ApiImageEditDialog(isCurrentChat = isCurrentImageEditChat, onClose = { apiImageEditOpen = false })
    }
}
