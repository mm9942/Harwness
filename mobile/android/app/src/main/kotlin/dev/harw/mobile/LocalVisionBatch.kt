package dev.harw.mobile

import java.util.concurrent.ConcurrentHashMap
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.async
import kotlinx.coroutines.awaitAll
import kotlinx.coroutines.channels.Channel
import kotlinx.coroutines.coroutineScope
import kotlinx.coroutines.ensureActive
import kotlinx.coroutines.launch

internal const val LocalVisionMaxRefs = 50
internal const val LocalVisionMaxImageEncodedBytes = 240_000
internal const val LocalVisionMaxEncodedBytes = 12_000_000
private const val LocalVisionDecodeConcurrency = 2

internal data class LocalVisionRef<T>(val id: ULong, val value: T)
internal data class LocalVisionPrepared<T>(val value: T, val encodedBytes: Int)

internal enum class LocalVisionPartialCause { OutputLimit, ContextLimit }

internal sealed interface LocalVisionItemState {
    data object Preparing : LocalVisionItemState
    data object Admitted : LocalVisionItemState
    data object Unreadable : LocalVisionItemState
    data object SizeLimit : LocalVisionItemState
    data object InvalidInput : LocalVisionItemState
    data object Failed : LocalVisionItemState
    data class Caption(val text: String) : LocalVisionItemState
    data class Partial(val text: String, val cause: LocalVisionPartialCause) : LocalVisionItemState
}

internal fun <T> appendLocalVisionRefs(
    refs: List<LocalVisionRef<T>>,
    picked: List<T>,
    nextId: ULong,
): List<LocalVisionRef<T>>? {
    val seen = refs.mapTo(HashSet()) { it.value }
    val added = ArrayList<LocalVisionRef<T>>()
    for (item in picked) {
        if (!seen.add(item)) continue
        if (refs.size + added.size == LocalVisionMaxRefs) return null
        added += LocalVisionRef(nextId + added.size.toULong(), item)
    }
    return refs + added
}

/** Snapshot inputs first, prepare at most two at a time, and invoke the chosen local model once. */
internal suspend fun <R, T> runLocalVisionBatch(
    refs: List<LocalVisionRef<R>>,
    maxEncodedBytes: Int = LocalVisionMaxEncodedBytes,
    prepare: suspend (LocalVisionRef<R>) -> LocalVisionPrepared<T>?,
    describe: suspend (List<LocalVisionRef<T>>, (ULong, LocalVisionItemState) -> Unit) -> Unit,
    update: (ULong, LocalVisionItemState) -> Unit,
) = coroutineScope {
    require(refs.size <= LocalVisionMaxRefs && refs.map { it.id }.toSet().size == refs.size)
    var remaining = maxEncodedBytes.coerceIn(0, LocalVisionMaxEncodedBytes)
    val admitted = ArrayList<LocalVisionRef<T>>(refs.size)
    for (window in refs.chunked(LocalVisionDecodeConcurrency)) {
        ensureActive()
        for (ref in window) update(ref.id, LocalVisionItemState.Preparing)
        val prepared = window.map { ref ->
            async {
                try {
                    prepare(ref)
                } catch (error: CancellationException) {
                    throw error
                } catch (_: Exception) {
                    null
                }
            }
        }.awaitAll()
        for ((index, ref) in window.withIndex()) {
            val result = prepared[index]
            when {
                result == null -> update(ref.id, LocalVisionItemState.Unreadable)
                result.encodedBytes !in 1..minOf(LocalVisionMaxImageEncodedBytes, remaining) ->
                    update(ref.id, LocalVisionItemState.SizeLimit)
                else -> {
                    remaining -= result.encodedBytes
                    admitted += LocalVisionRef(ref.id, result.value)
                    update(ref.id, LocalVisionItemState.Admitted)
                }
            }
        }
    }
    ensureActive()
    if (admitted.isEmpty()) return@coroutineScope

    // The native callback may arrive on different threads. Do not update Compose there.
    val knownIds = admitted.mapTo(HashSet()) { it.id }
    val queued = ConcurrentHashMap.newKeySet<ULong>()
    val events = Channel<Pair<ULong, LocalVisionItemState>>(admitted.size)
    val settled = HashSet<ULong>()
    val collector = launch {
        for ((id, outcome) in events) {
            settled += id
            update(id, outcome)
        }
    }
    try {
        describe(admitted) { id, outcome ->
            if (id in knownIds && queued.add(id)) events.trySend(id to outcome)
        }
    } catch (error: CancellationException) {
        throw error
    } catch (_: Exception) {
        // Never show FFI exception messages: they can contain model or file paths.
    } finally {
        events.close()
    }
    collector.join()
    for (id in knownIds - settled) update(id, LocalVisionItemState.Failed)
}
