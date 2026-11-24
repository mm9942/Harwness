package dev.harw.mobile

internal enum class KnowledgeAvailability {
    NoCloudProfile,
    Offline,
    Checking,
    ManagementUnavailable,
    ObserveDenied,
    NotOffered,
}

internal data class KnowledgeCloudSupport(val manage: Boolean, val observe: Boolean)

internal fun knowledgeAvailability(
    hasCloudProfile: Boolean,
    online: Boolean,
    connecting: Boolean,
    support: KnowledgeCloudSupport?,
): KnowledgeAvailability = when {
    connecting -> KnowledgeAvailability.Checking
    !hasCloudProfile -> KnowledgeAvailability.NoCloudProfile
    !online -> KnowledgeAvailability.Offline
    support == null -> KnowledgeAvailability.Checking
    !support.manage -> KnowledgeAvailability.ManagementUnavailable
    !support.observe -> KnowledgeAvailability.ObserveDenied
    // Generic manage/observe grants do not prove that a knowledge feature has a mobile read API.
    else -> KnowledgeAvailability.NotOffered
}
