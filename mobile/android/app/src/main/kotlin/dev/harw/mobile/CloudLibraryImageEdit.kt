package dev.harw.mobile

/** Short-lived origin. The native library independently checks scope and revision on every RPC. */
internal data class CloudLibraryImageEditIdentity<S>(
    val scope: S,
    val revision: ULong,
    val assetId: String,
) {
    fun permits(liveScope: S?, currentRevision: ULong?, currentAssetIds: Collection<String>, foreground: Boolean): Boolean =
        foreground && revision != 0uL && scope == liveScope && revision == currentRevision && assetId in currentAssetIds

    fun derivativeName(digest: String): String = libraryDerivativeName(assetId, digest)
}

/** Human-visible lineage only, not an immutable store-level provenance record. */
internal fun libraryDerivativeName(assetId: String, digest: String): String {
    require(assetId.length in 1..64 && assetId.all { it in '0'..'9' || it in 'a'..'z' || it in 'A'..'Z' || it == '-' || it == '_' })
    require(digest.length == 64 && digest.all { it in '0'..'9' || it in 'a'..'f' })
    return "API-Bearbeitung · Quelle $assetId · gpt-image-1.5 · ${digest.take(16)}"
}

internal sealed interface LibraryImageEditDestination {
    data object PrivateDevice : LibraryImageEditDestination
    data class Cloud(val gateway: String) : LibraryImageEditDestination
}

/** No more than one image and one pinned destination; never writes without the second confirmation. */
internal class LibraryApiImageEdit(
    val label: String,
    val destination: LibraryImageEditDestination,
    // Delivers owned bytes before an intervening dispatcher handoff can discard the cleanup handle.
    val readExactJpeg: suspend ((ByteArray) -> Unit) -> Unit,
    val requireExactOrigin: suspend () -> Unit,
    val saveExactDerivative: suspend (digest: String, jpeg: ByteArray) -> Unit,
)
