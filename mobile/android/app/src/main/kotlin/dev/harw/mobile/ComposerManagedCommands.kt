package dev.harw.mobile

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.Button
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.produceState
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.unit.dp
import kotlinx.coroutines.CancellationException
import uniffi.harw_mobile_ffi.ManageCommand

internal class ComposerManagedRequest(val draft: String)

/**
 * Exact no-argument Observe forms in harw-ops' RemotePolicy.
 * The intersection with the attached host's command catalogue is the actual UI inventory.
 * Never infer a verb's safety from its description, family-level availability or spelling.
 * Goal/plan stay off until the gateway actually injects their scoped stores and principal.
 */
internal enum class ComposerManagedRead(val line: String) {
    Status("/status"),
    Usage("/usage"),
    ModelShow("/model show"),
    ModelList("/model list"),
    ModelsShow("/models show"),
    ModelsList("/models list"),
    ImageModels("/models image show"),
    WorkerModels("/models worker"),
    ModeShow("/mode show"),
    EffortShow("/effort show"),
    AgentsList("/agent list"),
    JobsList("/jobs list"),
    SkillsList("/skills list"),
    ;

    private val family: String get() = line.substring(1).substringBefore(' ')

    fun fromCatalog(commands: List<ManageCommand>, exactDeviceForms: Boolean = false): ManageCommand? =
        commands.singleOrNull { row ->
            row.name == family && row.remote && row.available && (!exactDeviceForms ||
                row.subcommands.any { hint -> hint.args.isEmpty() && "/$family ${hint.name}" == line })
        }

    companion object {
        fun parse(text: String): ComposerManagedRead? {
            val line = normalizedComposerCommand(text) ?: return null
            return entries.singleOrNull { it.line == line }
        }
    }
}

private fun normalizedComposerCommand(text: String): String? {
    if (text.length > MAX_LINE_CHARS || text.any { Character.isISOControl(it) }) return null
    val line = text.trim(' ')
    if (!line.startsWith('/') || line.drop(1).any {
            it !in 'a'..'z' && it !in 'A'..'Z' && it !in '0'..'9' && it != '-' && it != ' '
        }
    ) return null
    return line.split(' ').filter(String::isNotEmpty).joinToString(" ").lowercase()
}

private class AdvertisedRead(val read: ComposerManagedRead, val row: ManageCommand) {
    fun names(): Sequence<String> {
        val suffix = read.line.substringAfter(' ', "").takeIf(String::isNotEmpty)?.let { " $it" } ?: ""
        return sequenceOf(read.line) + row.aliases.asSequence().mapNotNull { alias ->
            normalizedComposerCommand("/${alias.removePrefix("/")}")?.takeUnless { it == "/" || ' ' in it }
                ?.let { it + suffix }
        }
    }
}

