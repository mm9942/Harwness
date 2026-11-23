package dev.harw.mobile

import android.content.Context
import org.json.JSONArray
import org.json.JSONObject
import uniffi.harw_mobile_ffi.MobileToolState
import uniffi.harw_mobile_ffi.TranscriptEntry
import uniffi.harw_mobile_ffi.TranscriptKind
import java.io.File
import java.security.MessageDigest

/**
 * Frühere Fassungen einer Nachricht ("Bearbeiten" und "Neu generieren"): Vor dem Ersetzen wird der bisherige
 * Verlauf ab der Nachricht (samt Antworten) gemerkt, damit man ihn per Pfeiltasten ansehen kann.
 * Die Fassungen sind nur eine Ansicht auf dem Telefon; der Server kennt nur die aktuelle.
 */
internal data class ChatVersion(val tail: List<TranscriptEntry>)

internal object ChatVersions {
    private const val MAX_VERSIONS_PER_MESSAGE = 20
    private const val MAX_FILE_BYTES = 2_000_000

    private fun file(context: Context, sessionId: String): File {
        val dir = File(context.filesDir, "chat-versions").apply { mkdirs() }
        val name = MessageDigest.getInstance("SHA-256").digest(sessionId.toByteArray())
            .joinToString("") { "%02x".format(it) }.take(32)
        return File(dir, "$name.json")
    }

    /** Schlüssel einer Nachricht: Id des Eintrags davor (leer = erste Nachricht). */
    fun anchorOf(entries: List<TranscriptEntry>, index: Int): String = entries.getOrNull(index - 1)?.id.orEmpty()

    fun load(context: Context, sessionId: String): Map<String, List<ChatVersion>> = try {
        val f = file(context, sessionId)
        if (!f.isFile || f.length() > MAX_FILE_BYTES) emptyMap() else {
            val root = JSONObject(f.readText())
            root.keys().asSequence().associateWith { anchor ->
                val versions = root.getJSONArray(anchor)
                (0 until versions.length()).map { v ->
                    val tail = versions.getJSONArray(v)
                    ChatVersion((0 until tail.length()).map { entryOf(tail.getJSONObject(it)) })
                }
            }
        }
    } catch (_: Exception) {
        emptyMap()
    }

    fun append(
        context: Context,
        sessionId: String,
        current: Map<String, List<ChatVersion>>,
        anchor: String,
        version: ChatVersion,
    ): Map<String, List<ChatVersion>> {
        val list = (current[anchor].orEmpty() + version).takeLast(MAX_VERSIONS_PER_MESSAGE)
        val next = current + (anchor to list)
        try {
            val root = JSONObject()
            next.forEach { (key, versions) ->
                root.put(key, JSONArray().also { arr ->
                    versions.forEach { v -> arr.put(JSONArray().also { tail -> v.tail.forEach { tail.put(jsonOf(it)) } }) }
                })
            }
            val text = root.toString()
            if (text.length < MAX_FILE_BYTES) file(context, sessionId).writeText(text)
        } catch (_: Exception) {
        }
        return next
    }

    private fun jsonOf(e: TranscriptEntry) = JSONObject()
        .put("id", e.id).put("kind", e.kind.name).put("text", e.text)
        .put("tool", e.toolState?.name ?: JSONObject.NULL)
        .put("images", JSONArray(e.images))

    private fun entryOf(o: JSONObject) = TranscriptEntry(
        id = o.getString("id"),
        kind = TranscriptKind.valueOf(o.getString("kind")),
        text = o.getString("text"),
        toolState = if (o.isNull("tool")) null else MobileToolState.valueOf(o.getString("tool")),
        images = o.optJSONArray("images")?.let { a -> (0 until a.length()).map { a.getString(it) } } ?: emptyList(),
    )
}

/** Zustand und Aktionen der Fassungen für die Unterhaltungsansicht. */
internal class VersionsUi(
    val versions: Map<String, List<ChatVersion>>,
    val viewing: Map<String, Int>,
    val onView: (anchor: String, index: Int?) -> Unit,
    val onEdit: (entry: TranscriptEntry, anchor: String, text: String, images: List<uniffi.harw_mobile_ffi.PromptImage>) -> Unit,
)

/** Zeigt, falls eine ältere Fassung gewählt ist, deren Verlauf statt des aktuellen ab dieser Nachricht. Liefert (Liste, ab welchem Index nur Ansicht). */
internal fun applyViewing(entries: List<TranscriptEntry>, ui: VersionsUi): Pair<List<TranscriptEntry>, Int> {
    for ((i, entry) in entries.withIndex()) {
        if (entry.kind != TranscriptKind.USER) continue
        val anchor = ChatVersions.anchorOf(entries, i)
        val old = ui.viewing[anchor] ?: continue
        val version = ui.versions[anchor]?.getOrNull(old) ?: continue
        return (entries.take(i) + version.tail) to i
    }
    return entries to Int.MAX_VALUE
}

/**
 * A local archived suffix is not on the host. At its first row the prefix is
 * still the current transcript's prefix; later rows have no honest boundary.
 */
internal fun forkBoundaryId(
    current: List<TranscriptEntry>,
    displayed: List<TranscriptEntry>,
    index: Int,
    firstArchived: Int,
): String? {
    val shown = displayed.getOrNull(index)?.takeIf { it.kind == TranscriptKind.USER } ?: return null
    val canonical = current.getOrNull(index)?.takeIf { it.kind == TranscriptKind.USER } ?: return null
    return when {
        index < firstArchived && shown.id == canonical.id -> canonical.id
        index == firstArchived -> canonical.id
        else -> null
    }
}

/** Versehentlich mehrfach gesendete, identische Nachrichten ohne Antwort dazwischen zeigt die Ansicht nur einmal. */
internal fun dedupeUserRuns(entries: List<TranscriptEntry>): List<TranscriptEntry> {
    if (entries.size < 2) return entries
    val out = ArrayList<TranscriptEntry>(entries.size)
    for (entry in entries) {
        val last = out.lastOrNull()
        if (entry.kind == TranscriptKind.USER && last != null && last.kind == TranscriptKind.USER &&
            last.text == entry.text && last.images.size == entry.images.size
        ) continue
        out.add(entry)
    }
    return out
}
