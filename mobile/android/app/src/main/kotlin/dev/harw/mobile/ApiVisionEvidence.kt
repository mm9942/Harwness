package dev.harw.mobile

import android.content.Context
import java.security.MessageDigest
import java.util.Base64
import uniffi.harw_mobile_ffi.ApiVisionChoice

private const val VisionEvidencePreferencesName = "harw_api_vision_evidence"
private const val MaxVisionEvidenceRoutes = 12
private const val RequiredCleanBatches = 3
private const val MaxVisionEvidenceAgeMs = 7L * 24 * 60 * 60 * 1000
private val visionEvidenceLock = Any()

/** Length-prefix each field. A delimiter within a model name cannot alias a different route. */
internal fun visionPolicyDigest(vararg fields: String): String {
    val digest = MessageDigest.getInstance("SHA-256")
    for (field in fields) {
        val bytes = field.encodeToByteArray()
        val size = bytes.size
        digest.update(byteArrayOf((size shr 24).toByte(), (size shr 16).toByte(), (size shr 8).toByte(), size.toByte()))
        digest.update(bytes)
    }
    return Base64.getUrlEncoder().withoutPadding().encodeToString(digest.digest())
}

internal class ApiVisionBatchEvidence(private val configured: Int, private val count: Int) {
    private var peakAdmitted = 0
    private var providerDampened = false

    fun observe(progress: ApiVisionBatchProgress) {
        peakAdmitted = maxOf(peakAdmitted, progress.active)
        providerDampened = providerDampened || progress.pause != null ||
            progress.coolingDown || progress.effective != configured
    }

    /** An actual qualified observation, not a benchmark or promise of provider capacity. */
    fun qualifiedLimit(captions: Int): Int? =
        configured.takeIf { !providerDampened && peakAdmitted >= configured && count >= configured && captions == count }

    // A dormant or tiny clean batch neither validates nor invalidates the previous sample.
    fun invalidates(captions: Int): Boolean = providerDampened || captions != count
}

internal data class ApiVisionRecommendation(val parallelism: Int, val cleanBatches: Int, val updatedAt: Long)

internal fun evolveVisionRecommendation(
    previous: ApiVisionRecommendation?,
    qualified: Int?,
    invalidates: Boolean,
    updatedAt: Long,
): ApiVisionRecommendation? {
    if (invalidates) return null
    if (qualified == null) return previous
    if (qualified !in ApiVisionMinParallelism..ApiVisionMaxRefs) return null
    val count = if (previous != null && previous.parallelism == qualified) previous.cleanBatches + 1 else 1
    return ApiVisionRecommendation(qualified, count.coerceAtMost(RequiredCleanBatches), updatedAt)
}

internal class ApiVisionEvidenceStore(context: Context) {
    private val prefs = context.applicationContext.getSharedPreferences(VisionEvidencePreferencesName, Context.MODE_PRIVATE)

    private fun key(scope: String, choice: ApiVisionChoice) =
        visionPolicyDigest(scope, choice.routeId, choice.modelId, choice.endpoint, choice.stream.toString())

    private fun decode(value: Any?): ApiVisionRecommendation? {
        val fields = (value as? String)?.takeIf { it.length <= 32 }?.split(':') ?: return null
        if (fields.size != 3) return null
        val limit = fields[0].toIntOrNull()?.takeIf { it in ApiVisionMinParallelism..ApiVisionMaxRefs } ?: return null
        val samples = fields[1].toIntOrNull()?.takeIf { it in 1..RequiredCleanBatches } ?: return null
        val timestamp = fields[2].toLongOrNull() ?: return null
        return ApiVisionRecommendation(limit, samples, timestamp)
    }

    private fun ApiVisionRecommendation.isFresh(now: Long): Boolean =
        updatedAt > 0L && updatedAt <= now && now - updatedAt <= MaxVisionEvidenceAgeMs

    fun recommendation(scope: String, choice: ApiVisionChoice): Int? =
        decode(prefs.all[key(scope, choice)])?.takeIf {
            it.cleanBatches == RequiredCleanBatches && it.isFresh(System.currentTimeMillis())
        }?.parallelism

    fun record(scope: String, choice: ApiVisionChoice, observed: ApiVisionBatchEvidence, captions: Int) =
        synchronized(visionEvidenceLock) {
            val key = key(scope, choice)
            val all = prefs.all
            val now = System.currentTimeMillis()
            val old = decode(all[key])
            val fresh = old?.takeIf { it.isFresh(now) }
            val next = evolveVisionRecommendation(fresh, observed.qualifiedLimit(captions), observed.invalidates(captions), now)
            if (old == next) return@synchronized
            val edit = prefs.edit()
            if (next == null) {
                edit.remove(key)
            } else {
                val others = all.filterKeys { it != key }
                others.entries.sortedBy { decode(it.value)?.updatedAt ?: 0L }
                    .take((others.size - MaxVisionEvidenceRoutes + 1).coerceAtLeast(0))
                    .forEach { edit.remove(it.key) }
                edit.putString(key, "${next.parallelism}:${next.cleanBatches}:${next.updatedAt}")
            }
            edit.apply()
        }
}
