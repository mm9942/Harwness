package dev.harw.mobile

import android.net.Uri
import androidx.compose.runtime.staticCompositionLocalOf

/** Only Activity memory: no Bundle, SavedStateHandle, persisted URI grant, or path. */
internal class LibraryPhotoRequest(
    val profile: LocalUiaProfile,
    val vault: VaultPinScope,
    val revision: ULong,
)

internal class PendingLibraryPhoto(val request: LibraryPhotoRequest, val uri: Uri)

internal class PrivateLibraryGallery(
    val pending: PendingLibraryPhoto?,
    val request: (VaultPinScope, ULong) -> Unit,
    val consume: (PendingLibraryPhoto) -> Boolean,
)

/** Host-owned external picker; never invoked by a still-unlocked Activity route. */
internal val LocalPrivateLibraryGallery = staticCompositionLocalOf<PrivateLibraryGallery?> { null }
