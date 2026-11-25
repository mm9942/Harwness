package dev.harw.mobile

import android.os.SystemClock
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue

/** Lives with the window, not with a particular composition of an opened chat. */
internal class ChatNoticeLedger(private val elapsed: () -> Long = SystemClock::elapsedRealtime) {
    private var notices by mutableStateOf<Map<Any, Notice>>(emptyMap())
    private data class Notice(val sendUntil: Long, val dismissed: Boolean = false)

    fun show(key: Any): Long {
        val existing = notices[key]
        if (existing != null) return existing.sendUntil
        val deadline = elapsed() + NoticeDurationMs
        val next = LinkedHashMap(notices)
        if (next.size >= MaxNotices) next.remove(next.keys.first())
        next[key] = Notice(deadline)
        notices = next
        return deadline
    }

    fun isDismissed(key: Any): Boolean = notices[key]?.let { it.dismissed || elapsed() >= it.sendUntil } == true

    fun dismiss(key: Any) {
        val notice = notices[key] ?: return
        if (!notice.dismissed) notices = notices + (key to notice.copy(dismissed = true))
    }

    private companion object {
        const val NoticeDurationMs = 60_000L
        const val MaxNotices = 256
    }
}
