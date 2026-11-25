package dev.harw.mobile

import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.selection.selectable
import androidx.compose.foundation.selection.selectableGroup
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.RadioButton
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.unit.dp

@Composable
internal fun HarwLanguageSetting() {
    val context = LocalContext.current
    var current by remember(context) { mutableStateOf(harwUiLanguagePreference(context)) }
    var pending by remember(current) { mutableStateOf<HarwUiLanguagePreference?>(null) }
    val options: List<HarwUiLanguagePreference> = remember {
        listOf(HarwUiLanguagePreference.FollowDevice) +
            HarwUiLanguage.entries.map { HarwUiLanguagePreference.Explicit(it) }
    }

    Column(
        Modifier.fillMaxWidth()
            .clickable(role = Role.Button, onClick = { pending = current })
            .padding(horizontal = 16.dp, vertical = 8.dp),
    ) {
        Text(stringResource(R.string.harw_ui_language), style = MaterialTheme.typography.bodyLarge)
        Text(
            when (val preference = current) {
                HarwUiLanguagePreference.FollowDevice ->
                    stringResource(R.string.harw_ui_language_follow_device) + " · " + effectiveHarwUiLanguage(context).autonym
                is HarwUiLanguagePreference.Explicit -> preference.language.autonym
            },
            style = MaterialTheme.typography.bodySmall,
        )
    }

    val selected = pending ?: return
    AlertDialog(
        onDismissRequest = { pending = null },
        title = { Text(stringResource(R.string.harw_ui_language)) },
        text = {
            Column {
                Text(stringResource(R.string.harw_ui_language_summary), style = MaterialTheme.typography.bodySmall)
                LazyColumn(Modifier.heightIn(max = 380.dp).selectableGroup()) {
                    items(options, key = { choice ->
                        when (choice) {
                            HarwUiLanguagePreference.FollowDevice -> "follow_device"
                            is HarwUiLanguagePreference.Explicit -> choice.language.code
                        }
                    }) { item ->
                        Row(
                            Modifier.fillMaxWidth().selectable(
                                selected = item == selected,
                                role = Role.RadioButton,
                                onClick = { pending = item },
                            ).padding(vertical = 10.dp),
                            verticalAlignment = Alignment.CenterVertically,
                        ) {
                            RadioButton(selected = item == selected, onClick = null)
                            Text(
                                when (item) {
                                    HarwUiLanguagePreference.FollowDevice -> stringResource(R.string.harw_ui_language_follow_device)
                                    is HarwUiLanguagePreference.Explicit -> item.language.autonym
                                },
                                Modifier.padding(start = 12.dp),
                            )
                        }
                    }
                }
            }
        },
        confirmButton = {
            TextButton(onClick = {
                pending = null
                setHarwUiLanguagePreference(context, selected)
                current = selected
            }) { Text(stringResource(R.string.harw_ui_language_apply)) }
        },
        dismissButton = {
            TextButton(onClick = { pending = null }) {
                Text(stringResource(R.string.harw_ui_language_cancel))
            }
        },
    )
}
