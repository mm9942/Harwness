package dev.harw.mobile

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Test
import uniffi.harw_mobile_ffi.ManageEffect
import uniffi.harw_mobile_ffi.ManageRunResult
import uniffi.harw_mobile_ffi.ManageStepUp

class HarwConnectionManageTest {
    private fun challenge() = ManageStepUp(
        stepupId = "su-1",
        action = "models.set",
        target = "uia",
        text = "harw step-up v1\naktion: models.set",
        nonce = "n",
        hash = "h",
        expiresAt = "2026-11-22T10:00:00Z",
        validForMs = 60_000uL,
        lines = emptyList(),
        address = null,
        keyName = null,
    )

    private fun result(effect: ManageEffect, stepup: ManageStepUp?) = ManageRunResult(
        ok = effect != ManageEffect.REFUSED,
        text = "t",
        dataJson = null,
        dataTruncated = false,
        effect = effect,
        reasonCode = null,
        stepup = stepup,
    )

    @Test
    fun stepupRequiredOpensDialogNotCard() {
        val next = applyManageResult(ManageState(running = "/x"), "/x", result(ManageEffect.STEPUP_REQUIRED, challenge()), 4242L)
        assertNotNull(next.stepUp)
        assertNull(next.card)
        assertNull(next.running)
        assertEquals(4242L, next.stepUpReceivedAt)
    }

    @Test
    fun refusedBecomesCard() {
        val next = applyManageResult(ManageState(), "/stop", result(ManageEffect.REFUSED, null), 1L)
        assertNull(next.stepUp)
        assertEquals("/stop", next.card?.line)
        assertEquals(ManageEffect.REFUSED, next.card?.result?.effect)
    }

    @Test
    fun stepupWithoutChallengeBecomesCard() {
        val next = applyManageResult(ManageState(), "/models set", result(ManageEffect.STEPUP_REQUIRED, null), 1L)
        assertNull(next.stepUp)
        assertNotNull(next.card)
    }
}
