package dev.harw.mobile

import android.os.Handler
import android.os.Looper
import androidx.camera.core.CameraSelector
import androidx.camera.core.ImageAnalysis
import androidx.camera.core.Preview
import androidx.camera.lifecycle.ProcessCameraProvider
import androidx.camera.view.PreviewView
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.WindowInsets
import androidx.compose.foundation.layout.aspectRatio
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.safeDrawing
import androidx.compose.foundation.layout.windowInsetsPadding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.Button
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberUpdatedState
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.compose.ui.viewinterop.AndroidView
import androidx.compose.ui.window.Dialog
import androidx.compose.ui.window.DialogProperties
import androidx.core.content.ContextCompat
import androidx.lifecycle.compose.LocalLifecycleOwner
import java.util.concurrent.Executors
import java.util.concurrent.atomic.AtomicBoolean
import uniffi.harw_mobile_ffi.MobileException
import uniffi.harw_mobile_ffi.ScannedProfile
import uniffi.harw_mobile_ffi.scanPairingFrame

private const val REJECT_THROTTLE_MS = 2_000L

// Copies the whole Y plane including row padding; size >= (height - 1) * rowStride + width.
internal fun yPlaneBytes(buffer: java.nio.ByteBuffer): ByteArray {
    val view = buffer.duplicate()
    view.rewind()
    val bytes = ByteArray(view.remaining())
    view.get(bytes)
    return bytes
}

@Composable
internal fun QrScanner(modifier: Modifier = Modifier, onScanned: (ScannedProfile) -> Unit, onRejected: (String) -> Unit) {
    val context = LocalContext.current
    val lifecycleOwner = LocalLifecycleOwner.current
    val currentOnScanned by rememberUpdatedState(onScanned)
    val currentOnRejected by rememberUpdatedState(onRejected)
    val previewView = remember { PreviewView(context).apply { scaleType = PreviewView.ScaleType.FILL_CENTER } }

    DisposableEffect(lifecycleOwner) {
        val executor = Executors.newSingleThreadExecutor()
        val mainHandler = Handler(Looper.getMainLooper())
        val done = AtomicBoolean(false)
        var lastRejectAt = 0L
        var provider: ProcessCameraProvider? = null
        var disposed = false
        val providerFuture = ProcessCameraProvider.getInstance(context)

        val analysis = ImageAnalysis.Builder()
            .setBackpressureStrategy(ImageAnalysis.STRATEGY_KEEP_ONLY_LATEST)
            .setOutputImageFormat(ImageAnalysis.OUTPUT_IMAGE_FORMAT_YUV_420_888)
            .build()
        analysis.setAnalyzer(executor) { image ->
            try {
                if (!done.get()) {
                    val plane = image.planes[0]
                    val luma = yPlaneBytes(plane.buffer)
                    val result = try {
                        scanPairingFrame(luma, image.width.toUInt(), image.height.toUInt(), plane.rowStride.toUInt())
                    } catch (e: MobileException.InvalidProfile) {
                        val now = System.currentTimeMillis()
                        if (now - lastRejectAt >= REJECT_THROTTLE_MS) {
                            lastRejectAt = now
                            val detail = e.detail
                            mainHandler.post { if (!done.get()) currentOnRejected(detail) }
                        }
                        null
                    } catch (_: Exception) {
                        null
                    }
                    if (result != null && done.compareAndSet(false, true)) {
                        analysis.clearAnalyzer()
                        mainHandler.post { currentOnScanned(result) }
                    }
                }
            } finally {
                image.close()
            }
        }

        providerFuture.addListener({
            if (disposed) return@addListener
            val p = providerFuture.get()
            provider = p
            val preview = Preview.Builder().build().also { it.setSurfaceProvider(previewView.surfaceProvider) }
            try {
                p.unbindAll()
                p.bindToLifecycle(lifecycleOwner, CameraSelector.DEFAULT_BACK_CAMERA, preview, analysis)
            } catch (_: Exception) {
                currentOnRejected("The camera could not be started. You can import the profile file instead.")
            }
        }, ContextCompat.getMainExecutor(context))

        onDispose {
            disposed = true
            done.set(true)
            analysis.clearAnalyzer()
            provider?.unbindAll()
            executor.shutdown()
        }
    }

    AndroidView(factory = { previewView }, modifier = modifier)
}

private enum class ScanPhase { Scan, Confirm }