@Composable
internal fun ComposerManagedCommandSheet(
    request: ComposerManagedRequest,
    connection: HarwConnection,
    state: WorkspaceState,
    sessionId: String,
    generation: Long,
    currentSession: () -> Boolean,
    onClose: () -> Unit,
    onSuccess: () -> Unit,
) {
    // Do not consume WorkspaceState.manage.commands: the shared settings panel can point
    // at a different scope. Fetch for this exact attached session once per opening.
    val advertised by produceState<List<AdvertisedRead>?>(null, connection, sessionId, generation, request) {
        value = try {
            val catalog = connection.composerCommandCatalog(sessionId, generation)
            val resolved = ComposerManagedRead.entries.mapNotNull { read ->
                read.fromCatalog(catalog, exactDeviceForms = state.local)?.let { AdvertisedRead(read, it) }
            }
            if (currentSession()) resolved else emptyList()
        } catch (cancelled: CancellationException) {
            throw cancelled
        } catch (_: Exception) {
            emptyList()
        }
    }
    val originalLine = normalizedComposerCommand(request.draft)
    val browsingAll = originalLine == "/" || originalLine == "/help"
    var query by remember(request) {
        mutableStateOf(if (browsingAll) "" else originalLine ?: request.draft.filterNot { Character.isISOControl(it) }.take(MAX_LINE_CHARS))
    }
    // Entry with an exact invocation opens its review; the user can return to the picker.
    var chooseExact by remember(request) { mutableStateOf(true) }
    var selected by remember(request) { mutableStateOf<ComposerManagedRead?>(null) }
    var card by remember(request) { mutableStateOf<ManageCard?>(null) }
    var failed by remember(request) { mutableStateOf(false) }
    val available = advertised.orEmpty()
    val exact = if (chooseExact && !browsingAll && originalLine != null) {
        val canonical = ComposerManagedRead.parse(originalLine)
        if (canonical != null) available.firstOrNull { it.read == canonical }
        else available.singleOrNull { entry -> entry.names().any { it == originalLine } }
    } else null
    val chosen = selected?.let { read -> available.firstOrNull { it.read == read } } ?: exact
    val busy = state.manage.running != null || state.manage.stepUp != null

    Column(
        Modifier.fillMaxWidth().verticalScroll(rememberScrollState()).padding(16.dp),
        verticalArrangement = Arrangement.spacedBy(10.dp),
    ) {
        HarwHeader(title = chosen?.read?.line ?: stringResource(R.string.chat_slash_title), onBack = onClose) {}
        if (advertised == null) {
            Text(stringResource(R.string.chat_slash_host_loading))
        } else if (available.isEmpty() || !currentSession()) {
            Text(stringResource(R.string.chat_slash_host_unavailable))
            Text(stringResource(R.string.chat_slash_help))
        } else if (chosen != null) {
            Text(chosen.row.summary)
            val form = chosen.read.line.substringAfter(' ', "")
            chosen.row.subcommands.firstOrNull { it.name == form || it.name == form.substringBefore(' ') }?.summary?.takeIf { it.isNotBlank() }?.let {
                Text(it)
            }
            if (failed) Text(stringResource(R.string.chat_slash_host_unavailable))
            if (card == null) {
                Button(
                    enabled = !busy,
                    onClick = {
                        val now = connection.state.value.manage
                        if (currentSession() && now.running == null && now.stepUp == null) {
                            failed = false
                            val line = chosen.read.line
                            connection.runCommand(sessionId, line, publishCard = false) { result ->
                                if (currentSession()) {
                                    if (result == null) failed = true
                                    else {
                                        card = ManageCard(line, result)
                                        if (result.ok) onSuccess()
                                    }
                                }
                            }
                        } else failed = true
                    },
                ) { Text(stringResource(R.string.chat_slash_host_run)) }
            }
            card?.let { CommandResultCard(it, onClose) }
            TextButton(enabled = !busy, onClick = {
                chooseExact = false
                selected = null
                failed = false
                card = null
                query = ""
            }) { Text(stringResource(R.string.chat_slash_host_all)) }
        } else {
            Text(stringResource(R.string.chat_slash_host_discover))
            Text(stringResource(R.string.chat_slash_help))
            OutlinedTextField(
                value = query,
                onValueChange = { query = it.take(MAX_LINE_CHARS).replace("\n", "").replace("\r", "") },
                label = { Text(stringResource(R.string.chat_slash_host_filter)) },
                modifier = Modifier.fillMaxWidth(),
                singleLine = true,
            )
            val needle = query.trim().removePrefix("/").lowercase()
            val matches = if (needle.isEmpty()) available else available.filter { candidate ->
                candidate.names().any { it.removePrefix("/").startsWith(needle) }
            }.ifEmpty {
                // An incomplete/unsupported verb in a known family may show only its safe alternatives.
                val family = needle.substringBefore(' ')
                available.filter { candidate ->
                    candidate.names().any { it.removePrefix("/").substringBefore(' ') == family }
                }
            }
            if (matches.isEmpty()) {
                Text(stringResource(R.string.chat_slash_host_no_match))
                if (query.isNotEmpty()) TextButton(onClick = { query = "" }) {
                    Text(stringResource(R.string.chat_slash_host_all))
                }
            }
            matches.forEach { candidate ->
                TextButton(
                    enabled = !busy,
                    onClick = {
                        // Selecting is completion/review only. The next explicit tap executes.
                        chooseExact = false
                        selected = candidate.read
                    },
                ) { Text(candidate.read.line) }
            }
        }
    }
}
