package dev.harw.mobile

import android.app.Activity
import android.content.Context
import android.content.Intent
import android.net.VpnService
import android.os.Build
import android.os.PowerManager
import android.provider.Settings
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ExperimentalLayoutApi
import androidx.compose.foundation.layout.FlowRow
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.material3.Button
import androidx.compose.material3.FilterChip
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Switch
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.unit.dp
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import uniffi.harw_mobile_ffi.ConnectionPhase
import uniffi.harw_mobile_ffi.ConnectionRoute

private data class VpnSettingsApp(val name: String, val label: String, val identity: VpnAppIdentity)
private data class VpnSettingsGroup(val uid: Int, val apps: List<VpnSettingsApp>)
private data class VpnConsent(val profile: String, val configuration: VpnProfileSettings)

@Suppress("DEPRECATION")
private fun vpnSettingsApps(context: Context, configured: Set<String>): List<VpnSettingsGroup> {
    val manager = context.packageManager
    val launchable = manager.queryIntentActivities(Intent(Intent.ACTION_MAIN).addCategory(Intent.CATEGORY_LAUNCHER), 0)
        .map { it.activityInfo.packageName }
    return (launchable + configured + context.packageName).distinct().mapNotNull { name ->
        val identity = vpnAppIdentity(manager, name) ?: return@mapNotNull null
        val label = runCatching { manager.getApplicationLabel(manager.getApplicationInfo(name, 0)).toString() }
            .getOrDefault(name)
        VpnSettingsApp(name, label, identity)
    }.groupBy { it.identity.uid }.map { (uid, apps) ->
        VpnSettingsGroup(uid, apps.sortedBy { it.label.lowercase() })
    }.sortedBy { it.apps.first().label.lowercase() }
}

private fun VpnRoute.label(): Int = when (this) {
    VpnRoute.OnionOnly -> R.string.vpn_route_onion
    VpnRoute.InternetAndOnion -> R.string.vpn_route_internet
}

private fun VpnDnsMode.label(): Int = when (this) {
    VpnDnsMode.Tor -> R.string.vpn_dns_tor
    VpnDnsMode.Cloudflare -> R.string.vpn_dns_cloudflare
    VpnDnsMode.Google -> R.string.vpn_dns_google
    VpnDnsMode.AdGuard -> R.string.vpn_dns_adguard
    VpnDnsMode.Quad9 -> R.string.vpn_dns_quad9
    VpnDnsMode.CustomHttps -> R.string.vpn_dns_custom_https
    VpnDnsMode.CustomTls -> R.string.vpn_dns_custom_tls
}

