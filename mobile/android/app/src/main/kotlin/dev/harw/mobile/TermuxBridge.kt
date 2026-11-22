package dev.harw.mobile

import android.app.Activity
import android.app.PendingIntent
import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.content.IntentFilter
import android.content.pm.PackageManager
import android.os.Build
import android.os.Bundle
import java.util.UUID
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicInteger
import java.util.concurrent.atomic.AtomicReference
import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.withTimeoutOrNull
import uniffi.harw_mobile_ffi.DeviceException
import uniffi.harw_mobile_ffi.ShellExecutor
import uniffi.harw_mobile_ffi.ShellOutput
import uniffi.harw_mobile_ffi.ShellRequest

internal const val SHELL_MAX_ARGS = 64
internal const val SHELL_MAX_ARG_BYTES = 4096
internal const val SHELL_DEFAULT_TIMEOUT_MS = 30_000L
internal const val SHELL_MAX_TIMEOUT_MS = 120_000L
internal const val SHELL_MAX_OUTPUT_BYTES = 65_536
internal const val TERMUX_HOME_PREFIX = "/data/data/com.termux/files/"

private const val TERMUX_PACKAGE = "com.termux"
private const val TERMUX_PERMISSION = "com.termux.permission.RUN_COMMAND"
private const val TERMUX_SERVICE = "com.termux.app.RunCommandService"
private const val TERMUX_ACTION = "com.termux.RUN_COMMAND"
private const val TERMUX_BASH = "/data/data/com.termux/files/usr/bin/bash"
private const val TERMUX_HOME = "/data/data/com.termux/files/home/"
private const val TERMUX_NO_ERROR = -1
private const val APPROVAL_TIMEOUT_MS = 60_000L
private const val DEVICE_TOOLS_PREFS = "harw_device_tools"
private const val TERMUX_ENABLED_KEY = "termux_shell_enabled"
private const val SHELL_MAX_CWD_CHARS = 1024

/** Asks the person to approve one command before it leaves the app. */
internal fun interface CommandApprover {
    suspend fun approve(argv: List<String>, cwd: String?): Boolean
}

internal fun checkShellArgv(argv: List<String>, cwd: String?) {
    if (argv.isEmpty()) throw DeviceException.Invalid("empty command")
    if (argv.size > SHELL_MAX_ARGS) throw DeviceException.Invalid("too many arguments")
    if (argv[0].isEmpty()) throw DeviceException.Invalid("invalid argument")
    for (arg in argv) {
        if (arg.indexOf('\u0000') >= 0 || arg.toByteArray(Charsets.UTF_8).size > SHELL_MAX_ARG_BYTES) {
            throw DeviceException.Invalid("invalid argument")
        }
    }
    if (cwd == null) return
    if (cwd.startsWith(TERMUX_HOME_PREFIX)) {
        val outside =
            cwd.length > SHELL_MAX_CWD_CHARS ||
                cwd.indexOf('\u0000') >= 0 ||
                cwd.split('/').any { it == ".." }
        if (outside) throw DeviceException.InvalidPath("working directory outside Termux home")
        return
    }
    normalizeSafPath(cwd)
}

internal fun clampTimeout(ms: Long): Long =
    when {
        ms <= 0 -> SHELL_DEFAULT_TIMEOUT_MS
        ms > SHELL_MAX_TIMEOUT_MS -> SHELL_MAX_TIMEOUT_MS
        else -> ms
    }

internal fun clampOutput(n: Int): Int = if (n <= 0 || n > SHELL_MAX_OUTPUT_BYTES) SHELL_MAX_OUTPUT_BYTES else n

/** Cuts [text] to at most [maxBytes] UTF-8 bytes without splitting a code point. */
internal fun capOutput(text: String, maxBytes: Int): Pair<String, Boolean> {
    val budget = maxBytes.coerceAtLeast(0)
    var used = 0
    var index = 0
    while (index < text.length) {
        val cp = text.codePointAt(index)
        val size =
            when {
                cp < 0x80 -> 1
                cp < 0x800 -> 2
                cp < 0x10000 -> 3
                else -> 4
            }
        if (used + size > budget) return text.substring(0, index) to true
        used += size
        index += Character.charCount(cp)
    }
    return text to false
}

private fun utf8Size(text: String): Int = text.toByteArray(Charsets.UTF_8).size

/**
 * Runs commands in Termux through its RUN_COMMAND service.
 *
 * The manifest permission com.termux.permission.RUN_COMMAND (and the Termux package query) is not
 * declared yet, so [available] is false on a device until the integration round adds them.
 * Argv, working directory and output are never logged.
 */
