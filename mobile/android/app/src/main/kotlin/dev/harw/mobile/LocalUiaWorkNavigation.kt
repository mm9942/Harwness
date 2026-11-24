package dev.harw.mobile

import android.os.SystemClock
import androidx.activity.compose.BackHandler
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.LazyListState
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.produceState
import androidx.compose.runtime.remember
import androidx.compose.runtime.saveable.LocalSaveableStateRegistry
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.unit.dp
import androidx.lifecycle.SavedStateHandle
import java.text.DateFormat
import java.util.Date
import kotlinx.coroutines.delay
import uniffi.harw_mobile_ffi.ApprovalDecision
import uniffi.harw_mobile_ffi.LocalAssistantSessionAccess
import uniffi.harw_mobile_ffi.MobileSession

internal const val LocalUiaWorkTitle = "Lokale Arbeitssession"

internal data class LocalUiaWorkLink(
    val sessionId: String,
    val generation: Long,
    val title: String,
    val openedAtMs: Long,
    val monotonicAtMs: Long,
)

internal sealed interface LocalUiaWorkCreate {
    data object Unavailable : LocalUiaWorkCreate
    data object Unconfirmed : LocalUiaWorkCreate
    data class Created(val link: LocalUiaWorkLink) : LocalUiaWorkCreate
}

/** Keep only RAM navigation and presentation for the one currently authenticated lease. */
internal class LocalUiaWorkNavigation {
    val privateDraft = mutableStateOf("")
    val privateTranscriptPosition = LazyListState()
    var link by mutableStateOf<LocalUiaWorkLink?>(null)
    var detail by mutableStateOf(false)
    var confirming by mutableStateOf(false)
    var starting by mutableStateOf(false)
    var issue by mutableStateOf<LocalUiaWorkCreate?>(null)
    var search by mutableStateOf<LocalAssistantSearchTarget?>(null)
    var searchOpen by mutableStateOf(false)
    var searchOpening by mutableStateOf(false)
    var searchIssue by mutableStateOf(false)
}

/** Keep a single ordinary work window in RAM for this private lease. */
internal class LocalAssistantSearchTarget(
    val generation: Long,
    val access: LocalAssistantSessionAccess,
    val canRename: Boolean,
) {
    var showingWork by mutableStateOf(false)
    var work by mutableStateOf<LocalAssistantSearchedWork?>(null)
        private set

    fun select(row: MobileSession): Boolean {
        val previous = work
        if (previous?.link?.sessionId == row.sessionId) {
            showingWork = true
            return true
        }
        if (previous != null) {
            val draft = previous.window.draft(previous.link.sessionId)
            if (draft.text.isNotEmpty() || draft.images.isNotEmpty() || draft.attempt != null || draft.sending) {
                return false
            }
        }
        val id = row.sessionId
        val title = row.title?.takeUnless(String::isBlank) ?: "Lokale Session"
        val link = LocalUiaWorkLink(id, generation, title, System.currentTimeMillis(), SystemClock.elapsedRealtime())
        work = LocalAssistantSearchedWork(link, HarwWindowModel(SavedStateHandle()).also { it.select(id) })
        showingWork = true
        return true
    }
}

internal data class LocalAssistantSearchedWork(val link: LocalUiaWorkLink, val window: HarwWindowModel)

internal fun localAssistantSummariesCurrent(state: WorkspaceState, selection: LocalAssistantSearchTarget): Boolean =
    state.local && state.online && state.generation == selection.generation &&
        state.sessionSummariesGeneration == selection.generation

internal fun localAssistantSummary(
    state: WorkspaceState, selection: LocalAssistantSearchTarget, id: String,
): MobileSession? = if (!localAssistantSummariesCurrent(state, selection)) null else
    state.sessions.asSequence().take(AssistantSummaryLimit).firstOrNull { it.sessionId == id }