@OptIn(ExperimentalLayoutApi::class)
@Composable
internal fun VpnSettingsScreen(state: WorkspaceState, onBack: () -> Unit) {
    val context = LocalContext.current
    val profile = state.profile
    val key = profile?.routePreferenceKey()
    val store = remember(context) { VpnSettingsStore(context) }
    val phase by HarwVpnService.phase.collectAsState()
    val lockdown by HarwVpnService.lockdown.collectAsState()
    var saved by remember(key) { mutableStateOf(key?.let(store::read) ?: VpnProfileSettings()) }
    var draft by remember(key) { mutableStateOf(saved) }
    var consent by remember { mutableStateOf<VpnConsent?>(null) }
    var error by remember(key) { mutableStateOf<Int?>(null) }
    var groups by remember(key) { mutableStateOf(emptyList<VpnSettingsGroup>()) }
    var filter by remember(key) { mutableStateOf("") }
    val accepted = Build.VERSION.SDK_INT >= 29 && key != null && !profile.onion.isNullOrBlank()
    val configured = remember(saved) { saved.rules.mapTo(mutableSetOf()) { it.packageName } }
    LaunchedEffect(context, key, configured) {
        groups = withContext(Dispatchers.IO) { vpnSettingsApps(context, configured) }
    }
    val identities = remember(groups) { groups.flatMap(VpnSettingsGroup::apps).associate { it.name to it.identity } }

    fun apply(settings: VpnProfileSettings) {
        val current = key ?: return
        val previous = saved
        try {
            store.write(current, settings)
            if (settings.enabled) HarwVpnService.refresh(context)
            else if (previous.enabled || phase != VpnPhase.Off) HarwVpnService.stop(context)
            saved = settings
            draft = settings
            error = null
        } catch (_: Exception) {
            store.write(current, previous)
            error = R.string.vpn_start_failed
        }
    }

    val launcher = rememberLauncherForActivityResult(ActivityResultContracts.StartActivityForResult()) { result ->
        val request = consent
        consent = null
        if (request == null || request.profile != key) return@rememberLauncherForActivityResult
        if (result.resultCode == Activity.RESULT_OK) apply(request.configuration)
        else error = R.string.vpn_permission_declined
    }

    fun submit(settings: VpnProfileSettings) {
        if (settings.dnsMode.endpoint(settings.customDns) == null) {
            error = R.string.vpn_dns_invalid
            return
        }
        if (!settings.enabled) {
            apply(settings)
            return
        }
        val ready = runCatching { resolveVpnSettings(settings, identities::get) }.getOrNull()
        if (ready == null || (!ready.deviceWide && ready.packages.isEmpty())) {
            error = R.string.vpn_select_apps
            return
        }
        if (lockdown && !ready.deviceWide) {
            error = R.string.vpn_lockdown_scope
            return
        }
        val current = key ?: return
        try {
            val request = VpnService.prepare(context)
            if (request == null) apply(settings)
            else {
                consent = VpnConsent(current, settings)
                launcher.launch(request)
            }
        } catch (_: Exception) {
            consent = null
            error = R.string.vpn_start_failed
        }
    }

    Column(Modifier.fillMaxSize()) {
        HarwHeader(title = stringResource(R.string.vpn_title), onBack = onBack) {}
        LazyColumn(
            modifier = Modifier.fillMaxSize().padding(horizontal = 16.dp),
            verticalArrangement = Arrangement.spacedBy(8.dp),
        ) {
            item {
                Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.SpaceBetween, verticalAlignment = Alignment.CenterVertically) {
                    Column(Modifier.weight(1f)) {
                        Text(stringResource(phase.text), style = MaterialTheme.typography.titleMedium)
                        if (phase != VpnPhase.Off) {
                            Text(stringResource(
                                if (state.phase == ConnectionPhase.CONNECTED && state.activeRoute == ConnectionRoute.ONION)
                                    R.string.vpn_hub_connected else R.string.vpn_hub_pending,
                            ), style = MaterialTheme.typography.bodySmall)
                        }
                    }
                    Switch(
                        checked = saved.enabled,
                        enabled = accepted && consent == null && !lockdown,
                        onCheckedChange = { submit(draft.copy(enabled = it)) },
                    )
                }
                if (!accepted) Text(stringResource(
                    if (Build.VERSION.SDK_INT < 29) R.string.vpn_requires_android else R.string.vpn_requires_onion,
                ))
                if (lockdown) Text(stringResource(R.string.vpn_lockdown_scope))
                error?.let { Text(stringResource(it), color = MaterialTheme.colorScheme.error) }
            }

            if (accepted) {
                item {
                    Text(stringResource(R.string.vpn_scope), style = MaterialTheme.typography.titleMedium)
                    FlowRow(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                        FilterChip(
                            selected = !draft.deviceWide,
                            onClick = { draft = draft.copy(deviceWide = false) },
                            enabled = !lockdown,
                            label = { Text(stringResource(R.string.vpn_scope_apps)) },
                        )
                        FilterChip(
                            selected = draft.deviceWide,
                            onClick = { draft = draft.copy(deviceWide = true) },
                            label = { Text(stringResource(R.string.vpn_scope_device)) },
                        )
                    }
                    if (draft.deviceWide) {
                        Text(stringResource(R.string.vpn_default_route))
                        FlowRow(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                            VpnRoute.entries.forEach { route ->
                                FilterChip(
                                    selected = draft.defaultRoute == route,
                                    onClick = { draft = draft.copy(defaultRoute = route) },
                                    label = { Text(stringResource(route.label())) },
                                )
                            }
                        }
                    } else {
                        Text(stringResource(R.string.vpn_scope_apps_detail), style = MaterialTheme.typography.bodySmall)
                    }
                    Text(stringResource(R.string.vpn_dns_title), style = MaterialTheme.typography.titleMedium)
                    FlowRow(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                        VpnDnsMode.entries.forEach { mode ->
                            FilterChip(
                                selected = draft.dnsMode == mode,
                                onClick = { draft = draft.copy(dnsMode = mode) },
                                label = { Text(stringResource(mode.label())) },
                            )
                        }
                    }
                    if (draft.dnsMode == VpnDnsMode.CustomHttps || draft.dnsMode == VpnDnsMode.CustomTls) {
                        OutlinedTextField(
                            value = draft.customDns,
                            onValueChange = { draft = draft.copy(customDns = it.take(512)) },
                            label = { Text(stringResource(
                                if (draft.dnsMode == VpnDnsMode.CustomHttps) R.string.vpn_dns_https_address
                                else R.string.vpn_dns_tls_address,
                            )) },
                            singleLine = true,
                            modifier = Modifier.fillMaxWidth(),
                        )
                    }
                    Text(stringResource(R.string.vpn_dns_scope), style = MaterialTheme.typography.bodySmall)
                    Button(onClick = { submit(draft) }, enabled = draft != saved && consent == null) {
                        Text(stringResource(R.string.vpn_apply))
                    }
                    if (draft.deviceWide || lockdown) {
                        TextButton(onClick = {
                            runCatching { context.startActivity(Intent(Settings.ACTION_VPN_SETTINGS)) }
                        }) { Text(stringResource(R.string.vpn_android_settings)) }
                        Text(stringResource(R.string.vpn_lockdown_help), style = MaterialTheme.typography.bodySmall)
                    }
                    val power = context.getSystemService(PowerManager::class.java)
                    if (power?.isIgnoringBatteryOptimizations(context.packageName) == false && saved.enabled) {
                        TextButton(onClick = {
                            runCatching { context.startActivity(Intent(Settings.ACTION_IGNORE_BATTERY_OPTIMIZATION_SETTINGS)) }
                        }) { Text(stringResource(R.string.vpn_battery_settings)) }
                    }
                    TextButton(onClick = {
                        runCatching { context.startActivity(Intent(Settings.ACTION_WIRELESS_SETTINGS)) }
                    }) { Text(stringResource(R.string.vpn_dns_android_settings)) }
                    Text(stringResource(R.string.vpn_dns_help), style = MaterialTheme.typography.bodySmall)
                    HorizontalDivider(Modifier.padding(top = 8.dp))
                }
                item {
                    OutlinedTextField(
                        value = filter,
                        onValueChange = { filter = it },
                        label = { Text(stringResource(R.string.vpn_app_search)) },
                        singleLine = true,
                        modifier = Modifier.fillMaxWidth(),
                    )
                }
                items(groups.filter { group ->
                    group.apps.any { it.label.contains(filter, true) || it.name.contains(filter, true) }
                }, key = { it.uid }) { group ->
                    val names = group.apps.mapTo(mutableSetOf()) { it.name }
                    val dismissed = draft.rules.filterNot { it.packageName in names }
                    val selected = draft.rules.firstOrNull { rule ->
                        group.apps.any { it.name == rule.packageName && it.identity.signer == rule.signer }
                    }?.route
                    Column {
                        Text(group.apps.joinToString { it.label }, style = MaterialTheme.typography.titleSmall)
                        if (group.apps.size > 1) {
                            Text(stringResource(R.string.vpn_shared_uid), style = MaterialTheme.typography.bodySmall)
                        }
                        FlowRow(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                            FilterChip(
                                selected = selected == null,
                                onClick = { draft = draft.copy(rules = dismissed) },
                                label = { Text(stringResource(if (draft.deviceWide) R.string.vpn_app_default else R.string.vpn_app_outside)) },
                            )
                            VpnRoute.entries.forEach { route ->
                                FilterChip(
                                    selected = selected == route,
                                    onClick = {
                                        draft = draft.copy(rules = dismissed + group.apps.map {
                                            VpnAppRule(it.name, it.identity.signer, route)
                                        })
                                    },
                                    label = { Text(stringResource(route.label())) },
                                )
                            }
                        }
                        HorizontalDivider()
                    }
                }
            }
        }
    }
}
