package dev.harw.mobile

import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.content.IntentFilter
import android.content.pm.ServiceInfo
import android.net.ConnectivityManager
import android.net.Network
import android.net.NetworkCapabilities
import android.net.NetworkRequest
import android.net.VpnService
import android.os.Build
import android.os.Handler
import android.os.Looper
import android.os.ParcelFileDescriptor
import android.system.OsConstants
import androidx.annotation.Keep
import androidx.annotation.RequiresApi
import androidx.core.app.NotificationCompat
import androidx.core.app.ServiceCompat
import androidx.core.content.ContextCompat
import java.io.File
import java.net.InetAddress
import java.net.InetSocketAddress
import java.security.SecureRandom
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.cancel
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.distinctUntilChanged
import kotlinx.coroutines.flow.map
import kotlinx.coroutines.launch

internal enum class VpnPhase(val text: Int) {
    Off(R.string.vpn_off),
    Waiting(R.string.vpn_wait_network),
    Starting(R.string.vpn_starting),
    TorReady(R.string.vpn_tor_ready),
    Retrying(R.string.vpn_retrying),
    Blocked(R.string.vpn_blocked),
}

@Keep
@RequiresApi(29)
internal class HarwVpnService : VpnService() {
    private val main = Handler(Looper.getMainLooper())
    private val jobs = CoroutineScope(SupervisorJob() + Dispatchers.Main.immediate)
    private val connectivity by lazy { getSystemService(ConnectivityManager::class.java)!! }
    private val notifications by lazy { getSystemService(NotificationManager::class.java)!! }
    private val store by lazy { VpnSettingsStore(this) }
    private val random = SecureRandom()
    private val networks = linkedMapOf<Network, NetworkCapabilities>()
    private val connection get() = (application as HarwApplication).connection

    private var destroyed = false
    private var foreground = false
    private var desired: Pair<String, ResolvedVpnSettings>? = null
    private var applied: Pair<String, ResolvedVpnSettings>? = null
    private var tun: ParcelFileDescriptor? = null
    private var worker: Long = 0
    private var stoppingWorker = false
    @Volatile private var relayNetwork: Network? = null

    private val callback = object : ConnectivityManager.NetworkCallback() {
        override fun onCapabilitiesChanged(network: Network, capabilities: NetworkCapabilities) {
            main.post {
                if (!destroyed) {
                    networks[network] = capabilities
                    updateNetwork()
                }
            }
        }

        override fun onLost(network: Network) {
            main.post {
                if (!destroyed) {
                    networks.remove(network)
                    updateNetwork()
                }
            }
        }
    }

    private val packagesChanged = object : BroadcastReceiver() {
        override fun onReceive(context: Context?, intent: Intent?) {
            refreshProfile()
        }
    }

