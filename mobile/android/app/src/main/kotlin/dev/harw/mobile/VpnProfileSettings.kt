package dev.harw.mobile

import android.content.Context
import android.content.pm.PackageManager
import java.nio.ByteBuffer
import java.security.MessageDigest
import org.json.JSONArray
import org.json.JSONObject

internal enum class VpnRoute(val code: Int) { OnionOnly(1), InternetAndOnion(2) }

internal data class VpnAppRule(val packageName: String, val signer: String, val route: VpnRoute)

internal data class VpnProfileSettings(
    val enabled: Boolean = false,
    val deviceWide: Boolean = false,
    val defaultRoute: VpnRoute = VpnRoute.OnionOnly,
    val rules: List<VpnAppRule> = emptyList(),
)

internal class VpnSettingsStore(context: Context) {
    private val preferences = context.getSharedPreferences("harw_vpn", Context.MODE_PRIVATE)

    fun read(profileKey: String): VpnProfileSettings {
        val value = preferences.getString(profileKey, null) ?: return VpnProfileSettings()
        return runCatching {
            val json = JSONObject(value)
            val rules = json.getJSONArray("apps")
            check(rules.length() <= MAX_RULES)
            VpnProfileSettings(
                enabled = json.getBoolean("enabled"),
                deviceWide = json.getBoolean("deviceWide"),
                defaultRoute = VpnRoute.valueOf(json.getString("default")),
                rules = (0 until rules.length()).map { index ->
                    val entry = rules.getJSONObject(index)
                    VpnAppRule(
                        entry.getString("package"),
                        entry.getString("signer"),
                        VpnRoute.valueOf(entry.getString("route")),
                    )
                },
            )
        }.getOrDefault(VpnProfileSettings())
    }

    fun write(profileKey: String, settings: VpnProfileSettings) {
        require(settings.rules.size <= MAX_RULES)
        val rules = JSONArray()
        settings.rules.forEach { entry ->
            rules.put(JSONObject().put("package", entry.packageName)
                .put("signer", entry.signer).put("route", entry.route.name))
        }
        val value = JSONObject().put("enabled", settings.enabled).put("deviceWide", settings.deviceWide)
            .put("default", settings.defaultRoute.name).put("apps", rules)
        preferences.edit().putString(profileKey, value.toString()).apply()
    }

    companion object {
        private const val MAX_RULES = 4096
    }
}

internal data class VpnAppIdentity(val uid: Int, val signer: String)

@Suppress("DEPRECATION")
internal fun vpnAppIdentity(packages: PackageManager, packageName: String): VpnAppIdentity? = runCatching {
    val info = packages.getPackageInfo(packageName, PackageManager.GET_SIGNING_CERTIFICATES)
    val signers = checkNotNull(info.signingInfo?.apkContentsSigners).map { signer ->
        MessageDigest.getInstance("SHA-256").digest(signer.toByteArray()).joinToString("") { "%02x".format(it) }
    }
    check(signers.isNotEmpty())
    VpnAppIdentity(checkNotNull(info.applicationInfo).uid, signers.sorted().joinToString(","))
}.getOrNull()

internal data class ResolvedVpnSettings(
    val deviceWide: Boolean,
    val mode: Int,
    val packages: Set<String>,
    val rules: Map<Int, VpnRoute>,
) {
    fun nativeRules(): ByteArray {
        val frame = ByteBuffer.allocate(rules.size * 5)
        rules.forEach { (uid, route) -> frame.putInt(uid).put(route.code.toByte()) }
        return frame.array()
    }
}

internal fun resolveVpnSettings(
    settings: VpnProfileSettings,
    identity: (String) -> VpnAppIdentity?,
): ResolvedVpnSettings {
    val packages = mutableSetOf<String>()
    val rules = mutableMapOf<Int, VpnRoute>()
    settings.rules.forEach { rule ->
        val app = identity(rule.packageName)
        if (app == null || app.signer != rule.signer) return@forEach
        val previous = rules.putIfAbsent(app.uid, rule.route)
        require(previous == null || previous == rule.route) { "Conflicting policies for one Android UID" }
        packages.add(rule.packageName)
    }
    return ResolvedVpnSettings(
        settings.deviceWide,
        if (settings.deviceWide) settings.defaultRoute.code else 0,
        packages,
        rules,
    )
}
