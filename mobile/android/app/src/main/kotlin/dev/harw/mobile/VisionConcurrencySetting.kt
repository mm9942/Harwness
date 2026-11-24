package dev.harw.mobile

/** Requested preference is separate from the proven on-device runtime cap. */
internal val LocalVisionConcurrencyRange = 1..5
internal const val DefaultLocalVisionConcurrency = 3
internal enum class VisionConcurrencyChange {
    Applied, Unchanged, Invalid, NativeUnavailable, PersistenceFailed, RestoreFailed,
}

/** Caller serializes edits and runs them off the UI thread. The two write ports report success. */
internal fun changeVisionConcurrency(
    current: Int,
    requested: Int,
    applyNative: (Int) -> Boolean,
    persist: (Int) -> Boolean,
): VisionConcurrencyChange {
    if (current !in LocalVisionConcurrencyRange || requested !in LocalVisionConcurrencyRange) {
        return VisionConcurrencyChange.Invalid
    }
    if (requested == current) return VisionConcurrencyChange.Unchanged
    if (!applyNative(requested)) return VisionConcurrencyChange.NativeUnavailable
    if (persist(requested)) return VisionConcurrencyChange.Applied

    val restoredNative = applyNative(current)
    // A failed SharedPreferences commit may have already changed its process cache. Restore it too.
    val restoredPreference = persist(current)
    return if (restoredNative && restoredPreference) {
        VisionConcurrencyChange.PersistenceFailed
    } else {
        VisionConcurrencyChange.RestoreFailed
    }
}
