package dev.harw.mobile

import java.io.File
import javax.crypto.AEADBadTagException
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertThrows
import org.junit.Rule
import org.junit.Test
import org.junit.rules.TemporaryFolder

class LocalBackupTest {
    @get:Rule val files = TemporaryFolder()
    private val password = "local backup passphrase".toCharArray()
    private val provider = "profiles/default/providers/provider.toml"
    private val transcript = "state/local-host/sessions/chat.jsonl"
    private val hostedRecord = "state/local-host/host/hosted-sessions/chat.json"
    private val metadata = "state/local-host/sessions/chat.meta.json"

    @Test
    fun copiesProviderSettingsAndChatsWithMetadata() {
        val source = files.newFolder()
        val target = files.newFolder()
        write(source, provider, "model = \"example\"")
        write(source, transcript, "{\"message\":\"example\"}")
        write(source, metadata, "{}")
        write(source, "profiles/default/chat-uias.json", "{}")
        write(source, "profiles/default/agents/uia-example/system.md", "Example UIA")
        write(source, "profiles/default/agents/uia-example/definition.toml", "title = \"Example\"")
        write(source, "profiles/default/agents/worker/agent.toml", "unrelated")
        write(source, hostedRecord, "{\"session_id\":\"chat\"}")
        write(source, "state/local-host/sessions/chat.lease", "transient")
        write(source, "profiles/default/config.toml", "default_provider = \"provider\"")
        val archive = LocalBackup.encode(source, password)

        assertEquals(8, restore(target, archive))
        assertEquals("model = \"example\"", File(target, provider).readText())
        assertEquals("{\"message\":\"example\"}", File(target, transcript).readText())
        assertEquals("{}", File(target, metadata).readText())
        assertEquals("{\"session_id\":\"chat\"}", File(target, hostedRecord).readText())
        assertFalse(File(target, "state/local-host/sessions/chat.lease").exists())
        assertFalse(File(target, "profiles/default/agents/worker/agent.toml").exists())
        assertEquals("Example UIA", File(target, "profiles/default/agents/uia-example/system.md").readText())
        assertEquals("default_provider = \"provider\"", File(target, "profiles/default/config.toml").readText())

        write(target, transcript, "newer chat")
        assertEquals(5, restore(target, archive))
        assertEquals("newer chat", File(target, transcript).readText())
    }

    @Test
    fun authenticatesBeforeWritingAnyFiles() {
        val source = files.newFolder()
        val target = files.newFolder()
        write(source, provider, "model = \"example\"")
        val archive = LocalBackup.encode(source, password)
        assertThrows(AEADBadTagException::class.java) {
            LocalBackup.restoreInto(target, archive, "wrong password".toCharArray()) { _, _ -> error("unexpected write") }
        }
        archive[archive.lastIndex] = (archive.last().toInt() xor 1).toByte()
        assertThrows(AEADBadTagException::class.java) { restore(target, archive) }
        assertFalse(File(target, provider).exists())
    }

    private fun restore(root: File, bytes: ByteArray): Int =
        LocalBackup.restoreInto(root, bytes, password) { target, content -> target.writeBytes(content) }

    private fun write(root: File, path: String, content: String) {
        File(root, path).apply {
            parentFile!!.mkdirs()
            writeText(content)
        }
    }
}
