package dev.harw.mobile

import android.content.Context
import android.content.SharedPreferences
import java.security.MessageDigest
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.rememberUpdatedState
import androidx.compose.runtime.setValue
import androidx.compose.runtime.snapshotFlow
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.ui.Modifier
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.currentCoroutineContext
import kotlinx.coroutines.ensureActive
import kotlinx.coroutines.launch
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import uniffi.harw_mobile_ffi.ServerProfile
import uniffi.harw_mobile_ffi.SessionTitleTurn

private data class TitleAttemptKey(
    val connection: HarwConnection,
    val profile: ServerProfile?,
    val device: String?,
    val session: String,
    val model: ChatTitleModelKey,
)

private object TitleAttemptLedger {
    private data class Entry(val seedDigest: String, val candidate: String?)
    private val lock = Mutex()
    private val attempted = LinkedHashMap<TitleAttemptKey, Entry>()

    /** Reuse a successful candidate after a stale head only for the same exact source text. */
    suspend fun candidate(
        key: TitleAttemptKey,
        pairs: List<SessionTitleTurn>,
        mayGenerate: () -> Boolean,
        generate: suspend () -> String,
    ): String? = lock.withLock {
        if (!mayGenerate()) return@withLock null
        val digest = titleSeedDigest(pairs)
        attempted[key]?.let { return@withLock it.candidate.takeIf { _ -> it.seedDigest == digest } }
        if (attempted.size >= 256) attempted.remove(attempted.keys.first())
        attempted[key] = Entry(digest, null)
        generate().also { attempted[key] = Entry(digest, it) }
    }
}

private fun titleSeedDigest(pairs: List<SessionTitleTurn>): String {
    val digest = MessageDigest.getInstance("SHA-256")
    for (turn in pairs) {
        for (part in listOf(turn.user, turn.agent)) {
            val bytes = part.toByteArray(Charsets.UTF_8)
            digest.update(bytes.size.toString().toByteArray(Charsets.US_ASCII))
            digest.update(0.toByte())
            digest.update(bytes)
        }
    }
    return digest.digest().joinToString("") { "%02x".format(it) }
}

internal data class ChatTitleFallbackOffer(val label: String, val confirm: () -> Unit)
private data class ExactTitleFallback(val profile: String, val prefs: ChatTitlePreferences, val option: ChatTitleModelOption)

@Composable
internal fun ChatTitleFallbackButton(offer: ChatTitleFallbackOffer, modifier: Modifier = Modifier) {
    var open by remember(offer.label) { mutableStateOf(false) }
    TextButton(onClick = { open = true }, modifier = modifier) { Text("Titel per API-Fallback erzeugen") }
    if (open) {
        AlertDialog(
            onDismissRequest = { open = false },
            title = { Text("API-Fallback für diesen Titel nutzen?") },
            text = { Text("Die ersten drei vollständigen Textpaare werden einmal an ${offer.label} gesendet. Keine Bilder oder übrigen Nachrichten.") },
            confirmButton = {
                TextButton(onClick = { open = false; offer.confirm() }) { Text("Senden") }
            },
            dismissButton = { TextButton(onClick = { open = false }) { Text("Abbrechen") } },
        )
    }
}

