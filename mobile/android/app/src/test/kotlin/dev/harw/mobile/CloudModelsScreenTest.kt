package dev.harw.mobile

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import uniffi.harw_mobile_ffi.ManageAuthKind
import uniffi.harw_mobile_ffi.ManageKnownAuth
import uniffi.harw_mobile_ffi.ManageSupport

class CloudModelsScreenTest {
    private val handle = "0123456789abcdef0123456789abcdef"
    private val fakeSecret = "sk-test-FAKE-0000000000000000"

    private fun support(
        manage: Boolean = true, stepup: Boolean = true, control: Boolean = true,
    ) = ManageSupport(
        manage = manage, stepup = stepup, observe = true, steer = true, approve = true, control = control,
    )

    private fun known(auth: ManageKnownAuth) = ProviderAddRequest(
        known = true, catalogId = "anthropic", auth = auth, name = "anthropic-key", keyName = "anthropic-key",
        api = null, baseUrl = null, authHeader = null,
    )

    private fun custom(
        auth: ManageKnownAuth,
        api: String? = "openai-chat",
        url: String? = "https://llm.example.test/v1",
        header: String? = null,
    ) = ProviderAddRequest(
        known = false, catalogId = null, auth = auth, name = "eigen", keyName = "eigen-key",
        api = api, baseUrl = url, authHeader = header,
    )

    @Test
    fun modelsSetLineWithAndWithoutEffort() {
        assertEquals("/models set uia p/m high", modelsSetLine("uia", "p", "m", "high"))
        val plain = modelsSetLine("uia", "p", "m", null)
        assertEquals("/models set uia p/m", plain)
        assertFalse(plain!!.endsWith(" "))
        assertNull(modelsSetLine("uia", "p", "a b", null))
        assertNull(modelsSetLine("uia", "p", "m", "extreme"))
    }

    @Test
    fun modelSwitchAndScanLines() {
        assertEquals("/model switch p/m", modelSwitchLine("p", "m"))
        assertNull(modelSwitchLine("p", "a b"))
        assertNull(modelSwitchLine("p/q", "m"))
        assertEquals("/models scan", modelsScanLine(null))
        assertEquals("/models scan p", modelsScanLine("p"))
        assertNull(modelsScanLine("a b"))
    }

    @Test
    fun providerAddKnownLines() {
        assertEquals(
            "/provider add anthropic --auth api-key --name anthropic-key --key $handle",
            providerAddLine(known(ManageKnownAuth.API_KEY), handle),
        )
        val oauth = providerAddLine(known(ManageKnownAuth.OAUTH), null)
        assertEquals("/provider add anthropic --auth oauth --name anthropic-key", oauth)
        assertFalse(oauth!!.contains("--key"))
    }

    @Test
    fun providerAddCustomLines() {
        assertEquals(
            "/provider add-custom eigen --api openai-chat --base-url https://llm.example.test/v1 " +
                "--key $handle --auth-header bearer",
            providerAddLine(custom(ManageKnownAuth.API_KEY, header = "bearer"), handle),
        )
        val none = providerAddLine(custom(ManageKnownAuth.NONE), null)
        assertNotNull(none)
        assertTrue(none!!.endsWith("--no-auth"))
        assertNull(providerAddLine(custom(ManageKnownAuth.API_KEY), null))
        assertNull(providerAddLine(custom(ManageKnownAuth.NONE, api = "grpc"), null))
        assertNull(providerAddLine(custom(ManageKnownAuth.API_KEY), "abc def"))
        assertNull(providerAddLine(custom(ManageKnownAuth.NONE, url = "https://llm.example.test/" + "a".repeat(600)), null))
    }

    @Test
    fun customUrlRules() {
        assertNull(customBaseUrlProblem("https://llm.example.test/v1"))
        listOf(
            "http://llm.example.test/v1",
            "https://user:pw@llm.example.test/v1",
            "https://llm.example.test/v1?k=v",
            "https://llm.example.test/v1#f",
            "https://localhost/v1",
            "https://10.0.0.1/v1",
            "https://[::1]/",
            "https:///v1",
        ).forEach { assertNotNull(it, customBaseUrlProblem(it)) }
    }

    @Test
    fun writeBlockedWithoutControl() {
        assertEquals("Ändern braucht Profil Voll", manageWriteBlock(support(control = false)))
        assertNull(manageWriteBlock(support()))
        assertEquals(NO_MANAGE_TEXT, manageWriteBlock(null))
        assertNotNull(manageWriteBlock(support(stepup = false)))
    }

    @Test
    fun authLabelsNeverShowValues() {
        assertEquals("Secret-Store anthropic-key", authLabel(ManageAuthKind.SECRETS, "anthropic-key", true))
        assertEquals("Secret-Store anthropic-key (fehlt)", authLabel(ManageAuthKind.SECRETS, "anthropic-key", false))
    }

    @Test
    fun noSecretInLines() {
        // Nur der Griff darf in Zeilen stehen, nie der Klartext-Wert.
        val lines = listOf(
            providerAddLine(known(ManageKnownAuth.API_KEY), handle),
            providerAddLine(known(ManageKnownAuth.OAUTH), null),
            providerAddLine(custom(ManageKnownAuth.API_KEY, header = "x-api-key"), handle),
            providerAddLine(custom(ManageKnownAuth.NONE), null),
            modelsSetLine("uia", "p", "m", "high"),
            modelSwitchLine("p", "m"),
            authLabel(ManageAuthKind.SECRETS, "anthropic-key", true),
        )
        lines.forEach {
            assertNotNull(it)
            assertFalse(it!!.contains(fakeSecret))
        }
        assertTrue(lines[0]!!.contains(handle))
    }
}
