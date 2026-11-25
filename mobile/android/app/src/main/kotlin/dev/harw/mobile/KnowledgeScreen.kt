package dev.harw.mobile

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.ui.Modifier
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.unit.dp
import uniffi.harw_mobile_ffi.ConnectionPhase

/** Profile-level information only. A generic manage grant is not a Knowledge-store capability. */
private val KnowledgeSections = listOf(
    R.string.knowledge_memory to R.string.knowledge_memory_scope,
    R.string.knowledge_diary to R.string.knowledge_diary_scope,
    R.string.knowledge_session_workbench to R.string.knowledge_session_workbench_scope,
    R.string.knowledge_project_workbench to R.string.knowledge_project_workbench_scope,
    R.string.knowledge_goals to R.string.knowledge_goals_scope,
    R.string.knowledge_plan to R.string.knowledge_plan_scope,
)

@Composable
internal fun KnowledgeScreen(state: WorkspaceState, connection: HarwConnection, onBack: () -> Unit) {
    LaunchedEffect(connection, state.online, state.generation) {
        if (state.online) connection.refreshManageSupport()
    }
    val availability = knowledgeAvailability(
        hasCloudProfile = state.profile != null,
        online = state.online,
        connecting = state.setupPending || state.phase == ConnectionPhase.CONNECTING ||
            state.phase == ConnectionPhase.RECONNECTING,
        support = state.manage.support?.let { KnowledgeCloudSupport(it.manage, it.observe) },
    )
    Column(Modifier.fillMaxSize()) {
        HarwHeader(title = stringResource(R.string.settings_hub_knowledge), onBack = onBack) {}
        Column(
            Modifier.fillMaxSize().verticalScroll(rememberScrollState()).padding(12.dp),
            verticalArrangement = Arrangement.spacedBy(12.dp),
        ) {
            Text(stringResource(R.string.knowledge_cloud_profile), style = MaterialTheme.typography.titleSmall)
            StatusMessage(
                stringResource(
                    when (availability) {
                        KnowledgeAvailability.NoCloudProfile -> R.string.knowledge_no_profile
                        KnowledgeAvailability.Offline -> R.string.knowledge_offline
                        KnowledgeAvailability.Checking -> R.string.knowledge_checking
                        KnowledgeAvailability.ManagementUnavailable -> R.string.knowledge_no_manage
                        KnowledgeAvailability.ObserveDenied -> R.string.knowledge_no_observe
                        KnowledgeAvailability.NotOffered -> R.string.knowledge_no_store
                    },
                ),
            )
            Text(stringResource(R.string.knowledge_no_scope), style = MaterialTheme.typography.bodyMedium)
            KnowledgeSections.forEach { (label, detail) ->
                Column(verticalArrangement = Arrangement.spacedBy(2.dp)) {
                    Text(stringResource(label), style = MaterialTheme.typography.titleMedium)
                    Text(
                        stringResource(detail),
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                }
            }
        }
    }
}
