package dev.harw.mobile

import android.content.Context
import java.security.MessageDigest
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import uniffi.harw_mobile_ffi.ProviderStore
import uniffi.harw_mobile_ffi.ModelFileState
import uniffi.harw_mobile_ffi.SessionTitleApiChoice
import uniffi.harw_mobile_ffi.SessionTitleLocalChoice
import uniffi.harw_mobile_ffi.SessionTitleTurn

internal sealed interface ExactChatTitleModel {
    val option: ChatTitleModelOption
    suspend fun generate(pairs: List<SessionTitleTurn>): String

    data class Local(
        override val option: ChatTitleModelOption,
        private val connection: HarwConnection,
        private val choice: SessionTitleLocalChoice,
    ) : ExactChatTitleModel {
        override suspend fun generate(pairs: List<SessionTitleTurn>) =
            connection.generateLocalSessionTitle(choice, pairs)
    }

    data class Configured(
        override val option: ChatTitleModelOption,
        private val providerStore: ProviderStore,
        private val choice: SessionTitleApiChoice,
    ) : ExactChatTitleModel {
        override suspend fun generate(pairs: List<SessionTitleTurn>) =
            withContext(Dispatchers.IO) { providerStore.sessionTitleGenerateApi(choice, pairs) }
    }
}

internal data class ExactChatTitleCatalog(
    val profile: String,
    val models: List<ExactChatTitleModel>,
    val installed: List<ChatTitleModelOption>,
) {
    fun forSettings(): ChatTitleCatalog = ChatTitleCatalog(profile, (models.map { it.option } + installed).distinctBy { it.key })
    fun resolve(key: ChatTitleModelKey?): ExactChatTitleModel? = models.singleOrNull { it.option.key == key }
}

internal suspend fun loadChatTitleModelCatalog(context: Context): ExactChatTitleCatalog {
    val appContext = context.applicationContext
    val app = appContext as? HarwApplication
    val network = withContext(Dispatchers.IO) {
        val store = ProviderStore(appContext.filesDir.path, AndroidPlatformKeystore(appContext))
        Triple(store.apiVisionProfileKey(), store, store.sessionTitleApiChoices())
    }
    val local = app?.localConnection
    val installed = ModelManager(ModelStore(appContext)).local().filter { it.state == ModelFileState.COMPLETE }
    val localModels = local?.sessionTitleLocalModels().orEmpty().mapNotNull { choice ->
        if (local == null) return@mapNotNull null
        val option = ChatTitleModelOption(
            key = ChatTitleModelKey(ChatTitleModelKind.Local, "local", choice.modelId, "on-device"),
            providerLabel = context.getString(R.string.chat_title_provider_local),
            modelLabel = installed.firstOrNull { it.id == choice.modelId }?.label?.ifBlank { choice.modelId } ?: choice.modelId,
        )
        ExactChatTitleModel.Local(option, local, choice)
    }
    val configuredModels = network.third.mapNotNull { choice ->
        if (choice.chatgptAccount && choice.accountScope.isNullOrBlank()) return@mapNotNull null
        val kind = if (choice.chatgptAccount) ChatTitleModelKind.ChatgptAccount else ChatTitleModelKind.ConfiguredProvider
        val exactRoute = "provider:" + titleBindingDigest(choice.routeId, choice.endpoint, kind.name)
        val option = ChatTitleModelOption(
            key = ChatTitleModelKey(kind, exactRoute, choice.modelId, titleBindingDigest(network.first, exactRoute, choice.accountScope.orEmpty())),
            providerLabel = if (choice.chatgptAccount) context.getString(R.string.chat_title_provider_linked, choice.routeId) else choice.routeId,
            modelLabel = choice.modelId,
        )
        ExactChatTitleModel.Configured(option, network.second, choice)
    }
    val installedOptions = installed.map {
        ChatTitleModelOption(
            key = ChatTitleModelKey(ChatTitleModelKind.Local, "local", it.id, "on-device"),
            providerLabel = context.getString(R.string.chat_title_provider_local), modelLabel = it.label.ifBlank { it.id }, ready = false,
        )
    }
    return ExactChatTitleCatalog(network.first, (localModels + configuredModels).distinctBy { it.option.key }, installedOptions)
}

private fun titleBindingDigest(vararg values: String): String {
    val digest = MessageDigest.getInstance("SHA-256")
    for (value in values) {
        val bytes = value.toByteArray(Charsets.UTF_8)
        digest.update(bytes.size.toString().toByteArray(Charsets.US_ASCII))
        digest.update(0.toByte())
        digest.update(bytes)
    }
    return digest.digest().joinToString("") { "%02x".format(it) }
}