@Composable
internal fun LocalUiaWorkCard(
    navigation: LocalUiaWorkNavigation,
    enabled: Boolean,
    onOpen: (LocalUiaWorkLink) -> Unit,
    onCreate: () -> Unit,
) {
    val link = navigation.link
    if (link != null) {
        val opened = remember(link) { DateFormat.getDateTimeInstance(DateFormat.SHORT, DateFormat.SHORT).format(Date(link.openedAtMs)) }
        val elapsed by produceState(0L, link) {
            while (true) {
                value = (SystemClock.elapsedRealtime() - link.monotonicAtMs).coerceAtLeast(0L) / 1_000L
                delay(1_000L)
            }
        }
        Surface(
            onClick = { onOpen(link) },
            enabled = enabled,
            modifier = Modifier.fillMaxWidth(),
            color = MaterialTheme.colorScheme.secondaryContainer,
            shape = MaterialTheme.shapes.medium,
        ) {
            Column(Modifier.padding(12.dp), verticalArrangement = Arrangement.spacedBy(2.dp)) {
                Text(link.title, style = MaterialTheme.typography.titleSmall)
                Text("Geöffnet: $opened · Seit Anlage: ${elapsed / 60} Min. ${elapsed % 60} Sek.", style = MaterialTheme.typography.bodySmall)
                Text("Arbeitssession öffnen", style = MaterialTheme.typography.labelMedium)
            }
        }
    } else {
        OutlinedButton(
            onClick = { navigation.confirming = true },
            enabled = enabled && !navigation.starting && navigation.issue != LocalUiaWorkCreate.Unconfirmed,
        ) {
            Text(if (navigation.starting) "Arbeitssession wird angelegt …" else "Leere Arbeitssession anlegen")
        }
    }
    when (navigation.issue) {
        LocalUiaWorkCreate.Unavailable -> Text("Der lokale Host kann gerade keine Session anlegen.", style = MaterialTheme.typography.bodySmall)
        LocalUiaWorkCreate.Unconfirmed -> Text("Die Anlage ist nicht bestätigt. Prüfe erst den lokalen Sessionverlauf; es wird nicht automatisch erneut versucht.", style = MaterialTheme.typography.bodySmall)
        else -> Unit
    }
    if (navigation.confirming) {
        AlertDialog(
            onDismissRequest = { navigation.confirming = false },
            title = { Text("Lokale Arbeitssession anlegen?") },
            text = {
                Text("Das öffnet eine getrennte, leere Harw-Session auf dem lokalen Host. Sie gehört nicht zum privaten Verlauf. Es werden keine Nachrichten oder Entwürfe übernommen und kein Modell wird gestartet.")
            },
            confirmButton = {
                TextButton(
                    enabled = enabled && !navigation.starting,
                    onClick = { navigation.confirming = false; onCreate() },
                ) { Text("Leer anlegen") }
            },
            dismissButton = {
                TextButton(onClick = { navigation.confirming = false }) { Text("Abbrechen") }
            },
        )
    }
}

@Composable
internal fun LocalUiaWorkConversation(
    link: LocalUiaWorkLink,
    state: WorkspaceState,
    connection: HarwConnection,
    allowed: () -> Boolean,
    authorizingApproval: Boolean,
    onDecide: (String, Long, String, ApprovalDecision) -> Unit,
    onBack: () -> Unit,
    windowOverride: HarwWindowModel? = null,
    expectLocal: Boolean = true,
) {
    BackHandler(onBack = onBack)
    val stillOnHost = state.local == expectLocal && state.online && state.generation == link.generation
    if (!stillOnHost) {
        LaunchedEffect(link, stillOnHost) { onBack() }
        return
    }
    // Do not change the Activity's normal selection, or save a route into its restore bundle.
    val workWindow = windowOverride ?: remember(link) { HarwWindowModel(SavedStateHandle()).also { it.select(link.sessionId) } }
    val workSession = state.sessions.firstOrNull { it.sessionId == link.sessionId }
    var agentVersion by remember(connection, link) { mutableIntStateOf(0) }
    val archiveMenu = remember(connection, link) { ChatArchiveMenuEntry() }
    DisposableEffect(connection, link) {
        val retained = allowed()
        if (retained) connection.retainSession(link.sessionId)
        onDispose { if (retained) connection.releaseSession(link.sessionId) }
    }
    CompositionLocalProvider(LocalSaveableStateRegistry provides null) {
        Column(Modifier.fillMaxSize()) {
            HarwHeader(link.title, onBack = onBack) {
                ChatRouteBadge(state)
                SessionChatSettings(state, workWindow, link.sessionId, workSession, connection, agentVersion, archiveMenu) { agentVersion++ }
            }
            SessionConversation(
                state = state,
                window = workWindow,
                sessionId = link.sessionId,
                session = workSession,
                connection = connection,
                authorizingApproval = authorizingApproval,
                onDecide = onDecide,
                archiveMenu = archiveMenu,
                agentVersion = agentVersion,
                showQuickPickers = expectLocal,
                modifier = Modifier.weight(1f),
            )
        }
    }
}
