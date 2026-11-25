package dev.harw.mobile

import android.Manifest
import android.app.Activity
import android.content.Context
import android.content.Intent
import android.content.pm.PackageManager
import android.net.Uri
import android.os.Build
import android.os.Bundle
import android.os.Environment
import android.os.SystemClock
import android.provider.DocumentsContract
import android.util.Log
import androidx.activity.ComponentActivity
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.compose.setContent
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.Button
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.mutableStateListOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.getValue
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.unit.dp
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import java.io.File
import java.io.FileInputStream
import java.io.FileOutputStream
import java.io.RandomAccessFile
import java.nio.channels.FileChannel

/**
 * Debug-only device spike S-DOCS (plan 5.3, contract-w2 section 9). Not wired into the app;
 * start with `adb shell am start -n dev.harw.mobile/.DocsSpikeActivity`. Removed after the device run.
 * Depends only on Android APIs. Log lines: `S-DOCS <step> PASS|FAIL <detail>`.
 */
class DocsSpikeActivity : ComponentActivity() {
    private val lines = mutableStateListOf<String>()
    private var pendingTreeStep: String? = null
    private var treeUri: Uri? = null
    private var treeDocId: String? = null

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        val prefs = getSharedPreferences(SpikePrefs, Context.MODE_PRIVATE)
        prefs.edit().putInt(KeyLaunches, prefs.getInt(KeyLaunches, 0) + 1).apply()
        emit("header", "sdk=${Build.VERSION.SDK_INT} launches=${prefs.getInt(KeyLaunches, 0)}")
        setContent {
            Surface(Modifier.fillMaxSize(), color = MaterialTheme.colorScheme.background) {
                SpikeScreen()
            }
        }
    }

    @Composable
    private fun SpikeScreen() {
        val scope = rememberCoroutineScope()
        var busy by remember { mutableStateOf(false) }
        val treeLauncher = rememberLauncherForActivityResult(ActivityResultContracts.StartActivityForResult()) { r ->
            val step = pendingTreeStep ?: return@rememberLauncherForActivityResult
            pendingTreeStep = null
            val uri = r.data?.data
            if (r.resultCode != Activity.RESULT_OK || uri == null) {
                report(step, false, "tree not selectable or cancelled resultCode=${r.resultCode}")
                return@rememberLauncherForActivityResult
            }
            scope.launch {
                busy = true
                val (ok, detail) = withContext(Dispatchers.IO) { onTreePicked(step, uri) }
                report(step, ok, detail)
                busy = false
            }
        }
        val permLauncher = rememberLauncherForActivityResult(ActivityResultContracts.RequestPermission()) { granted ->
            report("perm", granted, "WRITE_EXTERNAL_STORAGE granted=$granted")
        }

        fun run(step: String, block: suspend () -> Pair<Boolean, String>) {
            if (busy) return
            scope.launch {
                busy = true
                val (ok, detail) = try {
                    withContext(Dispatchers.IO) { block() }
                } catch (t: Throwable) {
                    false to "${t.javaClass.simpleName}: ${t.message}"
                }
                report(step, ok, detail)
                busy = false
            }
        }

        fun pickTree(step: String, startDocId: String) {
            pendingTreeStep = step
            val start = DocumentsContract.buildDocumentUri(ExternalStorageAuthority, startDocId)
            val intent = Intent(Intent.ACTION_OPEN_DOCUMENT_TREE).apply {
                putExtra(DocumentsContract.EXTRA_INITIAL_URI, start)
                addFlags(
                    Intent.FLAG_GRANT_READ_URI_PERMISSION or Intent.FLAG_GRANT_WRITE_URI_PERMISSION or
                        Intent.FLAG_GRANT_PERSISTABLE_URI_PERMISSION,
                )
            }
            try {
                treeLauncher.launch(intent)
            } catch (t: Throwable) {
                pendingTreeStep = null
                report(step, false, "launch failed ${t.javaClass.simpleName}")
            }
        }

        Column(Modifier.padding(16.dp).verticalScroll(rememberScrollState())) {
            Text("S-DOCS (debug spike)", style = MaterialTheme.typography.titleMedium)
            if (Build.VERSION.SDK_INT <= 29) {
                Button(onClick = { permLauncher.launch(Manifest.permission.WRITE_EXTERNAL_STORAGE) }) {
                    Text("Permission (API<=29)")
                }
            }
            Button(enabled = !busy, onClick = { run("D1 mkdir") { d1(SpikeDir) } }) { Text("D1 mkdir + marker") }
            Button(enabled = !busy, onClick = { run("D2 relaunch") { d2() } }) { Text("D2 relaunch (after app restart)") }
            Button(enabled = !busy, onClick = { run("D3 list") { d3(SpikeDir) } }) { Text("D3 list") }
            Button(enabled = !busy, onClick = { run("D4 mmap") { d4(SpikeDir) } }) { Text("D4 mmap (512 MiB)") }
            Button(enabled = !busy, onClick = { run("D5 reinstall") { d5() } }) { Text("D5 reinstall (after uninstall+install)") }
            Button(enabled = !busy, onClick = { pickTree("D6 tree hidden", "primary:Documents/.harw") }) { Text("D6 tree .harw") }
            Button(enabled = !busy, onClick = { pickTree("D7 tree documents", "primary:Documents") }) { Text("D7 tree Documents") }
            Button(enabled = !busy, onClick = { run("D8 copy-replace") { d8() } }) { Text("D8 copy-replace") }
            Button(enabled = !busy, onClick = { run("D9 visible") { d9() } }) { Text("D9 visible fallback") }
            Button(enabled = !busy, onClick = { run("D10 cleanup") { d10(markerToo = false) } }) { Text("D10 cleanup (keep marker)") }
            Button(enabled = !busy, onClick = { run("D10b cleanup marker") { d10(markerToo = true) } }) { Text("D10b delete marker") }
            for (l in lines) Text(l, style = MaterialTheme.typography.bodySmall)
        }
    }

    // ---- reporting ----

    private fun report(step: String, ok: Boolean, detail: String) = emit(step, "${if (ok) "PASS" else "FAIL"} $detail")

    private fun emit(step: String, rest: String) {
        val line = "S-DOCS $step $rest"
        Log.i(SpikeTag, line)
        try {
            File(filesDir, SpikeLogFile).appendText(line + "\n")
        } catch (_: Throwable) {
        }
        runOnUiThread { lines.add(line) }
    }

    // ---- helpers ----

    @Suppress("DEPRECATION")
    private fun documents(): File = Environment.getExternalStoragePublicDirectory(Environment.DIRECTORY_DOCUMENTS)

    private fun spikeDir(rel: String) = File(documents(), rel)

    private fun markerContent() = "ts=${System.currentTimeMillis()}\ninstall=1\n"

    private fun writeLarge(file: File, bytes: Long): Long {
        val t0 = SystemClock.elapsedRealtime()
        val buf = ByteArray(1024 * 1024) { (it % 251 + 1).toByte() }
        FileOutputStream(file).use { out ->
            var left = bytes
            while (left > 0) {
                val n = minOf(left, buf.size.toLong()).toInt()
                out.write(buf, 0, n)
                left -= n
            }
            out.fd.sync()
        }
        return SystemClock.elapsedRealtime() - t0
    }

    /** Maps the file read-only and touches every 4 KiB page. Returns elapsed ms. */
    private fun mmapRead(file: File): Pair<Long, Long> {
        val t0 = SystemClock.elapsedRealtime()
        var sum = 0L
        RandomAccessFile(file, "r").use { raf ->
            val len = raf.length()
            var pos = 0L
            val window = 256L * 1024 * 1024
            while (pos < len) {
                val size = minOf(window, len - pos)
                val map = raf.channel.map(FileChannel.MapMode.READ_ONLY, pos, size)
                var i = 0
                while (i < size) {
                    sum += map.get(i)
                    i += SpikePageBytes
                }
                pos += size
            }
        }
        return (SystemClock.elapsedRealtime() - t0) to sum
    }

    // ---- steps ----

    private fun d1(rel: String): Pair<Boolean, String> {
        val dir = spikeDir(rel)
        val made = dir.mkdirs() || dir.isDirectory
        val content = markerContent()
        File(dir, SpikeMarker).writeText(content)
        val back = File(dir, SpikeMarker).readText()
        val prefs = getSharedPreferences(SpikePrefs, Context.MODE_PRIVATE)
        if (rel == SpikeDir) {
            prefs.edit().putInt(KeyD1Launch, prefs.getInt(KeyLaunches, 0)).putString(KeyD1Content, content).apply()
        }
        return (made && back == content) to "dir=$rel isDir=${dir.isDirectory} readBack=${back == content}"
    }

    private fun d2(): Pair<Boolean, String> {
        val prefs = getSharedPreferences(SpikePrefs, Context.MODE_PRIVATE)
        val d1Launch = prefs.getInt(KeyD1Launch, -1)
        val launches = prefs.getInt(KeyLaunches, 0)
        val expected = prefs.getString(KeyD1Content, null)
        if (d1Launch < 0 || expected == null) return false to "D1 not run in this install"
        if (launches <= d1Launch) return false to "not a relaunch (launches=$launches d1Launch=$d1Launch); end the app and start again"
        val f = File(spikeDir(SpikeDir), SpikeMarker)
        val got = if (f.exists()) f.readText() else null
        return (got == expected) to "launches=$launches exists=${f.exists()} same=${got == expected}"
    }

    private fun d3(rel: String): Pair<Boolean, String> {
        val names = spikeDir(rel).listFiles()?.map { it.name }?.sorted()
        val ok = names != null && SpikeMarker in names && SpikeLargeName in names
        return ok to "dir=$rel files=${names ?: "null"}${if (names != null && SpikeLargeName !in names) " (run D4 first)" else ""}"
    }

    private fun d4(rel: String): Pair<Boolean, String> {
        val dir = spikeDir(rel)
        dir.mkdirs()
        val big = File(dir, SpikeLargeName)
        val priv = File(filesDir, SpikeLargeName)
        if (documents().usableSpace < SpikeLargeBytes + 64L * 1024 * 1024) return false to "not enough free space for ${SpikeLargeBytes shr 20} MiB"
        try {
            val wDocs = writeLarge(big, SpikeLargeBytes)
            val (rDocs, sDocs) = mmapRead(big)
            val wPriv = writeLarge(priv, SpikeLargeBytes)
            val (rPriv, sPriv) = mmapRead(priv)
            val ratio = rDocs.toDouble() / maxOf(rPriv, 1L)
            val warn = if (ratio > 3.0) " WARN ratio>3x" else ""
            return (sDocs == sPriv && big.length() == SpikeLargeBytes) to
                "dir=$rel writeDocsMs=$wDocs mmapDocsMs=$rDocs writePrivMs=$wPriv mmapPrivMs=$rPriv ratio=${"%.2f".format(ratio)}$warn"
        } finally {
            priv.delete()
        }
    }

    /** Run after `adb uninstall` + reinstall: the marker is now foreign to this install. */
    private fun d5(): Pair<Boolean, String> {
        val dir = spikeDir(SpikeDir)
        val marker = File(dir, SpikeMarker)
        val out = StringBuilder()
        out.append("exists=${marker.exists()} canRead=${marker.canRead()}")
        out.append(" read=").append(attempt { FileInputStream(marker).use { it.read() } })
        out.append(" list=").append(attempt { dir.listFiles()?.map { it.name }?.sorted() ?: "null" })
        val same = File(dir, SpikeMarker)
        out.append(" overwrite=").append(attempt { FileOutputStream(same, true).use { it.write(0) }; "ok" })
        out.append(" createNewSame=").append(attempt { same.createNewFile() })
        val own = File(dir, ".harw-d5-own.tmp")
        out.append(" createOwn=").append(attempt { own.writeText("x"); "ok" })
        out.append(" renameOverForeign=").append(attempt { own.renameTo(same) })
        out.append(" deleteForeign=").append(attempt { same.delete() })
        attempt { own.delete() }
        return true to out.toString().replace('\n', ' ')
    }

    private fun attempt(block: () -> Any?): String = try {
        block().toString()
    } catch (t: Throwable) {
        "${t.javaClass.simpleName}(${t.message?.take(60)})"
    }

    private fun onTreePicked(step: String, uri: Uri): Pair<Boolean, String> {
        try {
            contentResolver.takePersistableUriPermission(
                uri,
                Intent.FLAG_GRANT_READ_URI_PERMISSION or Intent.FLAG_GRANT_WRITE_URI_PERMISSION,
            )
        } catch (t: Throwable) {
            Log.i(SpikeTag, "takePersistable failed ${t.javaClass.simpleName}")
        }
        val docId = DocumentsContract.getTreeDocumentId(uri)
        treeUri = uri
        treeDocId = docId
        val rootOk = docId.startsWith("primary:Documents")
        if (step.startsWith("D6")) {
            val kids = listChildren(uri, docId)
            return (rootOk && kids != null) to "treeDocId=$docId children=${kids ?: "null"}"
        }
        val spikeDocId = "primary:Documents/$SpikeDir"
        val spike = DocumentsContract.buildDocumentUriUsingTree(uri, spikeDocId)
        val kids = listChildren(uri, spikeDocId)
        return (rootOk && kids != null) to "treeDocId=$docId spikeUri=${spike != null} spikeChildren=${kids ?: "null"}"
    }

    private fun listChildren(tree: Uri, docId: String): List<String>? = try {
        val children = DocumentsContract.buildChildDocumentsUriUsingTree(tree, docId)
        val names = mutableListOf<String>()
        contentResolver.query(children, arrayOf(DocumentsContract.Document.COLUMN_DISPLAY_NAME), null, null, null)?.use { c ->
            while (c.moveToNext()) names.add(c.getString(0))
        } ?: return null
        names.sorted()
    } catch (t: Throwable) {
        Log.i(SpikeTag, "listChildren failed ${t.javaClass.simpleName}")
        null
    }

    private fun d8(): Pair<Boolean, String> {
        val tree = treeUri ?: return false to "pick a tree with D6 or D7 first"
        val spikeDocId = "primary:Documents/$SpikeDir"
        val kids = listChildren(tree, spikeDocId)
        val doc = DocumentsContract.buildDocumentUriUsingTree(tree, "$spikeDocId/$SpikeMarker")
            ?: return false to "cannot resolve marker"
        val content = contentResolver.openInputStream(doc)?.use { it.readBytes() }
            ?: return false to "cannot read marker via SAF kids=$kids"
        val dir = spikeDir(SpikeDir)
        val tmp = File(dir, ".harw-import-$SpikeMarker")
        tmp.writeBytes(content)
        val deleted = DocumentsContract.deleteDocument(contentResolver, doc)
        val renamed = deleted && tmp.renameTo(File(dir, SpikeMarker))
        val final = File(dir, SpikeMarker)
        val same = final.exists() && final.readBytes().contentEquals(content)
        val cleanupFailed = !deleted && !tmp.delete() && tmp.exists()
        return (deleted && renamed && same) to
            "bytes=${content.size} deleted=$deleted renamed=$renamed same=$same cleanupFailed=$cleanupFailed"
    }

    private fun d9(): Pair<Boolean, String> {
        val a = d1(SpikeVisibleDir)
        val c = d4(SpikeVisibleDir)
        val b = d3(SpikeVisibleDir)
        return (a.first && b.first && c.first) to "d1=${a.first} d3=${b.first} d4=${c.first} (${c.second})"
    }

    private fun d10(markerToo: Boolean): Pair<Boolean, String> {
        var removed = 0
        var failed = 0
        for (rel in listOf(SpikeDir, SpikeVisibleDir)) {
            val dir = spikeDir(rel)
            dir.listFiles()?.forEach { f ->
                if (f.name == SpikeMarker && !markerToo) return@forEach
                if (f.delete()) removed++ else failed++
            }
            if (markerToo) dir.delete()
        }
        File(filesDir, SpikeLargeName).delete()
        return (failed == 0) to "removed=$removed failed=$failed markerToo=$markerToo"
    }

    private companion object {
        const val ExternalStorageAuthority = "com.android.externalstorage.documents"
        const val SpikePrefs = "docs_spike"
        const val KeyLaunches = "launches"
        const val KeyD1Launch = "d1_launch"
        const val KeyD1Content = "d1_content"
    }
}

private const val SpikeTag = "HarwDocsSpike"
private const val SpikeDir = ".harw/spike"
private const val SpikeVisibleDir = "harw/spike"
private const val SpikeMarker = "marker.txt"
private const val SpikeLargeName = "large.gguf"
private const val SpikeLargeBytes = 512L * 1024 * 1024
private const val SpikePageBytes = 4096
private const val SpikeLogFile = "docs-spike.log"
