package dev.harw.mobile

internal enum class HomeAction {
    Assistant,
    CloudAssistant,
    Settings,
    Library,
    CloudLibrary,
    NewSession,
}

internal enum class CloudShortcutState { Available, Disconnected, Unsupported }

internal data class HomeShortcut(
    val action: HomeAction,
    val enabled: Boolean,
    val cloudState: CloudShortcutState? = null,
)

private fun cloudShortcut(action: HomeAction, online: Boolean, supported: Boolean): HomeShortcut {
    val state = when {
        !online -> CloudShortcutState.Disconnected
        !supported -> CloudShortcutState.Unsupported
        else -> CloudShortcutState.Available
    }
    return HomeShortcut(action, enabled = state == CloudShortcutState.Available, cloudState = state)
}

/** The caller supplies verified availability for the visible backend and selected profile. */
internal fun homeShortcuts(
    online: Boolean, canCreate: Boolean, creating: Boolean,
    assistantDev: Boolean = false, privateLibrary: Boolean = false,
    viewingLocal: Boolean = true, cloudAssistant: Boolean = false, cloudLibrary: Boolean = false,
): List<HomeShortcut> = buildList {
    if (viewingLocal) add(HomeShortcut(HomeAction.Assistant, enabled = assistantDev))
    else add(cloudShortcut(HomeAction.CloudAssistant, online, cloudAssistant))
    add(HomeShortcut(HomeAction.Settings, enabled = true))
    if (viewingLocal) add(HomeShortcut(HomeAction.Library, enabled = privateLibrary))
    else add(cloudShortcut(HomeAction.CloudLibrary, online, cloudLibrary))
    add(HomeShortcut(HomeAction.NewSession, enabled = online && canCreate && !creating))
}
