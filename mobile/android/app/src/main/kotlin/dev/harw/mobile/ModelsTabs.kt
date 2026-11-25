package dev.harw.mobile

import androidx.compose.material3.Tab
import androidx.compose.material3.TabRow
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import uniffi.harw_mobile_ffi.ProviderStore

internal enum class ModelsTab(val label: String) {
    Local("Lokal"),
    Cloud("Cloud"),
}

/**
 * "Modelle & Provider": Tab Cloud = Gateway-Sicht (unveraendert), Tab Lokal = Provider-Routen
 * des On-Device-Stores samt Import aus dem Cloud-Katalog.
 */
@Composable
internal fun ModelsAndProviders(
    initialTab: ModelsTab,
    cloudState: WorkspaceState,
    cloudConnection: HarwConnection,
    cloudSessionId: String?,
    providerStore: ProviderStore,
    onBack: () -> Unit,
    onOpenConnection: () -> Unit,
    onOpenLocalModels: () -> Unit,
) {
    var tab by rememberSaveable { mutableStateOf(initialTab) }
    val tabs: @Composable () -> Unit = {
        TabRow(selectedTabIndex = tab.ordinal) {
            ModelsTab.entries.forEach { t ->
                Tab(selected = tab == t, onClick = { tab = t }, text = { Text(t.label) })
            }
        }
    }
    when (tab) {
        ModelsTab.Cloud -> CloudModelsScreen(cloudState, cloudConnection, cloudSessionId, onBack = onBack, tabs = tabs)
        ModelsTab.Local -> ProvidersScreen(
            providerStore,
            onBack = onBack,
            onOpenConnection = onOpenConnection,
            onOpenLocalModels = onOpenLocalModels,
            tabs = tabs,
            importCard = { existing, onImported ->
                CloudImportCard(cloudState, cloudConnection, cloudSessionId, providerStore, existing, onImported)
            },
        )
    }
}