    override fun onCreate() {
        super.onCreate()
        notifications.createNotificationChannel(
            NotificationChannel(CHANNEL, getString(R.string.vpn_title), NotificationManager.IMPORTANCE_LOW),
        )
        connectivity.registerNetworkCallback(
            NetworkRequest.Builder().addCapability(NetworkCapabilities.NET_CAPABILITY_INTERNET)
                .addCapability(NetworkCapabilities.NET_CAPABILITY_NOT_VPN).build(),
            callback,
        )
        ContextCompat.registerReceiver(
            this, packagesChanged,
            IntentFilter().apply {
                addAction(Intent.ACTION_PACKAGE_ADDED)
                addAction(Intent.ACTION_PACKAGE_REMOVED)
                addAction(Intent.ACTION_PACKAGE_REPLACED)
                addDataScheme("package")
            },
            ContextCompat.RECEIVER_NOT_EXPORTED,
        )
        jobs.launch {
            connection.state.map { it.profile }.distinctUntilChanged().collect { refreshProfile() }
        }
    }

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        val type = if (Build.VERSION.SDK_INT >= 34) ServiceInfo.FOREGROUND_SERVICE_TYPE_SPECIAL_USE else 0
        ServiceCompat.startForeground(this, NOTIFICATION, notification(phase.value), type)
        foreground = true
        if (intent?.action == STOP && !isLockdownEnabled) {
            disableProfile()
            stopSelf()
            return START_NOT_STICKY
        }
        refreshProfile()
        return START_STICKY
    }

    private fun refreshProfile() {
        if (!foreground) return
        val profile = connection.state.value.profile
        val key = profile?.routePreferenceKey()
        val settings = key?.let(store::read)
        if (key == null || settings?.enabled != true || profile?.onion.isNullOrBlank()) {
            desired = null
            stopWorker()
            if (tun != null || isAlwaysOn) publish(VpnPhase.Blocked)
            else if (key != null) stopSelf()
            return
        }
        val resolved = runCatching {
            resolveVpnSettings(settings) { vpnAppIdentity(packageManager, it) }
        }.getOrNull()
        if (resolved == null || !resolved.deviceWide && (resolved.packages.isEmpty() || isLockdownEnabled)) {
            desired = null
            stopWorker()
            publish(VpnPhase.Blocked)
            return
        }
        desired = key to resolved
        if (applied != desired) stopWorker()
        advance()
    }

    private fun bestNetwork(): Network? = networks.entries.maxByOrNull { (_, capabilities) ->
        (if (capabilities.hasCapability(NetworkCapabilities.NET_CAPABILITY_VALIDATED)) 2 else 0) +
            (if (capabilities.hasTransport(NetworkCapabilities.TRANSPORT_WIFI)) 1 else 0)
    }?.key

    private fun updateNetwork() {
        if (worker != 0L && relayNetwork != bestNetwork()) stopWorker()
        advance()
    }

    private fun stopWorker() {
        relayNetwork = null
        if (worker != 0L && !stoppingWorker) {
            stoppingWorker = true
            nativeStop(worker)
        }
    }

    private fun advance() {
        if (destroyed || worker != 0L) return
        val next = desired ?: return
        if (applied != next) {
            val replacement = runCatching { establishTun(next.second) }.getOrNull()
            if (replacement == null) {
                publish(VpnPhase.Blocked)
                return
            }
            tun?.close()
            tun = replacement
            applied = next
        }
        val network = bestNetwork()
        if (network == null) {
            setUnderlyingNetworks(emptyArray())
            publish(VpnPhase.Waiting)
            return
        }
        if (!setUnderlyingNetworks(arrayOf(network))) {
            publish(VpnPhase.Blocked)
            return
        }
        relayNetwork = network
        val configDirectory = File(filesDir, "vpn-arti")
        val descriptor = tun ?: return
        worker = runCatching {
            System.loadLibrary("harw_mobile_ffi")
            nativeStart(
                descriptor.fd, configDirectory.absolutePath, next.second.mode,
                next.second.nativeRules(), random.nextLong(),
            )
        }.getOrDefault(0)
        if (worker == 0L) relayNetwork = null
        publish(if (worker == 0L) VpnPhase.Blocked else VpnPhase.Starting)
    }

    private fun establishTun(settings: ResolvedVpnSettings): ParcelFileDescriptor? {
        val builder = Builder().setSession(getString(R.string.vpn_title)).setMtu(1280).setBlocking(false)
            .addAddress("10.8.0.2", 32).addAddress("fd68:6172:7700::fffc", 128)
            .addRoute("0.0.0.0", 0).addRoute("::", 0)
            .addDnsServer("198.19.255.254").addDnsServer("fd68:6172:7700::fffe")
        if (!settings.deviceWide) settings.packages.forEach(builder::addAllowedApplication)
        if (Build.VERSION.SDK_INT >= 29) builder.setMetered(false)
        return builder.establish()
    }

    @Keep
    private fun protectRelaySocket(fd: Int): Boolean {
        val network = relayNetwork ?: return false
        return runCatching {
            if (!protect(fd)) return false
            ParcelFileDescriptor.fromFd(fd).use { socket -> network.bindSocket(socket.fileDescriptor) }
            relayNetwork == network
        }.getOrDefault(false)
    }

    @Keep
    private fun connectionOwnerUid(source: ByteArray, sourcePort: Int, target: ByteArray, targetPort: Int): Int =
        runCatching {
            connectivity.getConnectionOwnerUid(
                OsConstants.IPPROTO_TCP,
                InetSocketAddress(InetAddress.getByAddress(source), sourcePort),
                InetSocketAddress(InetAddress.getByAddress(target), targetPort),
            )
        }.getOrDefault(-1)

    @Keep
    private fun onNativeState(id: Long, code: Int) {
        main.post {
            if (destroyed || id != worker) return@post
            when (code) {
                1 -> if (!stoppingWorker) publish(VpnPhase.Starting)
                2 -> if (!stoppingWorker) publish(VpnPhase.TorReady)
                5 -> if (!stoppingWorker) publish(VpnPhase.Retrying)
                3, 4, 6 -> {
                    val restart = stoppingWorker
                    worker = 0
                    stoppingWorker = false
                    relayNetwork = null
                    if (restart) advance() else publish(VpnPhase.Blocked)
                }
            }
        }
    }

    private fun notification(state: VpnPhase) = NotificationCompat.Builder(this, CHANNEL)
        .setSmallIcon(R.drawable.ic_harw_notification)
        .setContentTitle(getString(R.string.vpn_title))
        .setContentText(getString(state.text))
        .setContentIntent(PendingIntent.getActivity(
            this, NOTIFICATION, Intent(this, MainActivity::class.java),
            PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT,
        ))
        .setOngoing(true)
        .build()

    private fun publish(state: VpnPhase) {
        mutablePhase.value = state
        notifications.notify(NOTIFICATION, notification(state))
    }

    private fun disableProfile() {
        connection.state.value.profile?.routePreferenceKey()?.let { key ->
            store.write(key, store.read(key).copy(enabled = false))
        }
    }

    override fun onRevoke() {
        main.post {
            disableProfile()
            stopSelf()
        }
    }

    override fun onDestroy() {
        destroyed = true
        jobs.cancel()
        stopWorker()
        tun?.close()
        tun = null
        connectivity.unregisterNetworkCallback(callback)
        unregisterReceiver(packagesChanged)
        mutablePhase.value = VpnPhase.Off
        super.onDestroy()
    }

    private external fun nativeStart(fd: Int, directory: String, mode: Int, rules: ByteArray, seed: Long): Long
    private external fun nativeStop(id: Long)

    companion object {
        private const val CHANNEL = "harw_vpn"
        private const val NOTIFICATION = 5421
        private const val STOP = "dev.harw.mobile.VPN_STOP"
        private val mutablePhase = MutableStateFlow(VpnPhase.Off)
        val phase = mutablePhase.asStateFlow()

        fun refresh(context: Context) {
            ContextCompat.startForegroundService(context, Intent(context, HarwVpnService::class.java))
        }

        fun stop(context: Context) {
            ContextCompat.startForegroundService(context, Intent(context, HarwVpnService::class.java).setAction(STOP))
        }
    }
}