/** The durable cursor only triggers a recheck; neither it nor the visible tail supplies title text. */
@Composable
internal fun rememberAutomaticChatTitle(
    context: Context,
    connection: HarwConnection,
    profile: ServerProfile?,
    deviceNodeId: String?,
    generation: Long?,
    sessionId: String,
    committedCursor: ULong?,
    hasTitle: Boolean,
): ChatTitleFallbackOffer? {
    val titleWork by rememberUpdatedState(committedCursor.takeUnless { hasTitle })
    val scope = rememberCoroutineScope()
    var fallback by remember(connection, profile, deviceNodeId, generation, sessionId) {
        mutableStateOf<ExactTitleFallback?>(null)
    }
    var sendingFallback by remember(connection, profile, generation, sessionId) { mutableStateOf(false) }
    val prefs = remember(context) { context.getSharedPreferences(UI_PREFS, Context.MODE_PRIVATE) }
    var titleSettingsRevision by remember(prefs) { mutableIntStateOf(0) }
    DisposableEffect(prefs) {
        val listener = SharedPreferences.OnSharedPreferenceChangeListener { _, key ->
            if (key == null || isChatTitlePreferenceKey(key)) titleSettingsRevision++
        }
        prefs.registerOnSharedPreferenceChangeListener(listener)
        onDispose { prefs.unregisterOnSharedPreferenceChangeListener(listener) }
    }
    fun current(): Boolean = generation != null && connection.state.value.let { state ->
        state.online && state.profile == profile && state.deviceNodeId == deviceNodeId &&
            state.generation == generation && state.windows[sessionId]?.generation == generation
    }
    fun approved(owner: String, config: ChatTitlePreferences): Boolean =
        current() && readChatTitlePreferences(prefs, owner) == config

    suspend fun candidate(model: ExactChatTitleModel, owner: String, config: ChatTitlePreferences, pairs: List<uniffi.harw_mobile_ffi.AutoTitleSeedPair>): String? {
        if (!approved(owner, config) || pairs.size != 3) return null
        val key = TitleAttemptKey(connection, profile, deviceNodeId, sessionId, model.option.key)
        val input = pairs.map { SessionTitleTurn(it.user, it.assistant) }
        val title = TitleAttemptLedger.candidate(key, input, { approved(owner, config) }) {
            currentCoroutineContext().ensureActive()
            model.generate(input)
        }
        currentCoroutineContext().ensureActive()
        return title.takeIf { approved(owner, config) }
    }

    LaunchedEffect(connection, profile, deviceNodeId, generation, sessionId) {
        if (generation == null) return@LaunchedEffect
        // Settings changes must be able to re-evaluate an already-eligible
        // untitled chat. They do not bypass exact-profile seed checks or the
        // per-model attempt ledger, and never approve a fallback by themselves.
        snapshotFlow { titleWork to titleSettingsRevision }.collect { (cursor, _) ->
            if (fallback?.let { !approved(it.profile, it.prefs) } == true) fallback = null
            if (cursor == null || !mayHaveEnabledChatTitle(prefs)) {
                fallback = null
                return@collect
            }
            try {
                val catalog = loadChatTitleModelCatalog(context)
                val captured = readChatTitlePreferences(prefs, catalog.profile)
                if (!captured.enabled || !current()) return@collect
                val modelKey = captured.model ?: return@collect
                val localPreference = modelKey.kind == ChatTitleModelKind.Local
                val model = catalog.resolve(modelKey) ?: if (localPreference && captured.allowLoadedLocalAlternative) {
                    catalog.models.singleOrNull { it.option.key.kind == ChatTitleModelKind.Local }
                } else null
                val apiOffer = if (localPreference) {
                    catalog.resolve(captured.apiFallback)?.option?.takeIf { it.key.kind != ChatTitleModelKind.Local }
                } else null
                if (model == null && apiOffer == null) return@collect
                connection.withAutoTitle(profile, generation, sessionId) { pairs ->
                    if (!approved(catalog.profile, captured)) return@withAutoTitle null
                    val title = try {
                        if (model == null) null else candidate(model, catalog.profile, captured, pairs)
                    } catch (cancelled: CancellationException) {
                        throw cancelled
                    } catch (_: Exception) {
                        null
                    }
                    fallback = if (title == null && approved(catalog.profile, captured)) {
                        apiOffer?.let { ExactTitleFallback(catalog.profile, captured, it) }
                    } else null
                    title
                }
            } catch (cancelled: CancellationException) {
                throw cancelled
            } catch (_: Exception) {
                // Optional title work; leave the neutral label and do not make another route request.
            }
        }
    }

    val offer = fallback?.takeIf { generation != null && !hasTitle && !sendingFallback && approved(it.profile, it.prefs) }
        ?: return null
    return ChatTitleFallbackOffer("${offer.option.providerLabel} · ${offer.option.modelLabel}") {
        val activeGeneration = generation ?: return@ChatTitleFallbackOffer
        if (!sendingFallback && approved(offer.profile, offer.prefs)) {
            sendingFallback = true
            fallback = null
            scope.launch {
                try {
                    val catalog = loadChatTitleModelCatalog(context)
                    val model = catalog.resolve(offer.option.key)
                    if (catalog.profile != offer.profile || model == null || !approved(offer.profile, offer.prefs)) return@launch
                    connection.withAutoTitle(profile, activeGeneration, sessionId) { pairs ->
                        candidate(model, offer.profile, offer.prefs, pairs)
                    }
                } catch (cancelled: CancellationException) {
                    throw cancelled
                } catch (_: Exception) {
                    // Explicit request failed. Never silently resend or choose another route.
                } finally {
                    sendingFallback = false
                }
            }
        }
    }
}
