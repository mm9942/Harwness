package dev.harw.mobile

internal enum class ForkSeedBackend { LOCAL, CLOUD }

/** Capture scope; none of these origin fields are serialized in the seed payload. */
internal data class ForkSeedOrigin(
    val backend: ForkSeedBackend,
    val profileId: String,
    val sessionId: String,
    val generation: Long,
)

/** Opaque durable media reference tied to the same authorized source, not preview/cache bytes. */
internal data class ForkSeedImage(val origin: ForkSeedOrigin, val reference: String)

internal enum class ForkVisibleRole { USER, HARW, TOOL, REASONING, ERROR, SYSTEM, APPROVAL }

/** A row supplied by the captured visible transcript, never a fresher server read. */
internal data class ForkVisibleRow(
    val id: String,
    val role: ForkVisibleRole,
    val text: String,
    val timestampMillis: Long? = null,
    val images: List<ForkSeedImage> = emptyList(),
    val imagesUnavailable: Boolean = false,
)

private const val MAX_MESSAGES = 128
private const val MAX_SEED_BYTES = 128 * 1024
private const val MAX_FIELD_CHARS = 64 * 1024
private const val MAX_REF_CHARS = 2_048
private const val MAX_IMAGES_PER_MESSAGE = 40

/** A local draft contract only. No import, source reattachment, provider call or tool execution. */
internal class ForkSeed private constructor(
    private val origin: ForkSeedOrigin,
    val prefix: List<ForkVisibleRow>,
    val draft: ForkVisibleRow?,
    private val json: String,
) {
    fun jsonFor(expectedOrigin: ForkSeedOrigin): String? =
        json.takeIf { origin == expectedOrigin }

    companion object {
        fun derive(
            origin: ForkSeedOrigin,
            expectedOrigin: ForkSeedOrigin,
            displayed: List<ForkVisibleRow>,
            selectedId: String,
        ): ForkSeed? {
            if (origin != expectedOrigin || origin.profileId.isBlank() || origin.sessionId.isBlank()) return null
            val index = displayed.indexOfFirst { it.id == selectedId }
            if (index < 0 || displayed.indexOfLast { it.id == selectedId } != index) return null
            val selected = displayed[index]
            if (!selected.isConversationRow()) return null

            val prefix = ArrayList<ForkVisibleRow>()
            val seen = HashSet<String>()
            var draft: ForkVisibleRow? = null
            val json = StringBuilder("{\"version\":1,\"history\":[")
            for (i in 0..index) {
                val row = displayed[i]
                if (!row.isConversationRow()) continue
                if (seen.size == MAX_MESSAGES || !seen.add(row.id) || !row.validFor(origin)) return null
                // Encode only User/Harw data and bounded facts. The role marker is a display
                // label; it never supplies privileged instructions or authenticated provenance.
                val frozen = row.copy(images = row.images.toList())
                if (i == index && frozen.role == ForkVisibleRole.USER) {
                    draft = frozen
                } else {
                    if (prefix.isNotEmpty()) json.append(',')
                    prefix += frozen
                    json.appendRow(frozen)
                    if (json.length > MAX_SEED_BYTES) return null
                }
            }
            json.append("],\"unsent_draft\":")
            if (draft == null) json.append("null") else json.appendRow(draft)
            json.append('}')
            val payload = json.toString()
            if (payload.toByteArray(Charsets.UTF_8).size > MAX_SEED_BYTES) return null
            return ForkSeed(origin, prefix.toList(), draft, payload)
        }
    }
}

internal fun deriveForkSeed(
    origin: ForkSeedOrigin,
    expectedOrigin: ForkSeedOrigin,
    displayed: List<ForkVisibleRow>,
    selectedId: String,
): ForkSeed? = ForkSeed.derive(origin, expectedOrigin, displayed, selectedId)

private fun ForkVisibleRow.isConversationRow(): Boolean =
    role == ForkVisibleRole.USER || role == ForkVisibleRole.HARW

private fun ForkVisibleRow.validFor(origin: ForkSeedOrigin): Boolean =
    id.length in 1..128 && id.all { it in '!'..'~' } && text.length <= MAX_FIELD_CHARS &&
        !imagesUnavailable && images.size <= MAX_IMAGES_PER_MESSAGE &&
        images.all { it.origin == origin && it.reference.length in 1..MAX_REF_CHARS }

private fun StringBuilder.appendRow(row: ForkVisibleRow): StringBuilder {
    append("{\"id\":").appendQuoted(row.id)
    append(",\"role\":").appendQuoted(if (row.role == ForkVisibleRole.USER) "user" else "harw")
    append(",\"text\":").appendQuoted(row.text)
    append(",\"timestamp_ms\":").append(row.timestampMillis?.toString() ?: "null")
    append(",\"image_refs\":[")
    row.images.forEachIndexed { index, image ->
        if (index != 0) append(',')
        appendQuoted(image.reference)
    }
    return append("]}")
}

private fun StringBuilder.appendQuoted(value: String): StringBuilder {
    append('"')
    for (ch in value) {
        when (ch) {
            '"' -> append("\\\"")
            '\\' -> append("\\\\")
            '\n' -> append("\\n")
            '\r' -> append("\\r")
            '\t' -> append("\\t")
            else -> if (ch.code < 32 || ch.isSurrogate()) {
                append("\\u").append(ch.code.toString(16).padStart(4, '0'))
            } else {
                append(ch)
            }
        }
    }
    return append('"')
}
