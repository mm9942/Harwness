package dev.harw.mobile

import java.nio.ByteBuffer
import java.nio.ByteOrder
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext

/** Source text is never stored in memory records or sent via the memory ABI. */
internal class UiaMemorySelections(val revision: Long, val marked: Set<Int>)

internal sealed interface UiaMemoryResult {
    class Current(val selections: UiaMemorySelections) : UiaMemoryResult
    data object Changed : UiaMemoryResult
    data object Unavailable : UiaMemoryResult
}

internal class UiaManualMemoryPort(
    private val call: (Long, Long, Int, Int, ByteBuffer) -> Long,
) {
    suspend fun read(roomRevision: Long): UiaMemoryResult =
        request(roomRevision, 0, 0, 0)

    suspend fun set(roomRevision: Long, prior: UiaMemorySelections, ordinal: Int, save: Boolean): UiaMemoryResult =
        request(roomRevision, prior.revision, ordinal, if (save) 1 else 2)

    private suspend fun request(room: Long, memory: Long, turn: Int, operation: Int): UiaMemoryResult =
        withContext(Dispatchers.IO) {
            if (room !in 0L..128L || memory < 0 || (operation != 0 && turn !in 1..room.toInt())) {
                return@withContext UiaMemoryResult.Unavailable
            }
            val frame = ByteBuffer.allocateDirect(24).order(ByteOrder.BIG_ENDIAN)
            try {
                when (call(room, memory, turn, operation, frame)) {
                    -8L -> UiaMemoryResult.Changed
                    24L -> {
                        val revision = frame.getLong(0)
                        val marked = buildSet {
                            for (ordinal in 1..128) {
                                val bit = 1 shl ((ordinal - 1) % 8)
                                if ((frame.get(8 + (ordinal - 1) / 8).toInt() and bit) != 0) add(ordinal)
                            }
                        }
                        if (revision < 0 || marked.any { it > room }) UiaMemoryResult.Unavailable
                        else UiaMemoryResult.Current(UiaMemorySelections(revision, marked))
                    }
                    else -> UiaMemoryResult.Unavailable
                }
            } catch (_: LinkageError) {
                UiaMemoryResult.Unavailable
            } finally {
                frame.clear()
                while (frame.hasRemaining()) frame.put(0)
            }
        }
}