@Composable
internal fun QrScanDialog(onDismiss: () -> Unit, onImport: (ScannedProfile) -> Unit) {
    var scanned by remember { mutableStateOf<ScannedProfile?>(null) }
    var lastRejection by remember { mutableStateOf<String?>(null) }
    val phase = if (scanned == null) ScanPhase.Scan else ScanPhase.Confirm
    Dialog(
        onDismissRequest = onDismiss,
        properties = DialogProperties(usePlatformDefaultWidth = false, decorFitsSystemWindows = false),
    ) {
        Surface(Modifier.fillMaxSize()) {
            Column(Modifier.fillMaxSize().windowInsetsPadding(WindowInsets.safeDrawing)) {
                HarwHeader("Pair with server", onBack = onDismiss) {}
                StepRow(phase)
                when (val current = scanned) {
                    null -> Column(
                        Modifier.weight(1f).fillMaxWidth().padding(horizontal = 20.dp, vertical = 17.dp),
                        verticalArrangement = Arrangement.spacedBy(12.dp),
                    ) {
                        Text("Scan the pairing code", style = MaterialTheme.typography.headlineSmall, fontWeight = FontWeight.SemiBold)
                        QrScanner(
                            modifier = Modifier.fillMaxWidth().aspectRatio(1f).clip(RoundedCornerShape(22.dp)),
                            onScanned = { scanned = it; lastRejection = null },
                            onRejected = { lastRejection = it },
                        )
                        StatusMessage("Point the camera at the QR code shown by your harw server.")
                        lastRejection?.let { StatusMessage("Not a valid pairing code: $it", error = true) }
                    }
                    else -> ConfirmContent(
                        scanned = current,
                        modifier = Modifier.weight(1f).fillMaxWidth(),
                        onRescan = { scanned = null },
                        onImport = { onImport(current) },
                    )
                }
            }
        }
    }
}

@Composable
private fun StepRow(phase: ScanPhase) {
    val steps = listOf("1 Scan", "2 Confirm", "3 Import", "4 Connect")
    val active = if (phase == ScanPhase.Scan) 0 else 1
    Row(
        Modifier.fillMaxWidth().padding(horizontal = 20.dp, vertical = 6.dp),
        horizontalArrangement = Arrangement.spacedBy(8.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        steps.forEachIndexed { index, step ->
            if (index > 0) Text("·", color = MaterialTheme.colorScheme.outline, style = MaterialTheme.typography.labelMedium)
            Text(
                step,
                color = if (index == active) MaterialTheme.colorScheme.primary else MaterialTheme.colorScheme.onSurfaceVariant,
                fontWeight = if (index == active) FontWeight.SemiBold else FontWeight.Normal,
                style = MaterialTheme.typography.labelMedium,
            )
        }
    }
}

@Composable
private fun ConfirmContent(scanned: ScannedProfile, modifier: Modifier, onRescan: () -> Unit, onImport: () -> Unit) {
    val profile = scanned.profile
    LazyColumn(
        modifier,
        contentPadding = PaddingValues(horizontal = 20.dp, vertical = 17.dp),
        verticalArrangement = Arrangement.spacedBy(12.dp),
    ) {
        item { Text("Confirm the server", style = MaterialTheme.typography.headlineSmall, fontWeight = FontWeight.SemiBold) }
        item { Field("Label", profile.label) }
        item { Field("Endpoint", profile.endpoint, mono = true) }
        profile.onion?.let { onion -> item { Field("Onion", onion, mono = true) } }
        item { Field("Server node ID", profile.serverNodeId, mono = true) }
        item {
            Column(verticalArrangement = Arrangement.spacedBy(4.dp)) {
                Text("Fingerprint", color = MaterialTheme.colorScheme.primary, style = MaterialTheme.typography.titleSmall)
                Text(
                    scanned.fingerprint,
                    style = MaterialTheme.typography.headlineSmall,
                    fontFamily = FontFamily.Monospace,
                    fontWeight = FontWeight.SemiBold,
                )
            }
        }
        item { StatusMessage("Compare this fingerprint with the one shown on your server.") }
        item {
            Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.spacedBy(10.dp)) {
                OutlinedButton(onClick = onRescan, modifier = Modifier.weight(1f)) { Text("Scan again") }
                Button(onClick = onImport, modifier = Modifier.weight(1f)) { Text("Import") }
            }
        }
    }
}

@Composable
private fun Field(label: String, value: String, mono: Boolean = false) {
    Column(verticalArrangement = Arrangement.spacedBy(2.dp)) {
        Text(label, color = MaterialTheme.colorScheme.onSurfaceVariant, style = MaterialTheme.typography.labelMedium)
        Text(value, style = MaterialTheme.typography.bodyMedium, fontFamily = if (mono) FontFamily.Monospace else null)
    }
}