internal class TermuxBridge(
    private val context: Context,
    private val approver: CommandApprover,
) : ShellExecutor {
    private val prefs = context.getSharedPreferences(DEVICE_TOOLS_PREFS, Context.MODE_PRIVATE)
    private val requestCodes = AtomicInteger(1)

    fun setEnabled(enabled: Boolean) {
        prefs.edit().putBoolean(TERMUX_ENABLED_KEY, enabled).apply()
    }

    fun isEnabled(): Boolean = prefs.getBoolean(TERMUX_ENABLED_KEY, false)

    override fun available(): Boolean = isEnabled() && termuxInstalled() && permissionGranted()

    override fun run(request: ShellRequest): ShellOutput {
        if (!isEnabled()) throw DeviceException.Disabled()
        if (!termuxInstalled()) throw DeviceException.Unavailable("Termux is not installed")
        if (!permissionGranted()) throw DeviceException.Unavailable("Termux RUN_COMMAND permission missing")
        checkShellArgv(request.argv, request.cwd)
        // The Rust core calls this from a blocking worker thread, never the main thread, so
        // blocking here on the approval dialog is safe.
        val approved = runBlocking { withTimeoutOrNull(APPROVAL_TIMEOUT_MS) { approver.approve(request.argv, request.cwd) } }
        if (approved != true) throw DeviceException.Declined()
        return execute(request)
    }

    /**
     * Opens an interactive Termux terminal session running [shellCommand] (fixed launchers such
     * as "exec bash -l", "exec claude", "exec codex"). The person tapped the launcher, so there is
     * no extra approval; the agent never uses this path.
     */
    fun launchInteractive(shellCommand: String) {
        if (!termuxInstalled()) throw DeviceException.Unavailable("Termux is not installed")
        if (!permissionGranted()) throw DeviceException.Unavailable("Termux RUN_COMMAND permission missing")
        val intent =
            Intent(TERMUX_ACTION).apply {
                setClassName(TERMUX_PACKAGE, TERMUX_SERVICE)
                putExtra("com.termux.RUN_COMMAND_PATH", TERMUX_BASH)
                putExtra("com.termux.RUN_COMMAND_ARGUMENTS", arrayOf("-l", "-c", shellCommand))
                putExtra("com.termux.RUN_COMMAND_WORKDIR", TERMUX_HOME)
                putExtra("com.termux.RUN_COMMAND_BACKGROUND", false)
                putExtra("com.termux.RUN_COMMAND_SESSION_ACTION", "0")
            }
        try {
            context.startService(intent)
        } catch (_: SecurityException) {
            throw DeviceException.Unavailable("Termux RUN_COMMAND permission missing")
        } catch (_: IllegalStateException) {
            throw DeviceException.Unavailable("Termux could not start")
        }
    }

    private fun termuxInstalled(): Boolean =
        try {
            context.packageManager.getPackageInfo(TERMUX_PACKAGE, 0)
            true
        } catch (_: PackageManager.NameNotFoundException) {
            false
        }

    private fun permissionGranted(): Boolean =
        context.checkSelfPermission(TERMUX_PERMISSION) == PackageManager.PERMISSION_GRANTED

    private fun execute(request: ShellRequest): ShellOutput {
        val action = "dev.harw.mobile.TERMUX_RESULT." + UUID.randomUUID()
        val latch = CountDownLatch(1)
        val result = AtomicReference<Bundle?>(null)
        val receiver =
            object : BroadcastReceiver() {
                override fun onReceive(ctx: Context?, intent: Intent?) {
                    result.set(intent?.getBundleExtra("result") ?: Bundle.EMPTY)
                    latch.countDown()
                }
            }
        val filter = IntentFilter(action)
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU) {
            context.registerReceiver(receiver, filter, Context.RECEIVER_NOT_EXPORTED)
        } else {
            @Suppress("UnspecifiedRegisterReceiverFlag")
            context.registerReceiver(receiver, filter)
        }
        try {
            val pending =
                PendingIntent.getBroadcast(
                    context,
                    requestCodes.getAndIncrement(),
                    Intent(action).setPackage(context.packageName),
                    PendingIntent.FLAG_MUTABLE or PendingIntent.FLAG_UPDATE_CURRENT,
                )
            val intent =
                Intent(TERMUX_ACTION).apply {
                    setClassName(TERMUX_PACKAGE, TERMUX_SERVICE)
                    putExtra("com.termux.RUN_COMMAND_PATH", request.argv[0])
                    putExtra("com.termux.RUN_COMMAND_ARGUMENTS", request.argv.drop(1).toTypedArray())
                    request.cwd?.let { putExtra("com.termux.RUN_COMMAND_WORKDIR", workdir(it)) }
                    putExtra("com.termux.RUN_COMMAND_BACKGROUND", true)
                    putExtra("com.termux.RUN_COMMAND_PENDING_INTENT", pending)
                }
            try {
                context.startService(intent)
            } catch (_: SecurityException) {
                throw DeviceException.Unavailable("Termux RUN_COMMAND permission missing")
            } catch (_: IllegalStateException) {
                throw DeviceException.Unavailable("Termux could not start")
            }
            if (!latch.await(clampTimeout(request.timeoutMs.toLong().coerceAtLeast(0L)), TimeUnit.MILLISECONDS)) {
                return ShellOutput(
                    exitCode = null,
                    stdout = "",
                    stderr = "",
                    truncated = false,
                    timedOut = true,
                )
            }
            return parse(result.get() ?: Bundle.EMPTY, clampOutput(request.maxOutputBytes.toInt().coerceAtLeast(0)))
        } finally {
            try {
                context.unregisterReceiver(receiver)
            } catch (_: IllegalArgumentException) {
                // Already unregistered.
            }
        }
    }

    private fun workdir(cwd: String): String =
        if (cwd.startsWith(TERMUX_HOME_PREFIX)) cwd else TERMUX_HOME + cwd

    private fun parse(bundle: Bundle, maxBytes: Int): ShellOutput {
        val err = if (bundle.containsKey("err")) bundle.getInt("err", TERMUX_NO_ERROR) else TERMUX_NO_ERROR
        if (err != TERMUX_NO_ERROR && err != Activity.RESULT_OK) {
            throw DeviceException.Failed("termux command error")
        }
        val exit = if (bundle.containsKey("exitCode")) bundle.getInt("exitCode") else null
        val (stdout, outCut) = capOutput(bundle.getString("stdout") ?: "", maxBytes)
        val remaining = (maxBytes - utf8Size(stdout)).coerceAtLeast(0)
        val (stderr, errCut) = capOutput(bundle.getString("stderr") ?: "", remaining)
        return ShellOutput(
            exitCode = exit,
            stdout = stdout,
            stderr = stderr,
            truncated = outCut || errCut,
            timedOut = false,
        )
    }
}
