package dev.harw.mobile

import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.joinAll
import kotlinx.coroutines.withContext
import kotlinx.coroutines.withTimeoutOrNull

internal object LocalUiaVaultReset {
    private val guard = Any()
    private val sessions = mutableSetOf<LocalUiaDevSession>()
    private var resetting: LocalUiaProfile? = null

    fun open(app: HarwApplication, profile: LocalUiaProfile, access: VaultAccess = VaultAccess.Vault): LocalUiaDevSession? =
        synchronized(guard) {
            if (resetting == profile) return null
            LocalUiaDevSession(app, profile, access).also { session ->
                sessions.add(session)
                session.lifetime.invokeOnCompletion { synchronized(guard) { sessions.remove(session) } }
            }
        }

    suspend fun erase(app: HarwApplication, profile: LocalUiaProfile): Boolean {
        val active = synchronized(guard) {
            if (resetting != null || readAppPinProfile(app) == profile) return false
            resetting = profile
            sessions.filter { it.profile == profile }
        }
        return try {
            if (withContext(Dispatchers.IO) { LocalUiaDevCapability.current(app) } != profile) return false
            active.forEach { it.close() }
            if (withTimeoutOrNull(10_000L) {
                    PrivateAssistantAttemptAdmission.stopForVaultReset(profile)
                    active.map { it.lifetime }.joinAll()
                    true
                } != true
            ) return false
            withContext(Dispatchers.IO) {
                if (LocalUiaDevCapability.current(app) != profile) return@withContext false
                AndroidVaultPinStateStore.localProfiles(app).eraseVault(profile) {
                    AndroidVaultPinStateStore.eraseScope(app, it)
                }
                true
            }
        } catch (cancelled: CancellationException) {
            throw cancelled
        } catch (_: Exception) {
            false
        } finally {
            synchronized(guard) { resetting = null }
        }
    }

    fun setAppPin(app: HarwApplication, profile: LocalUiaProfile?, authorized: LocalUiaProfile): Boolean =
        synchronized(guard) {
            val current = readAppPinProfile(app)
            if (resetting == authorized || (current != null && current != authorized) ||
                (profile != null && profile != authorized)
            ) return false
            app.getSharedPreferences(PinPrefs, 0).edit().putString(AppPinProfile, profile?.key).commit()
        }
}
