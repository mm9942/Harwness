package dev.harw.mobile

import android.content.Context
import android.os.SystemClock
import android.provider.Settings
import android.system.Os
import android.system.OsConstants
import java.io.IOException
import java.security.KeyStore

/** Not wired to an app or profile. All calls, including directory fsync, are worker-thread work. */
internal object AndroidVaultPinStateStore {
    private val durability = VaultPinDirectorySync { directory ->
        val fd = Os.open(
            directory.toString(),
            OsConstants.O_RDONLY or OsConstants.O_NOFOLLOW or OsConstants.O_CLOEXEC or OsConstants.O_NONBLOCK,
            0,
        )
        try {
            if (!OsConstants.S_ISDIR(Os.fstat(fd).st_mode)) throw IOException("Vault PIN sync target is not a directory")
            Os.fsync(fd)
        } finally {
            Os.close(fd)
        }
    }

    fun localProfiles(context: Context) = LocalUiaDevProfiles(privateRoot(context), durability::force)

    fun eraseScope(context: Context, scope: VaultPinScope) {
        VaultPinDiskStateStore.eraseScope(privateRoot(context), scope, durability) {
            val keys = KeyStore.getInstance("AndroidKeyStore").apply { load(null) }
            keys.deleteEntry(AndroidVaultPinMac.alias(scope.vaultId))
            keys.deleteEntry(AndroidVaultBiometricSlot.alias(scope))
        }
    }

    fun open(context: Context, scope: VaultPinScope): VaultPinDiskStateStore =
        VaultPinDiskStateStore.open(privateRoot(context), scope, durability)

    fun reopenAssistantEnrollment(context: Context, scope: VaultPinScope): VaultPinDiskStateStore =
        VaultPinDiskStateStore.reopenAssistantEnrollment(privateRoot(context), scope, durability)

    fun createForNewEnrollment(context: Context): VaultPinDiskStateStore {
        val root = privateRoot(context)
        // Context may have just allocated its no-backup directory. Persist its parent entry too.
        durability.force(root.parent ?: throw IOException("Vault PIN storage is unavailable"))
        return VaultPinDiskStateStore.createNew(root, durability)
    }

    private fun privateRoot(context: Context): java.nio.file.Path {
        val app = context.applicationContext
        if (context.isDeviceProtectedStorage || app.isDeviceProtectedStorage) {
            throw IOException("Vault PIN state requires credential-protected storage")
        }
        return app.noBackupFilesDir.toPath()
    }
}

internal class AndroidVaultPinClock(context: Context) : VaultPinClock {
    private val resolver = context.applicationContext.contentResolver

    override fun read(): VaultPinTime = stableVaultPinTime(
        { Settings.Global.getInt(resolver, Settings.Global.BOOT_COUNT) },
        SystemClock::elapsedRealtime,
    )
}

/** Do not treat an absent/rejected boot identifier as a new clean retry budget. */
internal fun stableVaultPinTime(boot: () -> Int, elapsed: () -> Long): VaultPinTime {
    val before = boot()
    val now = elapsed()
    val after = boot()
    if (before < 0 || now < 0 || before != after) throw IOException("Vault PIN timing is unavailable")
    return VaultPinTime(before.toLong(), now)
}
