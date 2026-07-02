package dev.pulsar.video

import android.Manifest
import android.app.Activity
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.app.WallpaperManager
import android.content.ClipboardManager
import android.content.ComponentName
import android.content.Context
import android.content.Intent
import android.content.pm.ActivityInfo
import android.content.pm.PackageManager
import android.graphics.Bitmap
import android.graphics.Canvas
import android.graphics.Color
import android.graphics.LinearGradient
import android.graphics.Matrix
import android.graphics.Paint
import android.graphics.PixelFormat
import android.graphics.Shader
import android.graphics.SurfaceTexture
import android.media.AudioAttributes
import android.media.AudioFormat
import android.media.AudioRecord
import android.media.AudioTrack
import android.media.MediaCodec
import android.media.MediaCodecInfo
import android.media.MediaCodecList
import android.media.MediaExtractor
import android.media.MediaFormat
import android.media.MediaRecorder
import android.media.projection.MediaProjection
import android.media.projection.MediaProjectionManager
import android.net.wifi.WifiManager
import android.os.Build
import android.os.PowerManager
import android.provider.Settings
import android.util.Base64
import android.util.Log
import android.view.Gravity
import android.view.Surface
import android.view.SurfaceHolder
import android.view.SurfaceView
import android.view.TextureView
import android.view.View
import android.view.ViewGroup
import android.webkit.WebView
import android.widget.FrameLayout
import androidx.activity.result.ActivityResult
import androidx.core.app.ActivityCompat
import androidx.core.app.NotificationCompat
import androidx.core.content.ContextCompat
import androidx.core.view.WindowCompat
import app.tauri.annotation.ActivityCallback
import app.tauri.annotation.Command
import app.tauri.annotation.InvokeArg
import app.tauri.annotation.TauriPlugin
import app.tauri.plugin.Invoke
import app.tauri.plugin.JSObject
import app.tauri.plugin.JSArray
import app.tauri.plugin.Plugin
import java.io.ByteArrayOutputStream
import java.io.File
import java.nio.ByteBuffer
import java.util.concurrent.atomic.AtomicBoolean
import java.util.concurrent.LinkedBlockingDeque

// W5-native: max pane slots — was 0..1, now supports up to 4 (quadrant grid).
private const val MAX_PANES = 4
// Identity-image (avatar) square edge + wire-size cap — matches the desktop
// avatar.rs AVATAR_EDGE/MAX_AVATAR_BYTES so peer rendering is consistent.
private const val AVATAR_EDGE = 96
private const val MAX_AVATAR_BYTES = 14000

// W6-host-notify: notification id for the incoming-connection-request heads-up.
// Distinct from the HostService foreground notification (id 2).
private const val REQUEST_NOTIF_ID = 7

@InvokeArg
class AttachArgs {
    var color: String? = null
    var slot: Int = 0
}

/**
 * W5-native: position panes in a named layout.
 * layout: "single" (default), "left-right", "top-bottom", "quad"
 * For "left-right" slot 0 goes LEFT, slot 1 goes RIGHT.
 * For "top-bottom" slot 0 goes TOP, slot 1 goes BOTTOM.
 * For "quad" slot 0=top-left, 1=top-right, 2=bottom-left, 3=bottom-right.
 */
@InvokeArg
class PositionPanesArgs {
    var layout: String = "single"
}

/**
 * W5-native: HDR color mode for a pane's SurfaceView.
 * mode: "sdr" | "hdr10" | "hlg"
 */
@InvokeArg
class SetHdrModeArgs {
    var slot: Int = 0
    var mode: String = "sdr"  // "sdr" | "hdr10" | "hlg"
}

/**
 * Pinch-zoom / pan transform for a slot, expressed as the video's DESTINATION
 * rect on screen, NORMALIZED to the surface [0..1] (w/h may exceed 1 when zoomed
 * in). Normalized so the CSS-pixel webview and the device-pixel native surface
 * agree regardless of devicePixelRatio. JS computes it aspect-correctly from the
 * video size (reported via the `video-size` event); native just applies it.
 */
@InvokeArg
class VideoTransformArgs {
    var slot: Int = 0
    var x: Float = 0f
    var y: Float = 0f
    var w: Float = 1f
    var h: Float = 1f
}

@InvokeArg
class VideoSizeArgs {
    var slot: Int = 0
}

@InvokeArg
class StartStreamArgs {
    var mime: String? = null
    var slot: Int = 0
}

@InvokeArg
class FeedAuArgs {
    var data: String? = null
    var slot: Int = 0
}

@InvokeArg
class HostArgs {
    var port: Int = 0
    var audio_port: Int = 0
    var codec: String? = null
    var width: Int = 0
    var height: Int = 0
    var fps: Int = 0
    var bitrate_kbps: Int = 0
}

@InvokeArg
class GestureArgs {
    var x1: Double = 0.0
    var y1: Double = 0.0
    var x2: Double = 0.0
    var y2: Double = 0.0
}

@InvokeArg
class SetAudioMutedArgs {
    var muted: Boolean = false
}

@InvokeArg
class SetAspectArgs {
    var slot: Int = 0
    var mode: String = "fit"  // "fit" | "fill" | "stretch"
}

@InvokeArg
class SetOrientationArgs {
    var landscape: Boolean = false
}

@InvokeArg
class NotifyRequestArgs {
    var peer: String? = null
}

@InvokeArg
class StatusBarArgs {
    var lightTheme: Boolean = true
}

/**
 * Native video surface(s) UNDER a transparent Tauri WebView (Path A).
 * One [Pane] per slot: slot 0 single/fullscreen, slot 0+1 = split (stacked or side-by-side),
 * slots 0-3 = 2x2 quad layout. W5-native bumps the cap from 2 to MAX_PANES.
 * Audio (M5) is shared and tied to slot 0.
 */
@TauriPlugin
class PulsarVideoPlugin(private val activity: Activity) : Plugin(activity) {
    private val TAG = "PulsarVideo"
    private var webView: WebView? = null
    // W5-native: bumped from 2 panes to MAX_PANES
    private val panes = Array(MAX_PANES) { Pane(it) }

    // --- live audio (M5): Opus → MediaCodec(audio/opus) → AudioTrack ---
    private val audioLock = Any()
    private var audioCodec: MediaCodec? = null
    private var audioTrack: AudioTrack? = null
    private var audioConfigured = false
    // Audio decode error containment: one invalid opus packet makes c2.android.opus.decoder
    // fail FATALLY (C2 err 14 → MediaCodec dead). Recover by releasing + reconfiguring on the
    // next packet; if the codec keeps dying (poison packet STREAM — the fatal error surfaces
    // one packet late, so a consecutive-failure counter would oscillate 0↔1 and never trip),
    // count REBUILDS in a time window and disable audio for the session. The alternative is a
    // per-packet exception+log storm on the main thread that starved the whole app (frozen
    // video), or a MediaCodec+AudioTrack rebuild every other packet.
    private var audioErrs = 0L
    private var audioRebuilds = 0
    private var audioRebuildWinMs = 0L
    private var audioDisabled = false
    /** Current gain for the AudioTrack: 1f = full volume, 0f = muted. W3-audio-mute. */
    private var audioGain = 1f
    /** Per-pane aspect mode: "fit" | "fill" | "stretch". W3-aspect. W5-native: 4 slots. */
    private val aspectMode = Array(MAX_PANES) { "fit" }
    /** W5-native: current layout — "single" | "left-right" | "top-bottom" | "quad". */
    private var currentLayout = "single"

    // --- W4-mic: AudioRecord mic upstream (phone mic → host via DataMsg::Audio) ---
    /** True while AudioRecord is capturing. */
    private val micRunning = AtomicBoolean(false)
    /** Background capture thread — non-null while running. */
    private var micThread: Thread? = null
    /**
     * Ring buffer of captured PCM frames (~20 ms each at 48k mono s16le = 1920 bytes/frame).
     * Bounded at 50 frames (~1 second) so a slow Rust consumer doesn't grow unbounded.
     * The W4-rust-client `mic_start` Tauri command drains this via a separate poll loop
     * and sends each frame as `DataMsg::Audio`. This buffer is package-accessible so
     * the Rust side could poll it via JNI in a future optimization, but for now it is
     * only read by `pollMicFrame` below.
     */
    val micBuffer = LinkedBlockingDeque<ByteArray>(50)
    /** Permission request code for RECORD_AUDIO. */
    private val MIC_PERM_CODE = 0x4D49 // 'MI'

    // --- mobile host (M16): MediaProjection → MediaCodec encoder → RTP → loopback UDP ---
    private var projection: MediaProjection? = null
    private var hostEncoder: HostEncoder? = null
    private var hPort = 0
    private var hAudioPort = 0
    private var hMime = "video/avc"
    private var hW = 0
    private var hH = 0
    private var hFps = 30
    private var hKbps = 0

    override fun load(webView: WebView) {
        super.load(webView)
        this.webView = webView
    }

    // ---------------- commands ----------------

    @Command
    fun attach(invoke: Invoke) {
        val args = invoke.parseArgs(AttachArgs::class.java)
        val pane = panes[args.slot.coerceIn(0, MAX_PANES - 1)]
        args.color?.let { try { pane.fillColor = Color.parseColor(it) } catch (_: Exception) {} }
        val wv = webView ?: run { invoke.reject("webView not available yet"); return }
        activity.runOnUiThread {
            try {
                val parent = ensureWebViewTransparent() ?: run { invoke.reject("no parent ViewGroup"); return@runOnUiThread }
                pane.ensureSurface(parent)
                relayout()
                val ret = JSObject(); ret.put("ok", true)
                ret.put("detail", "slot ${pane.slot} surface in ${parent.javaClass.simpleName}")
                invoke.resolve(ret)
            } catch (e: Exception) { invoke.reject("attach failed: ${e.message}") }
        }
    }

    @Command
    fun startStream(invoke: Invoke) {
        val args = invoke.parseArgs(StartStreamArgs::class.java)
        val pane = panes[args.slot.coerceIn(0, MAX_PANES - 1)]
        val mime = args.mime ?: "video/hevc"
        if (webView == null) { invoke.reject("webView not available yet"); return }
        activity.runOnUiThread {
            try {
                val parent = ensureWebViewTransparent() ?: run { invoke.reject("no parent ViewGroup"); return@runOnUiThread }
                pane.ensureSurface(parent)
                pane.arm(mime)
                val auPort = pane.ensureAuSocket() // raw AU feed (bypasses per-frame IPC)
                relayout()
                startStreamService()
                val ret = JSObject(); ret.put("ok", true); ret.put("detail", "port=$auPort")
                invoke.resolve(ret)
            } catch (e: Exception) { invoke.reject("startStream failed: ${e.message}") }
        }
    }

    @Command
    fun feedAu(invoke: Invoke) {
        val args = invoke.parseArgs(FeedAuArgs::class.java)
        val pane = panes[args.slot.coerceIn(0, MAX_PANES - 1)]
        val b64 = args.data ?: run { invoke.reject("no data"); return }
        val au = Base64.decode(b64, Base64.DEFAULT)
        // Enqueue + return immediately; the pane's decoder thread does the blocking
        // decode so the Rust read loop never stalls (the 120 fps latency fix).
        pane.enqueue(au)
        val ret = JSObject(); ret.put("ok", true)
        invoke.resolve(ret)
    }

    @Command
    fun feedAudio(invoke: Invoke) {
        val b64 = invoke.parseArgs(FeedAuArgs::class.java).data ?: run { invoke.reject("no data"); return }
        val opus = Base64.decode(b64, Base64.DEFAULT)
        var ok = false
        synchronized(audioLock) {
            try {
                if (!audioDisabled && opus.isNotEmpty()) {
                    if (!audioConfigured) configureAudioLocked()
                    if (audioConfigured) feedAudioPacketLocked(opus)
                    ok = audioConfigured
                }
            } catch (e: Exception) {
                // A fatal decode error leaves the MediaCodec permanently dead ("Released
                // state" on every later call). Tear it down so the NEXT packet rebuilds a
                // fresh decoder (~10 ms glitch) instead of feeding the corpse forever.
                audioErrs++
                if (audioErrs <= 3 || audioErrs % 500 == 0L) Log.e(TAG, "feedAudio error #$audioErrs", e)
                releaseAudioLocked()
                // Circuit breaker on rebuild RATE (not consecutiveness — the fatal error
                // surfaces on the packet AFTER the poison one, so successes interleave).
                val now = android.os.SystemClock.elapsedRealtime()
                if (now - audioRebuildWinMs > 10_000) { audioRebuildWinMs = now; audioRebuilds = 0 }
                audioRebuilds++
                if (audioRebuilds >= 5) {
                    audioDisabled = true
                    Log.e(TAG, "audio decoder died $audioRebuilds times in 10s — audio disabled for this session")
                }
            }
        }
        val ret = JSObject(); ret.put("ok", ok); invoke.resolve(ret)
    }

    @Command
    fun playTest(invoke: Invoke) {
        val pane = panes[0]
        if (webView == null) { invoke.reject("webView not available yet"); return }
        activity.runOnUiThread {
            try {
                val parent = ensureWebViewTransparent() ?: run { invoke.reject("no parent ViewGroup"); return@runOnUiThread }
                pane.ensureSurface(parent)
                relayout()
                pane.playCanned()
                val ret = JSObject(); ret.put("ok", true); ret.put("detail", "canned decode")
                invoke.resolve(ret)
            } catch (e: Exception) { invoke.reject("playTest failed: ${e.message}") }
        }
    }

    @Command
    fun stopStream(invoke: Invoke) {
        val slot = invoke.parseArgs(FeedAuArgs::class.java).slot.coerceIn(0, MAX_PANES - 1)
        activity.runOnUiThread {
            panes[slot].release()
            if (panes.none { it.active }) { releaseAudio(); stopStreamService() }
            relayout()
            val ret = JSObject(); ret.put("ok", true); invoke.resolve(ret)
        }
    }

    @Command
    fun detach(invoke: Invoke) {
        activity.runOnUiThread {
            try {
                panes.forEach { it.release() }
                releaseAudio()
                stopStreamService()
                webView?.setBackgroundColor(Color.WHITE)
                val ret = JSObject(); ret.put("ok", true); ret.put("detail", "detached")
                invoke.resolve(ret)
            } catch (e: Exception) { invoke.reject("detach failed: ${e.message}") }
        }
    }

    // ---------------- mobile host (M16) ----------------

    @Command
    fun startHost(invoke: Invoke) {
        val a = invoke.parseArgs(HostArgs::class.java)
        hPort = a.port; hAudioPort = a.audio_port
        hMime = if (a.codec == "h265" || a.codec == "hevc") "video/hevc" else "video/avc"
        hW = a.width; hH = a.height; hFps = if (a.fps > 0) a.fps else 30; hKbps = a.bitrate_kbps
        if (projection != null) {
            activity.runOnUiThread { startCapture() }
            val r = JSObject(); r.put("ok", true); invoke.resolve(r); return
        }
        val mpm = activity.getSystemService(Context.MEDIA_PROJECTION_SERVICE) as MediaProjectionManager
        startActivityForResult(invoke, mpm.createScreenCaptureIntent(), "onProjResult")
    }

    @ActivityCallback
    fun onProjResult(invoke: Invoke, result: ActivityResult) {
        Log.i(TAG, "onProjResult rc=${result.resultCode} data=${result.data != null}")
        if (result.resultCode != Activity.RESULT_OK || result.data == null) {
            invoke.reject("ekran paylaşım izni reddedildi")
            return
        }
        // Android 14+: the mediaProjection FGS must be RUNNING before getMediaProjection
        // (else SecurityException). startForegroundService is async → wait briefly.
        val svc = Intent(activity, HostService::class.java)
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) activity.startForegroundService(svc) else activity.startService(svc)
        val rc = result.resultCode
        val data = result.data!!
        // Android 14+: getMediaProjection is illegal until the mediaProjection FGS is actually
        // foregrounded. startForegroundService is async, so RETRY getMediaProjection — it throws
        // SecurityException ("requires a foreground service…") as a precondition check (the token
        // is NOT consumed) until the FGS is up. Retrying beats the flaky shared-ready-flag.
        val handler = android.os.Handler(android.os.Looper.getMainLooper())
        var tries = 0
        val attempt = object : Runnable {
            override fun run() {
                try {
                    val mpm = activity.getSystemService(Context.MEDIA_PROJECTION_SERVICE) as MediaProjectionManager
                    projection = mpm.getMediaProjection(rc, data)
                    startCapture()
                    val r = JSObject(); r.put("ok", true); invoke.resolve(r)
                } catch (e: SecurityException) {
                    if (tries++ < 40) { handler.postDelayed(this, 100); return }
                    Log.e(TAG, "getMediaProjection: FGS not ready after retries", e)
                    invoke.reject("foreground service not ready")
                } catch (e: Exception) {
                    Log.e(TAG, "projection/capture failed", e)
                    invoke.reject("projection: ${e.message}")
                }
            }
        }
        handler.post(attempt)
    }

    @Command
    fun stopHost(invoke: Invoke) {
        activity.runOnUiThread {
            hostEncoder?.stop(); hostEncoder = null
            try { projection?.stop() } catch (_: Exception) {}
            projection = null
            activity.stopService(Intent(activity, HostService::class.java))
            val r = JSObject(); r.put("ok", true); invoke.resolve(r)
        }
    }

    // Remote control (M16 polish): inject the peer's pointer via the AccessibilityService.
    @Command
    fun hostGesture(invoke: Invoke) {
        val a = invoke.parseArgs(GestureArgs::class.java)
        val dm = activity.resources.displayMetrics
        val w = dm.widthPixels; val h = dm.heightPixels
        Log.i(TAG, "hostGesture ${a.x1},${a.y1} instance=${PulsarA11yService.instance != null}")
        PulsarA11yService.instance?.gesture(
            (a.x1 * w).toFloat(), (a.y1 * h).toFloat(),
            (a.x2 * w).toFloat(), (a.y2 * h).toFloat()
        )
        val r = JSObject(); r.put("ok", PulsarA11yService.instance != null); invoke.resolve(r)
    }

    /** Open Android's Accessibility settings so the user can enable Pulsar control. */
    @Command
    fun openA11ySettings(invoke: Invoke) {
        activity.startActivity(Intent(Settings.ACTION_ACCESSIBILITY_SETTINGS).addFlags(Intent.FLAG_ACTIVITY_NEW_TASK))
        val r = JSObject(); r.put("ok", true); invoke.resolve(r)
    }

    /** Is the Pulsar control (AccessibilityService) currently enabled? */
    @Command
    fun a11yEnabled(invoke: Invoke) {
        val r = JSObject(); r.put("ok", isA11yGranted()); invoke.resolve(r)
    }

    /**
     * True if the user has enabled Pulsar's AccessibilityService in system settings.
     * Checks the persisted GRANTED state (ENABLED_ACCESSIBILITY_SERVICES) rather than
     * `PulsarA11yService.instance != null` (whether the service is bound THIS instant):
     * after an app reinstall the service is still enabled but not yet re-bound, so the
     * instance check wrongly reported "not granted" and the permission card never
     * disappeared. (Live input injection still gates on `instance` — see hostGesture.)
     */
    private fun isA11yGranted(): Boolean {
        val expected = ComponentName(activity.packageName, PulsarA11yService::class.java.name)
            .flattenToString()
        val enabled = Settings.Secure.getString(
            activity.contentResolver, Settings.Secure.ENABLED_ACCESSIBILITY_SERVICES
        ) ?: return false
        return enabled.split(':').any { it.equals(expected, ignoreCase = true) }
    }

    // ---- W3-media-native commands ------------------------------------------------

    /**
     * Mute or unmute the remote audio stream.
     * Sets the AudioTrack volume to 0 (muted) or restores the previous gain (unmuted).
     * Thread-safe via audioLock.
     */
    @Command
    fun setAudioMuted(invoke: Invoke) {
        val args = invoke.parseArgs(SetAudioMutedArgs::class.java)
        synchronized(audioLock) {
            audioGain = if (args.muted) 0f else 1f
            try {
                audioTrack?.setVolume(audioGain)
            } catch (e: Exception) {
                Log.w(TAG, "setAudioMuted: setVolume error", e)
            }
        }
        val r = JSObject(); r.put("ok", true); r.put("muted", args.muted); invoke.resolve(r)
    }

    /**
     * Set the video surface aspect mode for a given slot.
     * mode: "fit" (letterbox, default), "fill" (crop to fill), "stretch" (distort).
     * The mode is stored and applied on the next FORMAT_CHANGED output event.
     */
    @Command
    fun setAspect(invoke: Invoke) {
        val args = invoke.parseArgs(SetAspectArgs::class.java)
        val slotIdx = args.slot.coerceIn(0, MAX_PANES - 1)
        val modeStr = when (args.mode) { "fill", "stretch" -> args.mode; else -> "fit" }
        aspectMode[slotIdx] = modeStr
        // Apply immediately to the current frame dimensions if we have a format
        panes[slotIdx].lastFormat?.let { panes[slotIdx].applyAspect(it) }
        val r = JSObject(); r.put("ok", true); r.put("mode", modeStr); invoke.resolve(r)
    }

    /**
     * Apply a local pinch-zoom transform to a slot's video surface. scale ≥ 1 (1 = fit
     * to screen), tx/ty are pixel offsets with a top-left pivot, so a frame point maps to
     * screen as scale·point + offset. The caller (JS) clamps so the scaled surface always
     * covers the screen (no empty borders = max zoom-out is the fit).
     */
    @Command
    fun setVideoTransform(invoke: Invoke) {
        val args = invoke.parseArgs(VideoTransformArgs::class.java)
        val pane = panes[args.slot.coerceIn(0, MAX_PANES - 1)]
        pane.tnX = args.x
        pane.tnY = args.y
        pane.tnW = if (args.w <= 0f) 1f else args.w
        pane.tnH = if (args.h <= 0f) 1f else args.h
        activity.runOnUiThread { pane.applyTransform() }
        val r = JSObject()
        r.put("ok", true); r.put("x", pane.tnX); r.put("y", pane.tnY); r.put("w", pane.tnW); r.put("h", pane.tnH)
        invoke.resolve(r)
    }

    /**
     * Last decoded video size for a slot, as `detail="<vw>x<vh>"` ("0x0" until the
     * first frame). JS polls this (the plugin's `video-size` trigger event is not
     * deliverable — `registerListener` is not permitted here) to drive aspect-correct
     * pinch-zoom + touch→host coordinate mapping.
     */
    @Command
    fun getVideoSize(invoke: Invoke) {
        val slot = invoke.parseArgs(VideoSizeArgs::class.java).slot.coerceIn(0, MAX_PANES - 1)
        val pane = panes[slot]
        val r = JSObject(); r.put("ok", true); r.put("detail", "${pane.videoW}x${pane.videoH}")
        invoke.resolve(r)
    }

    /**
     * Poll+clear a slot's decoder-failed flag (set when the decode path threw and the codec
     * was released for rebuild). The Rust read loop polls this on its keepalive tick and
     * nudges the host for an IDR — the plugin's own `trigger()` events never reach JS in
     * this build, so a pull command is the reliable native→Rust channel.
     */
    @Command
    fun decoderStatus(invoke: Invoke) {
        val slot = invoke.parseArgs(VideoSizeArgs::class.java).slot.coerceIn(0, MAX_PANES - 1)
        val pane = panes[slot]
        val failed = pane.decoderFailed
        pane.decoderFailed = false
        val r = JSObject(); r.put("ok", true); r.put("detail", if (failed) "failed" else "")
        invoke.resolve(r)
    }

    /**
     * Lock the activity to landscape or portrait orientation.
     * landscape=true → sensorLandscape, false → sensorPortrait.
     */
    @Command
    fun setOrientation(invoke: Invoke) {
        val args = invoke.parseArgs(SetOrientationArgs::class.java)
        activity.requestedOrientation = if (args.landscape)
            ActivityInfo.SCREEN_ORIENTATION_SENSOR_LANDSCAPE
        else
            ActivityInfo.SCREEN_ORIENTATION_SENSOR_PORTRAIT
        val r = JSObject(); r.put("ok", true); r.put("landscape", args.landscape); invoke.resolve(r)
    }

    /**
     * Match the system status / navigation bar icon colour to the app theme.
     * `lightTheme=true` (light app background) → dark icons; `false` (dark theme) →
     * light icons. The MainActivity sets dark icons at launch for the light default;
     * this lets js/theme.js flip them live when the user switches theme.
     */
    @Command
    fun setStatusBar(invoke: Invoke) {
        val args = invoke.parseArgs(StatusBarArgs::class.java)
        activity.runOnUiThread {
            val c = WindowCompat.getInsetsController(activity.window, activity.window.decorView)
            c.isAppearanceLightStatusBars = args.lightTheme
            c.isAppearanceLightNavigationBars = args.lightTheme
        }
        val r = JSObject(); r.put("ok", true); invoke.resolve(r)
    }

    /**
     * Read the system clipboard text. The WebView denies
     * `navigator.clipboard.readText()`, so the JS paste button calls this and
     * gets the text back in `detail`. Clipboard access needs the app focused
     * (true on a button tap) and must run on the UI thread.
     */
    @Command
    fun readClipboard(invoke: Invoke) {
        activity.runOnUiThread {
            try {
                val cm = activity.getSystemService(Context.CLIPBOARD_SERVICE) as ClipboardManager
                val text = cm.primaryClip?.getItemAt(0)?.coerceToText(activity)?.toString() ?: ""
                val r = JSObject(); r.put("ok", true); r.put("detail", text); invoke.resolve(r)
            } catch (e: Exception) {
                invoke.reject("clipboard read failed: ${e.message}")
            }
        }
    }

    /** The display's current refresh rate in Hz (the Settings "frame rate = Auto"
     *  default — so the client never pulls more frames than the panel can show).
     *  Returned in AttachResponse.detail as a decimal string. */
    @Command
    fun screenRefreshRate(invoke: Invoke) {
        val hz = try {
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.R) {
                activity.display?.refreshRate ?: 60f
            } else {
                @Suppress("DEPRECATION")
                activity.windowManager.defaultDisplay.refreshRate
            }
        } catch (e: Exception) { 60f }
        val r = JSObject(); r.put("ok", true); r.put("detail", hz.toString()); invoke.resolve(r)
    }

    /** Connected gamepad/joystick controllers + battery level (0–100, or -1 if
     *  unknown). The Web Gamepad API has no battery; this reads Android's
     *  InputDevice BatteryState (API 31+). Returns a JSON array string in detail:
     *  `[{"name":"…","level":85}]`. */
    @Command
    fun gamepadBattery(invoke: Invoke) {
        val sb = StringBuilder("[")
        var first = true
        try {
            for (id in android.view.InputDevice.getDeviceIds()) {
                val dev = android.view.InputDevice.getDevice(id) ?: continue
                val src = dev.sources
                val isPad = (src and android.view.InputDevice.SOURCE_GAMEPAD) == android.view.InputDevice.SOURCE_GAMEPAD ||
                    (src and android.view.InputDevice.SOURCE_JOYSTICK) == android.view.InputDevice.SOURCE_JOYSTICK
                if (!isPad || dev.isVirtual) continue
                var level = -1
                if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.S) {
                    try {
                        val bs = dev.batteryState
                        if (bs != null && bs.isPresent) level = (bs.capacity * 100f).toInt()
                    } catch (_: Exception) {}
                }
                val name = (dev.name ?: "Controller").replace("\\", "\\\\").replace("\"", "\\\"")
                if (!first) sb.append(","); first = false
                sb.append("{\"name\":\"").append(name).append("\",\"level\":").append(level).append("}")
            }
        } catch (_: Exception) {}
        sb.append("]")
        val r = JSObject(); r.put("ok", true); r.put("detail", sb.toString()); invoke.resolve(r)
    }

    /**
     * A small (96×96, ≤14 KB) JPEG avatar derived from the home-screen wallpaper's
     * dominant COLORS (`WallpaperManager.getWallpaperColors`), base64'd in `detail`.
     *
     * We use the colors, NOT the wallpaper image: reading the image needs
     * READ_EXTERNAL_STORAGE, which is silently auto-denied on Android 13+ targets
     * (no dialog, not even toggleable in Settings), whereas getWallpaperColors needs
     * NO permission and works on every device. The result is a diagonal gradient of
     * the wallpaper's primary→secondary→tertiary colors — a stable, wallpaper-tied
     * identity tile that tracks the user's theme. Falls back to the Pulsar indigo if
     * colors are unavailable. `ok:false` only on unexpected error.
     */
    @Command
    fun getWallpaperAvatar(invoke: Invoke) {
        val r = JSObject()
        try {
            val wm = WallpaperManager.getInstance(activity)
            val wc = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O_MR1)
                wm.getWallpaperColors(WallpaperManager.FLAG_SYSTEM) else null
            val c1 = wc?.primaryColor?.toArgb() ?: Color.parseColor("#5B6CF0")
            val c2 = wc?.secondaryColor?.toArgb() ?: c1
            val c3 = wc?.tertiaryColor?.toArgb() ?: c2
            val bmp = gradientBitmap(AVATAR_EDGE, intArrayOf(c1, c2, c3))
            val out = encodeAvatarJpeg(bmp, AVATAR_EDGE, MAX_AVATAR_BYTES)
            if (out != null) {
                r.put("ok", true)
                r.put("detail", Base64.encodeToString(out, Base64.NO_WRAP))
            } else {
                r.put("ok", false); r.put("detail", "")
            }
        } catch (e: Exception) {
            r.put("ok", false); r.put("detail", "")
        }
        invoke.resolve(r)
    }

    /** Render an `edge`×`edge` diagonal gradient square from the given ARGB colors. */
    private fun gradientBitmap(edge: Int, colors: IntArray): Bitmap {
        val bmp = Bitmap.createBitmap(edge, edge, Bitmap.Config.ARGB_8888)
        val canvas = Canvas(bmp)
        val cols = if (colors.size >= 2) colors else intArrayOf(colors[0], colors[0])
        val paint = Paint(Paint.ANTI_ALIAS_FLAG)
        paint.shader = LinearGradient(0f, 0f, edge.toFloat(), edge.toFloat(), cols, null, Shader.TileMode.CLAMP)
        canvas.drawRect(0f, 0f, edge.toFloat(), edge.toFloat(), paint)
        return bmp
    }

    /** Center-crop `src` to a square, scale to `edge`px, encode JPEG at descending
     *  quality until the result is ≤ `maxBytes` (matches the desktop wire cap). */
    private fun encodeAvatarJpeg(src: Bitmap, edge: Int, maxBytes: Int): ByteArray? {
        return try {
            val w = src.width.coerceAtLeast(1)
            val h = src.height.coerceAtLeast(1)
            val side = minOf(w, h)
            val x = (w - side) / 2
            val y = (h - side) / 2
            val square = Bitmap.createBitmap(src, x, y, side, side)
            val scaled = Bitmap.createScaledBitmap(square, edge, edge, true)
            var last: ByteArray? = null
            for (q in intArrayOf(80, 60, 40, 25)) {
                val bos = ByteArrayOutputStream()
                scaled.compress(Bitmap.CompressFormat.JPEG, q, bos)
                last = bos.toByteArray()
                if (last.size <= maxBytes) return last
            }
            last
        } catch (e: Exception) { null }
    }

    /**
     * W6-host-notify: post a high-priority heads-up notification for an incoming
     * connection request. The Rust host emits a `session-request` event that shows
     * the in-app approval sheet, but that is invisible when the app is backgrounded
     * or the screen is off — so a host phone sitting idle gave the user NO alert.
     * This fires alongside that event (from `approval_race`); tapping it relaunches
     * the app, where the still-pending approval sheet (30 s window) is visible.
     */
    @Command
    fun notifyRequest(invoke: Invoke) {
        try {
            // Short, locale-matched text: long copy overran the heads-up banner on
            // narrow screens (e.g. the Galaxy Fold cover display). Follow the device
            // language like the rest of the UI does.
            val tr = java.util.Locale.getDefault().language == "tr"
            val chanName = if (tr) "Bağlantı istekleri" else "Connection requests"
            val title = if (tr) "Bağlantı isteği" else "Connection request"
            val text = if (tr) "Onaylamak için dokun" else "Tap to approve"
            val nm = activity.getSystemService(NotificationManager::class.java)
            val chan = "pulsar-request"
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
                nm.createNotificationChannel(
                    NotificationChannel(chan, chanName, NotificationManager.IMPORTANCE_HIGH)
                )
            }
            val launch = activity.packageManager
                .getLaunchIntentForPackage(activity.packageName)
                ?.apply { addFlags(Intent.FLAG_ACTIVITY_NEW_TASK or Intent.FLAG_ACTIVITY_SINGLE_TOP) }
            val pi = PendingIntent.getActivity(
                activity, 0, launch ?: Intent(),
                PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE
            )
            val n = NotificationCompat.Builder(activity, chan)
                .setSmallIcon(android.R.drawable.ic_dialog_info)
                .setContentTitle(title)
                .setContentText(text)
                .setPriority(NotificationCompat.PRIORITY_HIGH)
                .setCategory(NotificationCompat.CATEGORY_CALL)
                .setAutoCancel(true)
                .setContentIntent(pi)
                .build()
            nm.notify(REQUEST_NOTIF_ID, n)
            val r = JSObject(); r.put("ok", true); invoke.resolve(r)
        } catch (e: Exception) {
            Log.e(TAG, "notifyRequest failed", e)
            val r = JSObject(); r.put("ok", false); invoke.resolve(r)
        }
    }

    // ---- W4-mic commands --------------------------------------------------------

    /**
     * Start capturing microphone audio.
     *
     * Uses `MediaRecorder.AudioSource.VOICE_COMMUNICATION` (enables AEC + NS on
     * most Android hardware), 48 kHz, mono, PCM 16-bit — interoperable with the
     * desktop host's `paplay`/`aplay` pipeline that expects raw s16le from
     * `DataMsg::Audio`.
     *
     * Frame size = 20 ms = 960 samples × 2 bytes = 1920 bytes.
     *
     * The command first checks / requests `RECORD_AUDIO` at runtime (Android 6+).
     * If permission is already granted it starts immediately; otherwise it fires
     * the runtime request and resolves `ok: false, detail: "permission_requested"` —
     * the JS layer listens for the result and calls `micStart` again once granted.
     *
     * Frames are pushed into `micBuffer`. The W4-rust-client `mic_start` Tauri
     * command drains the buffer in a tokio task and forwards them as
     * `DataMsg::Audio` (~20 ms each). `micStop` sets `micRunning = false` which
     * causes the capture thread to exit, then clears the buffer.
     */
    @Command
    fun micStart(invoke: Invoke) {
        // Runtime permission check (Android 6+ / API 23)
        if (ContextCompat.checkSelfPermission(activity, Manifest.permission.RECORD_AUDIO)
            != PackageManager.PERMISSION_GRANTED
        ) {
            ActivityCompat.requestPermissions(
                activity,
                arrayOf(Manifest.permission.RECORD_AUDIO),
                MIC_PERM_CODE
            )
            val r = JSObject()
            r.put("ok", false)
            r.put("detail", "permission_requested")
            invoke.resolve(r)
            return
        }

        if (micRunning.get()) {
            val r = JSObject(); r.put("ok", true); r.put("detail", "already_running"); invoke.resolve(r); return
        }

        // AudioRecord parameters: VOICE_COMMUNICATION source enables hardware AEC/NS
        val sampleRate = 48000
        val channelConfig = AudioFormat.CHANNEL_IN_MONO
        val encoding = AudioFormat.ENCODING_PCM_16BIT
        val frameBytes = 1920 // 20 ms at 48k mono s16le: 960 samples × 2 bytes
        val minBuf = AudioRecord.getMinBufferSize(sampleRate, channelConfig, encoding)
        if (minBuf == AudioRecord.ERROR || minBuf == AudioRecord.ERROR_BAD_VALUE) {
            invoke.reject("AudioRecord not available on this device"); return
        }
        val bufSize = maxOf(minBuf, frameBytes * 4)

        val rec = try {
            AudioRecord(
                MediaRecorder.AudioSource.VOICE_COMMUNICATION,
                sampleRate,
                channelConfig,
                encoding,
                bufSize
            )
        } catch (e: SecurityException) {
            invoke.reject("mic permission denied: ${e.message}"); return
        } catch (e: Exception) {
            invoke.reject("AudioRecord init failed: ${e.message}"); return
        }

        if (rec.state != AudioRecord.STATE_INITIALIZED) {
            rec.release()
            invoke.reject("AudioRecord failed to initialize"); return
        }

        micBuffer.clear()
        micRunning.set(true)
        rec.startRecording()

        micThread = Thread {
            val buf = ByteArray(frameBytes)
            Log.i(TAG, "mic capture started: 48k mono s16le frameBytes=$frameBytes")
            try {
                while (micRunning.get()) {
                    var read = 0
                    while (read < frameBytes && micRunning.get()) {
                        val n = rec.read(buf, read, frameBytes - read)
                        if (n < 0) { micRunning.set(false); break }
                        read += n
                    }
                    if (read > 0) {
                        val frame = buf.copyOf(read)
                        // Offer without blocking — drop oldest if buffer is full
                        if (!micBuffer.offer(frame)) {
                            micBuffer.poll()
                            micBuffer.offer(frame)
                        }
                    }
                }
            } catch (e: Exception) {
                Log.e(TAG, "mic capture error", e)
            } finally {
                try { rec.stop() } catch (_: Exception) {}
                rec.release()
                Log.i(TAG, "mic capture stopped")
            }
        }.also { it.isDaemon = true; it.name = "pulsar-mic-capture"; it.start() }

        val r = JSObject(); r.put("ok", true); r.put("detail", "mic_started"); invoke.resolve(r)
    }

    /**
     * Stop the AudioRecord capture loop and clear any buffered PCM frames.
     * After this returns the Rust `mic_stop` command sends `DataMsg::AudioEnd`.
     */
    @Command
    fun micStop(invoke: Invoke) {
        micRunning.set(false)
        micThread?.interrupt()
        micThread = null
        micBuffer.clear()
        Log.i(TAG, "micStop called")
        val r = JSObject(); r.put("ok", true); r.put("detail", "mic_stopped"); invoke.resolve(r)
    }

    /**
     * Poll one PCM frame from the mic buffer (non-blocking).
     * Returns `{ ok: true, data: "<base64>" }` when a frame is available,
     * or `{ ok: false }` when the buffer is empty. The W4-rust-client mic loop
     * calls this in a tight poll (with a small sleep) to drain PCM and forward
     * it as `DataMsg::Audio` frames to the host.
     *
     * NOTE: This command is a native-only bridge (called from Rust via the Tauri
     * plugin handle, not from JS). It is NOT registered in `lib.rs`'s
     * `generate_handler!` and NOT in `permissions/default.toml`. Only
     * `PulsarVideo<R>.poll_mic_frame()` in `mobile.rs` calls it.
     */
    @Command
    fun pollMicFrame(invoke: Invoke) {
        val frame = micBuffer.poll()
        val r = JSObject()
        if (frame != null) {
            r.put("ok", true)
            r.put("data", Base64.encodeToString(frame, Base64.NO_WRAP))
        } else {
            r.put("ok", false)
        }
        invoke.resolve(r)
    }

    // ---- W5-native commands --------------------------------------------------------

    /**
     * W5-native: Position the active panes in a named layout.
     *
     * layout:
     *   "single"     – slot 0 fullscreen (default)
     *   "left-right" – slot 0 left half, slot 1 right half (landscape split)
     *   "top-bottom" – slot 0 top half, slot 1 bottom half (portrait split)
     *   "quad"       – slots 0-3 as 2x2 grid (top-left, top-right, bottom-left, bottom-right)
     *
     * Native-only bridge — not a JS command. Called from Rust `position_panes`.
     */
    @Command
    fun positionPanes(invoke: Invoke) {
        val args = invoke.parseArgs(PositionPanesArgs::class.java)
        val layout = when (args.layout) {
            "left-right", "top-bottom", "quad" -> args.layout
            else -> "single"
        }
        activity.runOnUiThread {
            currentLayout = layout
            relayout()
            val r = JSObject(); r.put("ok", true); r.put("layout", layout); invoke.resolve(r)
        }
    }

    /**
     * W5-native: Probe the device's MediaCodecList and return the supported video
     * decoder MIME types (for `host_codecs` command in the W5-rust-host lane).
     *
     * Returns { codecs: ["h265", "h264", "av1"] } filtered to the types Pulsar
     * actually supports, in preference order (best-quality first).
     *
     * Native-only bridge — not a JS command.
     */
    @Command
    fun enumerateDecoders(invoke: Invoke) {
        val supported = mutableSetOf<String>()
        try {
            val list = MediaCodecList(MediaCodecList.REGULAR_CODECS)
            for (info in list.codecInfos) {
                if (info.isEncoder) continue
                for (mime in info.supportedTypes) {
                    when (mime.lowercase()) {
                        "video/hevc" -> supported.add("h265")
                        "video/avc"  -> supported.add("h264")
                        "video/av01" -> supported.add("av1")
                        "video/av1"  -> supported.add("av1")
                        else -> {}
                    }
                }
            }
        } catch (e: Exception) {
            Log.w(TAG, "enumerateDecoders error", e)
        }
        // Return in preference order: h265 > h264 > av1
        val ordered = listOf("h265", "h264", "av1").filter { it in supported }
        val arr = JSArray()
        ordered.forEach { arr.put(it) }
        val r = JSObject(); r.put("ok", true); r.put("codecs", arr); invoke.resolve(r)
    }

    /**
     * W5-native: Set or clear HDR rendering for a slot's SurfaceView.
     *
     * mode: "sdr" (default), "hdr10", "hlg"
     *
     * On API 33+ sets the DataSpace on the Surface via SurfaceHolder; also sets
     * COLOR_MODE on the SurfaceView (API 26+). Falls back gracefully on older APIs.
     *
     * Native-only bridge — not a JS command.
     */
    @Command
    fun setHdrMode(invoke: Invoke) {
        val args = invoke.parseArgs(SetHdrModeArgs::class.java)
        val slotIdx = args.slot.coerceIn(0, MAX_PANES - 1)
        val mode = when (args.mode) { "hdr10", "hlg" -> args.mode; else -> "sdr" }
        activity.runOnUiThread {
            val pane = panes[slotIdx]
            pane.setHdrMode(mode)
            val r = JSObject(); r.put("ok", true); r.put("slot", slotIdx); r.put("mode", mode)
            invoke.resolve(r)
        }
    }

    private fun startCapture() {
        val proj = projection ?: return
        // A re-stream (the desktop's adaptive re-request) calls this again mid-session. We
        // capture at native res and ignore the client's params, so the running encoder is
        // still correct — KEEP it (tearing it down + recreating glitched/froze the capture).
        if (hostEncoder != null) {
            Log.i(TAG, "startCapture: already running — re-stream ignored")
            return
        }
        val dm = activity.resources.displayMetrics
        // Capture the phone's NATIVE screen (IGNORE the client's res/fps — desktop gaming
        // defaults). Downscale so the larger dim ≤ 1280: native 1080x2400 (2.6 MP) overloads a
        // software/emulator H.264 encoder + needs a high bitrate → glitchy macroblocks; a smaller
        // frame (same aspect → no distortion) is sharper per-bit + far lighter.
        var w = dm.widthPixels
        var h = dm.heightPixels
        val maxDim = 1280
        if (maxOf(w, h) > maxDim) {
            val s = maxDim.toDouble() / maxOf(w, h)
            w = (w * s).toInt(); h = (h * s).toInt()
        }
        w = w and 1.inv(); h = h and 1.inv()
        val fps = minOf(if (hFps > 0) hFps else 30, 30)
        val kbps = minOf(if (hKbps > 0) hKbps else 6000, 8000)
        Log.i(TAG, "startCapture native ${w}x${h}@$fps ${kbps}k mime=$hMime")
        HostEncoder(proj, hMime, w, h, fps, kbps * 1000, dm.densityDpi, hPort, hAudioPort).also { it.start(); hostEncoder = it }
    }

    // ---------------- layout ----------------

    private fun ensureWebViewTransparent(): ViewGroup? {
        val wv = webView ?: return null
        val parent = wv.parent as? ViewGroup ?: return null
        wv.setBackgroundColor(Color.TRANSPARENT)
        return parent
    }

    /**
     * W5-native: Position panes according to [currentLayout].
     *
     * "single"     – slot 0 fullscreen.
     * "left-right" – slot 0 left half / slot 1 right half (landscape gravity).
     * "top-bottom" – slot 0 top half / slot 1 bottom half (portrait gravity; legacy split).
     * "quad"       – 2x2 grid for slots 0-3 (top-left, top-right, bottom-left, bottom-right).
     *
     * The previous behaviour (stacked halves when ≥2 panes were live) is retained as
     * "top-bottom" fallback so existing callers that never set a layout keep working.
     */
    private fun relayout() {
        val parent = (webView?.parent as? ViewGroup) ?: return
        val live = panes.filter { it.surfaceView != null }

        // Determine effective layout: a single live pane always renders fullscreen.
        // With >=2 live panes honour an explicitly-set layout, but when none was ever
        // set (currentLayout still "single" — position_panes has no wired caller yet),
        // fall back to a stacked top/bottom split so the panes never render fullscreen-
        // overlapping (restores the documented pre-W5 behaviour). The window-level touch
        // router (app.js norm()) maps top/bottom halves to match this fallback.
        val effectiveLayout = when {
            live.size <= 1            -> "single"
            currentLayout == "single" -> "top-bottom"
            else                      -> currentLayout
        }

        for (p in panes) {
            val sv = p.surfaceView ?: continue
            val lp: FrameLayout.LayoutParams = when (effectiveLayout) {
                "left-right" -> {
                    // Slot 0 → left half, slot 1 → right half; extras fill right
                    val half = parent.width / 2
                    when (p.slot) {
                        0 -> FrameLayout.LayoutParams(half, ViewGroup.LayoutParams.MATCH_PARENT)
                            .also { it.gravity = Gravity.START or Gravity.TOP }
                        else -> FrameLayout.LayoutParams(half, ViewGroup.LayoutParams.MATCH_PARENT)
                            .also { it.gravity = Gravity.END or Gravity.TOP }
                    }
                }
                "quad" -> {
                    // 2×2 grid: slot % 2 = column (0=left,1=right), slot / 2 = row (0=top,1=bottom)
                    val hw = parent.width / 2
                    val hh = parent.height / 2
                    val col = p.slot % 2
                    val row = p.slot / 2
                    val hGrav = if (col == 0) Gravity.START else Gravity.END
                    val vGrav = if (row == 0) Gravity.TOP else Gravity.BOTTOM
                    FrameLayout.LayoutParams(hw, hh).also { it.gravity = hGrav or vGrav }
                }
                "top-bottom" -> {
                    // Slot 0 → top half, slot 1 → bottom half (legacy stacked split)
                    val half = parent.height / 2
                    when (p.slot) {
                        0 -> FrameLayout.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, half)
                            .also { it.gravity = Gravity.TOP }
                        else -> FrameLayout.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, half)
                            .also { it.gravity = Gravity.BOTTOM }
                    }
                }
                else -> {
                    // "single" — fullscreen
                    FrameLayout.LayoutParams(
                        ViewGroup.LayoutParams.MATCH_PARENT,
                        ViewGroup.LayoutParams.MATCH_PARENT
                    ).also { it.gravity = Gravity.CENTER }
                }
            }
            sv.layoutParams = lp
        }

        splitMode = effectiveLayout != "single"
        // Single-pane: recompute the aspect-fit rect for the new geometry (e.g. rotation).
        // A live pinch-zoom is re-applied by JS on the next gesture / `video-size` event.
        if (!splitMode) live.firstOrNull()?.let { p ->
            p.lastFormat?.let { p.applyAspect(it) }
        }
    }

    private var splitMode = false

    // ---------------- foreground service (M6) ----------------

    // Perf locks: keep the WiFi radio in low-latency mode (no power-save parking, which
    // causes ~100-300ms random latency spikes during a session) + CPU awake.
    private var wifiLock: WifiManager.WifiLock? = null
    private var wakeLock: PowerManager.WakeLock? = null

    private fun acquirePerfLocks() {
        try {
            if (wifiLock == null) {
                val wm = activity.applicationContext.getSystemService(Context.WIFI_SERVICE) as WifiManager
                val mode = if (Build.VERSION.SDK_INT >= 29) WifiManager.WIFI_MODE_FULL_LOW_LATENCY
                           else @Suppress("DEPRECATION") WifiManager.WIFI_MODE_FULL_HIGH_PERF
                wifiLock = wm.createWifiLock(mode, "pulsar:stream").apply { setReferenceCounted(false); acquire() }
            }
            if (wakeLock == null) {
                val pm = activity.getSystemService(Context.POWER_SERVICE) as PowerManager
                wakeLock = pm.newWakeLock(PowerManager.PARTIAL_WAKE_LOCK, "pulsar:stream").apply { setReferenceCounted(false); acquire() }
            }
            // KEEP_SCREEN_ON is REQUIRED for WIFI_MODE_FULL_LOW_LATENCY to stay engaged
            // (it silently disengages when the screen dims → power-save spikes return).
            // SustainedPerformanceMode caps peak clocks slightly to avoid the thermal
            // cliff on long 120fps sessions. Both are window calls (we're on the UI thread).
            try {
                activity.window.addFlags(android.view.WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON)
                if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.N) activity.window.setSustainedPerformanceMode(true)
            } catch (_: Exception) {}
            Log.i(TAG, "perf locks acquired (wifi low-latency + wake + screen-on + sustained)")
        } catch (e: Exception) { Log.w(TAG, "perf locks failed: ${e.message}") }
    }

    private fun releasePerfLocks() {
        try { wifiLock?.let { if (it.isHeld) it.release() } } catch (_: Exception) {}
        try { wakeLock?.let { if (it.isHeld) it.release() } } catch (_: Exception) {}
        try {
            activity.window.clearFlags(android.view.WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON)
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.N) activity.window.setSustainedPerformanceMode(false)
        } catch (_: Exception) {}
        wifiLock = null; wakeLock = null
    }

    private fun startStreamService() {
        acquirePerfLocks()
        val svc = Intent(activity, StreamService::class.java)
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) activity.startForegroundService(svc)
        else activity.startService(svc)
    }

    private fun stopStreamService() {
        releasePerfLocks()
        activity.stopService(Intent(activity, StreamService::class.java))
    }

    // ---------------- audio (M5) ----------------

    private fun configureAudioLocked() {
        val fmt = MediaFormat.createAudioFormat("audio/opus", 48000, 2)
        fmt.setByteBuffer("csd-0", ByteBuffer.wrap(OPUS_HEAD))
        fmt.setByteBuffer("csd-1", ByteBuffer.wrap(leLong(80_000_000L)))
        fmt.setByteBuffer("csd-2", ByteBuffer.wrap(leLong(80_000_000L)))
        val codec = MediaCodec.createDecoderByType("audio/opus")
        try {
            codec.configure(fmt, null, null, 0)
            codec.start()
        } catch (e: Exception) {
            // Never leak a created-but-unassigned codec instance — under a repeating
            // configure failure that leak accumulates one dead codec2 instance per packet
            // and eventually starves the VIDEO decoder's own create/configure.
            try { codec.release() } catch (_: Exception) {}
            throw e
        }
        audioCodec = codec
        val minBuf = AudioTrack.getMinBufferSize(48000, AudioFormat.CHANNEL_OUT_STEREO, AudioFormat.ENCODING_PCM_16BIT)
        // W5-native: target ~80-120 ms jitter buffer.
        // 48000 Hz stereo s16le = 192 000 bytes/s → 80ms = 15 360 bytes, 120ms = 23 040 bytes.
        // We pick 19 200 bytes (~100 ms) clamped to at least minBuf and rounded to a
        // power-of-2-friendly value (20 480 = 20 KiB). Previous default was 16 384 (~85 ms)
        // which is acceptable, but on some devices minBuf > 16 384 caused underruns; the new
        // value gives a safe headroom without perceptible extra latency.
        val targetBuf = 20_480 // ~106 ms at 48k stereo s16le
        audioTrack = AudioTrack.Builder()
            .setAudioAttributes(
                AudioAttributes.Builder().setUsage(AudioAttributes.USAGE_MEDIA)
                    .setContentType(AudioAttributes.CONTENT_TYPE_MOVIE).build()
            )
            .setAudioFormat(
                AudioFormat.Builder().setSampleRate(48000)
                    .setEncoding(AudioFormat.ENCODING_PCM_16BIT)
                    .setChannelMask(AudioFormat.CHANNEL_OUT_STEREO).build()
            )
            .setBufferSizeInBytes(maxOf(minBuf, targetBuf))
            .setTransferMode(AudioTrack.MODE_STREAM)
            .build()
            .also { it.setVolume(audioGain); it.play() }
        audioConfigured = true
        Log.i(TAG, "audio decoder configured: opus 48000/2 buf=${maxOf(minBuf, targetBuf)}B (~${maxOf(minBuf, targetBuf) * 1000 / 192000}ms)")
    }

    private fun feedAudioPacketLocked(opus: ByteArray) {
        val codec = audioCodec ?: return
        val inIdx = codec.dequeueInputBuffer(5000)
        if (inIdx >= 0) {
            val buf = codec.getInputBuffer(inIdx)!!
            buf.clear()
            if (opus.size > buf.remaining()) {
                // Never BufferOverflow into the catch path — an oversized "packet" is
                // garbage (real 10 ms opus is a few hundred bytes); drop it, keep the codec.
                codec.queueInputBuffer(inIdx, 0, 0, 0, 0)
                return
            }
            buf.put(opus)
            codec.queueInputBuffer(inIdx, 0, opus.size, 0, 0)
        }
        val info = MediaCodec.BufferInfo()
        while (true) {
            val outIdx = codec.dequeueOutputBuffer(info, 0)
            if (outIdx < 0) break
            if (info.size > 0) {
                val out = codec.getOutputBuffer(outIdx)!!
                val pcm = ByteArray(info.size)
                out.position(info.offset); out.get(pcm)
                // NON_BLOCKING: this runs on the plugin-invoke (main) thread, called
                // synchronously from the session read loop. A blocking write during a
                // post-hiccup packet burst would stall the read loop (and thus VIDEO
                // delivery) for the buffer-overflow duration; dropping the tail of stale
                // audio keeps playback near-live instead.
                audioTrack?.write(pcm, 0, pcm.size, AudioTrack.WRITE_NON_BLOCKING)
            }
            codec.releaseOutputBuffer(outIdx, false)
        }
    }

    private fun releaseAudio() {
        synchronized(audioLock) {
            releaseAudioLocked()
            // Session teardown: a fresh session starts with audio re-enabled.
            audioDisabled = false; audioRebuilds = 0; audioRebuildWinMs = 0; audioErrs = 0
        }
    }

    /** Caller must hold [audioLock]. */
    private fun releaseAudioLocked() {
        try { audioCodec?.stop() } catch (_: Exception) {}
        try { audioCodec?.release() } catch (_: Exception) {}
        try { audioTrack?.stop() } catch (_: Exception) {}
        try { audioTrack?.release() } catch (_: Exception) {}
        audioCodec = null; audioTrack = null; audioConfigured = false
    }

    private fun leLong(v: Long): ByteArray {
        val b = ByteArray(8)
        for (i in 0..7) b[i] = ((v shr (8 * i)) and 0xFF).toByte()
        return b
    }

    private val OPUS_HEAD = byteArrayOf(
        0x4F, 0x70, 0x75, 0x73, 0x48, 0x65, 0x61, 0x64,
        1, 2, 0x00, 0x0F, 0x80.toByte(), 0xBB.toByte(), 0x00, 0x00, 0x00, 0x00, 0x00
    )

    // ---------------- SPS parsing (real stream dimensions) ----------------

    /** Minimal RBSP bit reader (strips emulation-prevention 00 00 03 bytes). */
    private class BitReader(data: ByteArray, skipBytes: Int) {
        private val rbsp: ByteArray
        private var bit = 0
        init {
            val out = ByteArray(maxOf(0, data.size - skipBytes))
            var n = 0; var zeros = 0; var i = skipBytes
            while (i < data.size) {
                val b = data[i]
                if (zeros >= 2 && b.toInt() == 3) { zeros = 0; i++; continue }
                zeros = if (b.toInt() == 0) zeros + 1 else 0
                out[n++] = b; i++
            }
            rbsp = out.copyOf(n)
        }
        private fun bitAt(p: Int): Int {
            val idx = p ushr 3
            if (idx >= rbsp.size) throw IllegalStateException("sps eof")
            return (rbsp[idx].toInt() shr (7 - (p and 7))) and 1
        }
        fun u(n: Int): Int { var v = 0; repeat(n) { v = (v shl 1) or bitAt(bit++) }; return v }
        fun ue(): Int {
            var zeros = 0
            while (bitAt(bit) == 0) { bit++; zeros++; if (zeros > 31) throw IllegalStateException("bad ue") }
            bit++
            var v = 0; repeat(zeros) { v = (v shl 1) or bitAt(bit++) }
            return (1 shl zeros) - 1 + v
        }
        fun se(): Int { val k = ue(); return if (k and 1 == 0) -(k shr 1) else (k + 1) shr 1 }
    }

    /** pic_width/height_in_luma_samples from a raw HEVC SPS NAL (2-byte NAL header, no start code). */
    private fun hevcSpsDims(sps: ByteArray): Pair<Int, Int>? = try {
        val r = BitReader(sps, 2)
        r.u(4)                       // sps_video_parameter_set_id
        val maxSub = r.u(3)          // sps_max_sub_layers_minus1
        r.u(1)                       // sps_temporal_id_nesting_flag
        // profile_tier_level(profilePresent=1, maxSub): 88 fixed bits + level
        r.u(8); repeat(32) { r.u(1) }; r.u(32); r.u(16); r.u(8)
        if (maxSub > 0) {
            val prof = BooleanArray(maxSub); val lev = BooleanArray(maxSub)
            for (i in 0 until maxSub) { prof[i] = r.u(1) == 1; lev[i] = r.u(1) == 1 }
            for (i in maxSub until 8) r.u(2)
            for (i in 0 until maxSub) {
                if (prof[i]) { r.u(8); repeat(32) { r.u(1) }; r.u(32); r.u(16) }
                if (lev[i]) r.u(8)
            }
        }
        r.ue()                       // sps_seq_parameter_set_id
        if (r.ue() == 3) r.u(1)      // chroma_format_idc (+separate_colour_plane_flag)
        val w = r.ue(); val h = r.ue()
        if (w in 16..8192 && h in 16..8192) Pair(w, h) else null
    } catch (_: Exception) { null }

    /** Frame dimensions from a raw H.264 SPS NAL (1-byte NAL header, no start code). */
    private fun h264SpsDims(sps: ByteArray): Pair<Int, Int>? = try {
        val r = BitReader(sps, 1)
        val profileIdc = r.u(8)
        r.u(8); r.u(8)               // constraint flags + level_idc
        r.ue()                       // seq_parameter_set_id
        if (profileIdc in intArrayOf(100, 110, 122, 244, 44, 83, 86, 118, 128, 138, 139, 134, 135)) {
            val chroma = r.ue()
            if (chroma == 3) r.u(1)  // separate_colour_plane_flag
            r.ue(); r.ue(); r.u(1)   // bit depths + qpprime bypass
            if (r.u(1) == 1) {       // seq_scaling_matrix_present_flag
                val lists = if (chroma != 3) 8 else 12
                for (i in 0 until lists) {
                    if (r.u(1) == 1) {
                        val size = if (i < 6) 16 else 64
                        var last = 8; var next = 8
                        for (j in 0 until size) {
                            if (next != 0) next = (last + r.se() + 256) % 256
                            if (next != 0) last = next
                        }
                    }
                }
            }
        }
        r.ue()                       // log2_max_frame_num_minus4
        when (r.ue()) {              // pic_order_cnt_type
            0 -> r.ue()
            1 -> { r.u(1); r.se(); r.se(); repeat(r.ue()) { r.se() } }
        }
        r.ue(); r.u(1)               // max_num_ref_frames + gaps allowed
        val w = (r.ue() + 1) * 16
        val h = (r.ue() + 1) * 16 * (2 - r.u(1)) // frame_mbs_only_flag
        if (w in 16..8192 && h in 16..8192) Pair(w, h) else null
    } catch (_: Exception) { null }

    // ---------------- per-slot video pane ----------------

    /** Carries the MediaFormat HDR key values to set before decoder configure(). */
    private data class HdrHints(
        val colorStandard: Int?,
        val colorTransfer: Int?,
        val colorRange: Int?,
    )

    private inner class Pane(val slot: Int) {
        var surfaceView: View? = null  // TextureView (matrix-zoomable); SurfaceView only as fallback
        var surface: Surface? = null
        var fillColor: Int = Color.parseColor("#00E5FF")
        var lastFormat: MediaFormat? = null
        val active: Boolean get() = surfaceView != null
        // Set when the decode path threw and the codec was released for rebuild; the Rust
        // read loop polls it (decoderStatus command) and requests a host IDR so the fresh
        // decoder gets a reference frame immediately instead of at the 10 s safety GOP.
        @Volatile var decoderFailed = false

        // ── Decoupled decode feed (Moonlight-style) ─────────────────────────────
        // feedAu() enqueues + returns IMMEDIATELY; this dedicated thread drains the
        // queue and runs the (blocking) decode. Previously feedAndDrain ran inline on
        // the Rust read loop's synchronous IPC call — dequeueInputBuffer's 10 ms block
        // + decode capped throughput at ~90 fps, so at 120 fps the unbounded data_rx
        // backed up to ~1 s latency. Decoupling lets the read loop drain at line rate.
        private val inputQueue = java.util.concurrent.LinkedBlockingDeque<ByteArray>()
        @Volatile private var decoderRunning = false
        private var decoderThread: Thread? = null

        private var droppedAus = 0L

        fun enqueue(au: ByteArray) {
            // Runaway guard: steady-state the decoder keeps up so the queue stays ~empty;
            // if it ever can't, cap the backlog (drop oldest) so latency can't balloon.
            // Dropping tears the HEVC/AVC reference chain, so make it VISIBLE: this used to
            // discard ~96% of all frames silently while the inflight gate was wedged.
            while (inputQueue.size > 16) {
                inputQueue.pollFirst()
                droppedAus++
                if (droppedAus % 100 == 1L) Log.w(TAG, "slot $slot input backlog: dropped $droppedAus AUs total")
            }
            inputQueue.offerLast(au)
            startDecoderThread()
        }

        private fun startDecoderThread() {
            if (decoderRunning) return
            decoderRunning = true
            decoderThread = Thread {
                // A4: this is the latency-critical decode→present thread — run it at
                // URGENT_DISPLAY so the scheduler favours it over background work
                // (Moonlight runs its MediaCodec rendering thread at this priority).
                try { android.os.Process.setThreadPriority(android.os.Process.THREAD_PRIORITY_URGENT_DISPLAY) } catch (_: Exception) {}
                while (decoderRunning) {
                    val au = try { inputQueue.takeFirst() } catch (e: InterruptedException) { break }
                    // NO inflight gate here (Moonlight model): feed whenever the decoder has a
                    // free input buffer. The old framesQueued-framesOut spin gate WEDGED — any
                    // work completing without an output picture (decoder-rejected buffer,
                    // parameter-set-only AU, spin-up frames) inflated the diff forever, and once
                    // it stuck above the limit every AU paid the full spin timeout → ~4 fps
                    // "frozen video" while the HW decoder sat idle. Backpressure already exists
                    // three ways: dequeueInputBuffer's bounded wait in feedAndDrain (input pool
                    // exhaustion is the natural bound), the 16-AU inputQueue cap (drop-oldest),
                    // and the render thread's keep-latest present which bounds PRESENTED latency
                    // to ~1 frame regardless of decoder depth. The vendor low-latency keys
                    // (applyLowLatencyOptions) bound the decoder-internal depth itself.
                    // A24: recycle the AU byte[] back to the free-list once feed() has copied
                    // it into the codec input buffer, so the socket reader can reuse it instead
                    // of allocating per frame (cuts GC churn in the hot path).
                    try { feed(au) } catch (e: Exception) { Log.e(TAG, "slot $slot decode-thread error", e) }
                    finally { recycleAu(au) }
                }
            }.apply { isDaemon = true; name = "pulsar-dec-$slot"; start() }
        }

        fun stopDecoderThread() {
            decoderRunning = false
            decoderThread?.interrupt()
            decoderThread = null
            inputQueue.clear()
        }

        // ── Render thread (Moonlight MediaCodecDecoderRenderer model) ───────────
        // Drains decoded output + presents on its OWN thread, so the input-feed thread's
        // dequeueInputBuffer block can't stall presentation. Single-threaded feed+drain
        // stuttered under heavy motion (outFps swung 9↔149, inflight stuck at 4) because
        // the 10ms input block delayed the output drain. ONLY this thread touches the
        // output side (MediaCodec allows concurrent input/output; two output-drainers
        // would crash). Stopped + joined BEFORE the codec is released (see releaseCodec).
        @Volatile private var renderRunning = false
        private var renderThread: Thread? = null

        private fun startRenderThread() {
            if (renderRunning) return
            renderRunning = true
            renderThread = Thread {
                try { android.os.Process.setThreadPriority(android.os.Process.THREAD_PRIORITY_URGENT_DISPLAY) } catch (_: Exception) {}
                while (renderRunning) {
                    val c = codec
                    if (c == null) { try { Thread.sleep(2) } catch (_: InterruptedException) { break }; continue }
                    try {
                        var renderIdx = -1
                        // Block up to 8ms for the next decoded frame, then non-blocking-drain
                        // any extra (keep-latest), present only the newest with present-now.
                        var outIdx = c.dequeueOutputBuffer(decInfo, 8000)
                        while (outIdx >= 0 || outIdx == MediaCodec.INFO_OUTPUT_FORMAT_CHANGED) {
                            if (outIdx >= 0) {
                                framesOut++
                                if (renderIdx >= 0) c.releaseOutputBuffer(renderIdx, false)
                                renderIdx = outIdx
                            } else { lastFormat = c.outputFormat; applyAspect(c.outputFormat) }
                            outIdx = c.dequeueOutputBuffer(decInfo, 0)
                        }
                        if (renderIdx >= 0) c.releaseOutputBuffer(renderIdx, System.nanoTime())
                    } catch (e: IllegalStateException) {
                        // codec released/reconfigured under us — loop re-reads `codec`.
                        try { Thread.sleep(2) } catch (_: InterruptedException) { break }
                    } catch (e: Exception) {
                        if (renderRunning) Log.w(TAG, "slot $slot render-thread: ${e.message}")
                    }
                }
            }.apply { isDaemon = true; name = "pulsar-render-$slot"; start() }
        }

        private fun stopRenderThread() {
            renderRunning = false
            renderThread?.interrupt()
            try { renderThread?.join(200) } catch (_: Exception) {}
            renderThread = null
        }

        // ── Raw AU socket (loopback) ────────────────────────────────────────────
        // The Rust read loop writes length-prefixed access units here instead of the
        // per-frame base64+JSON IPC (which marshalled at ~12 ms/call → capped ~84 fps
        // → 120 fps backlog = ~1 s latency). Reader thread pushes raw bytes straight
        // to the decoder queue. Idempotent: one server per pane for the session.
        private var auServer: java.net.ServerSocket? = null
        @Volatile private var auSocketRunning = false
        private var auThread: Thread? = null

        // A24: small free-list of AU byte[]s reused across frames to cut GC churn in the
        // hot socket-reader path. The socket reader obtains a buffer (obtainAu); the decoder
        // thread returns it (recycleAu) once feed() has copied it into the codec input buffer.
        // We only reuse a buffer whose size EXACTLY matches the frame, because the array's
        // .size IS the AU length everywhere downstream (enqueue/feed/queueInputBuffer).
        private val auFreeList = java.util.ArrayList<ByteArray>(8)
        private val auFreeLock = Any()
        private fun obtainAu(len: Int): ByteArray {
            synchronized(auFreeLock) {
                val it = auFreeList.iterator()
                while (it.hasNext()) {
                    val b = it.next()
                    if (b.size == len) { it.remove(); return b }
                }
            }
            return ByteArray(len)
        }
        private fun recycleAu(au: ByteArray) {
            // Bound the pool so an I-frame size spike can't pin memory; extras are left to GC.
            synchronized(auFreeLock) { if (auFreeList.size < 8) auFreeList.add(au) }
        }

        fun ensureAuSocket(): Int {
            auServer?.let { return it.localPort }
            val srv = java.net.ServerSocket()
            srv.bind(java.net.InetSocketAddress(java.net.InetAddress.getByName("127.0.0.1"), 0))
            auServer = srv
            auSocketRunning = true
            startDecoderThread()
            auThread = Thread {
                // A4: raise the AU-reader above default so socket reads (and the enqueue that
                // wakes the decoder) aren't starved by background threads. Kept just below the
                // decoder's URGENT_DISPLAY so the decode→present thread still wins the CPU.
                try { android.os.Process.setThreadPriority(android.os.Process.THREAD_PRIORITY_DISPLAY) } catch (_: Exception) {}
                while (auSocketRunning) {
                    try {
                        val sock = srv.accept()
                        sock.tcpNoDelay = true
                        val ins = java.io.DataInputStream(java.io.BufferedInputStream(sock.getInputStream(), 1 shl 20))
                        while (auSocketRunning) {
                            val len = ins.readInt() // big-endian frame length
                            if (len <= 0 || len > (8 shl 20)) break
                            val au = obtainAu(len) // A24: reuse a pooled byte[] when one matches
                            ins.readFully(au)
                            enqueue(au)
                        }
                    } catch (e: Exception) {
                        if (auSocketRunning) Log.w(TAG, "slot $slot au-socket reader ended: ${e.message}")
                    }
                }
            }.apply { isDaemon = true; name = "pulsar-ausock-$slot"; start() }
            Log.i(TAG, "slot $slot au-socket listening on ${srv.localPort}")
            return srv.localPort
        }

        fun stopAuSocket() {
            auSocketRunning = false
            try { auServer?.close() } catch (_: Exception) {}
            auServer = null
            auThread?.interrupt(); auThread = null
        }

        // Last decoded video size (for the `video-size` event that drives JS's
        // aspect-correct zoom math).
        var videoW = 0
        var videoH = 0

        // Current zoom/pan transform = the video's destination rect on screen,
        // NORMALIZED to the surface [0..1] (w/h > 1 = zoomed in). Identity-ish full
        // until the first format gives us a fit rect. AnyDesk/RustDesk-style pinch:
        // JS owns the math (it knows the CSS-pixel geometry); native just applies the
        // rect as a TextureView Matrix — a SurfaceView's surface buffer does NOT
        // reliably follow a beyond-parent layout, so we render into a TextureView and
        // transform its texture quad (which composites correctly AND is screencap-able).
        var tnX = 0f
        var tnY = 0f
        var tnW = 1f
        var tnH = 1f

        /** Apply the current normalized dest rect to the view. UI thread only. */
        fun applyTransform() {
            val v = surfaceView ?: run { Log.i(TAG, "applyTransform: no surfaceView"); return }
            if (splitMode) return // split sizing is owned by relayout()
            val parent = (v.parent as? ViewGroup) ?: run { Log.i(TAG, "applyTransform: no parent"); return }
            val W = parent.width; val H = parent.height
            if (W == 0 || H == 0) { Log.i(TAG, "applyTransform: parent ${W}x${H} zero"); return }
            if (v is TextureView) {
                // Full-screen layout; the Matrix does fit + zoom inside it. The texture
                // is drawn to the full view (W×H) by default, so to land it in the dest
                // rect we scale by (w,h) and translate by (x·W, y·H).
                val lp = v.layoutParams
                if (lp == null || lp.width != ViewGroup.LayoutParams.MATCH_PARENT ||
                    lp.height != ViewGroup.LayoutParams.MATCH_PARENT) {
                    v.layoutParams = FrameLayout.LayoutParams(
                        ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.MATCH_PARENT
                    ).also { it.gravity = Gravity.TOP or Gravity.START }
                }
                val m = Matrix()
                m.setScale(tnW, tnH)
                m.postTranslate(tnX * W, tnY * H)
                v.setTransform(m)
                v.invalidate()
                Log.i(TAG, "applyTransform slot=$slot rect=($tnX,$tnY,$tnW,$tnH) parent=${W}x${H} TextureView")
            } else {
                // SurfaceView fallback: size by layout (works while within parent bounds).
                val lp = FrameLayout.LayoutParams((tnW * W).toInt(), (tnH * H).toInt())
                lp.gravity = Gravity.TOP or Gravity.START
                lp.leftMargin = (tnX * W).toInt()
                lp.topMargin = (tnY * H).toInt()
                v.layoutParams = lp
                v.requestLayout()
                Log.i(TAG, "applyTransform slot=$slot rect=($tnX,$tnY,$tnW,$tnH) parent=${W}x${H} SurfaceView-layout")
            }
        }

        private val lock = Any()
        private var codec: MediaCodec? = null
        private var configured = false
        private var mime: String? = null
        private var pts = 0L
        private var vps: ByteArray? = null
        private var sps: ByteArray? = null
        private var pps: ByteArray? = null
        private var pendingCanned = false
        private var cannedThread: Thread? = null

        fun ensureSurface(parent: ViewGroup) {
            if (surfaceView != null) return
            val lp = FrameLayout.LayoutParams(
                ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.MATCH_PARENT
            )
            // TextureView everywhere (was emulator-only). Two reasons:
            //  1. Pinch-zoom/pan needs setTransform(Matrix); a SurfaceView's surface
            //     buffer does NOT reliably follow a beyond-parent layout (pan breaks).
            //  2. A TextureView lives in the normal GPU hierarchy → composites correctly
            //     under the transparent webview AND is `screencap`-capturable (a real
            //     SurfaceView is a hardware overlay screencap can't read).
            // Trade-off: ~1 frame more latency than a SurfaceView overlay. Acceptable for
            // remote-desktop; if game-mode latency ever needs the overlay back, gate this
            // on the session mode (remote→TextureView, game→SurfaceView).
            // DEFAULT: SurfaceView — Moonlight's renderer. A hardware-overlay surface the
            // decoder writes straight to (no per-frame UI-thread GPU composite), which is
            // what keeps 120fps smooth on all phones. The non-opaque TextureView pays an
            // alpha composite on the UI thread every frame and chokes at 120fps complex
            // content (measured: inflight 4→1, aus 84→113 switching to SurfaceView).
            // Opt back into TextureView (for its Matrix pinch-zoom) only via:
            //   adb shell setprop debug.pulsar.tv 1
            val useTexture = try {
                val sp = Class.forName("android.os.SystemProperties")
                val get = sp.getMethod("get", String::class.java, String::class.java)
                (get.invoke(null, "debug.pulsar.tv", "0") as String) == "1"
            } catch (_: Exception) { false }
            Log.i(TAG, "slot $slot ensureSurface useTexture=$useTexture")
            if (useTexture) {
                // Black behind the surface so the letterbox area (outside the transformed
                // texture quad) shows clean black bars, AnyDesk-style. NOTE: a TextureView
                // itself rejects setBackgroundColor ("doesn't support a background
                // drawable"), so colour the parent — it's only visible (behind the
                // transparent webview) while in a session, which is exactly when we want it.
                parent.setBackgroundColor(Color.BLACK)
                val tv = TextureView(activity)
                // Non-opaque so the uncovered letterbox area shows the black parent rather
                // than undefined/stale pixels.
                tv.isOpaque = false
                tv.surfaceTextureListener = object : TextureView.SurfaceTextureListener {
                    override fun onSurfaceTextureAvailable(st: SurfaceTexture, w: Int, h: Int) {
                        surface = Surface(st)
                        if (pendingCanned) { pendingCanned = false; startCanned(surface!!) }
                        synchronized(lock) { if (mime != null && !configured && canConfigure()) configure() }
                    }
                    override fun onSurfaceTextureSizeChanged(st: SurfaceTexture, w: Int, h: Int) {
                        // Rotation / size change → re-fit so the Matrix transform uses the
                        // new parent dims. Without this the transform kept the old dims and
                        // the image glitched until the next set_video_transform (a pinch).
                        lastFormat?.let { applyAspect(it) }
                    }
                    override fun onSurfaceTextureDestroyed(st: SurfaceTexture): Boolean {
                        surface = null
                        // releaseCodec (NOT inline stop/release): it also joins the render
                        // thread and resets the DIAG counters; params are retained so the
                        // recreated surface reconfigures on the next AU.
                        synchronized(lock) { releaseCodec() }
                        // The recreated surface reconfigures from the retained param sets and
                        // is fed the next AU — almost always a P-frame, which the fresh decoder
                        // has no reference for (garbage/frozen until the ~10 s safety GOP). Flag
                        // decoder-failed so the Rust keepalive poll requests a host IDR at once.
                        decoderFailed = true
                        return true
                    }
                    override fun onSurfaceTextureUpdated(st: SurfaceTexture) {}
                }
                parent.addView(tv, 0, lp)
                surfaceView = tv
            } else {
                // SurfaceView punches a hole in the (transparent) webview window; the parent's
                // black fills the letterbox around the aspect-sized SurfaceView.
                parent.setBackgroundColor(Color.BLACK)
                val sv = SurfaceView(activity)
                sv.holder.setFormat(PixelFormat.OPAQUE)
                sv.holder.addCallback(object : SurfaceHolder.Callback {
                    override fun surfaceCreated(holder: SurfaceHolder) {
                        surface = holder.surface
                        if (pendingCanned) { pendingCanned = false; startCanned(holder.surface) }
                        synchronized(lock) { if (mime != null && !configured && canConfigure()) configure() }
                    }
                    override fun surfaceChanged(holder: SurfaceHolder, f: Int, w: Int, h: Int) { surface = holder.surface }
                    override fun surfaceDestroyed(holder: SurfaceHolder) {
                        surface = null
                        // See onSurfaceTextureDestroyed: releaseCodec joins the render thread
                        // + resets counters; params retained for instant reconfigure.
                        synchronized(lock) { releaseCodec() }
                        // Fresh decoder will be fed a P-frame → request a host IDR via the poll.
                        decoderFailed = true
                    }
                })
                parent.addView(sv, 0, lp)
                surfaceView = sv
            }
            applyTransform() // re-apply any active zoom/pan to the freshly created surface
        }

        fun arm(m: String) = synchronized(lock) {
            releaseCodec(); mime = m; pts = 0L
            // Codec change: the old stream's parameter sets don't apply to the new one.
            vps = null; sps = null; pps = null; av1SeqHeader = null
            // Fresh stream on this (pooled) pane: clear any decoder-failed flag left over
            // from the previous session's surface teardown so it doesn't trigger a spurious
            // early IDR request against the new stream.
            decoderFailed = false
        }

        fun feed(au: ByteArray): Boolean = synchronized(lock) {
            try {
                if (!configured) { collectParams(au); if (canConfigure()) configure() }
                if (configured) feedAndDrain(au)
            } catch (e: Exception) {
                Log.e(TAG, "slot $slot feed error", e)
                // W5-native: emit decoder-error event so the JS/Rust layer can trigger recovery
                emitDecoderError()
                // The reliable recovery channel: the Rust read loop polls this flag
                // (decoderStatus) and requests a host IDR (trigger() events don't arrive).
                decoderFailed = true
                // Release the broken codec so the next AU triggers a fresh configure()
                releaseCodec()
            }
            configured
        }

        private fun isH265() = mime == "video/hevc"
        private fun isAv1()  = mime == "video/av01"

        // W5-native: AV1 sequence header OBU (stored until configure() is called)
        private var av1SeqHeader: ByteArray? = null

        private fun collectParams(au: ByteArray) {
            when {
                isAv1() -> {
                    // AV1: sequence header OBU has obu_type == 1 (bits [7:3] of first byte = 0b00001).
                    // Walk all OBUs in the temporal unit looking for type 1.
                    var off = 0
                    while (off < au.size) {
                        val obuHeader = au[off].toInt() and 0xFF
                        val obuType = (obuHeader shr 3) and 0x0F
                        val hasExtension = (obuHeader shr 2) and 1
                        val hasSizeField = (obuHeader shr 1) and 1
                        off++ // consume obu_header
                        if (hasExtension != 0) off++ // skip obu_extension_header
                        // Decode leb128 size if present
                        val obuSize: Int
                        if (hasSizeField != 0) {
                            var sz = 0; var shift = 0; var more = true
                            while (off < au.size && more) {
                                val b = au[off++].toInt() and 0xFF
                                sz = sz or ((b and 0x7F) shl shift); shift += 7
                                more = (b and 0x80) != 0
                            }
                            obuSize = sz
                        } else {
                            obuSize = au.size - off
                        }
                        if (obuType == 1 && obuSize > 0 && off + obuSize <= au.size) {
                            // Sequence header OBU — store the full OBU (header+size+payload)
                            // for use as csd-0 in the MediaFormat.
                            // csd-0 for video/av01 is the raw sequence header OBU bytes.
                            av1SeqHeader = au.copyOfRange(off - 1 - (if (hasExtension != 0) 1 else 0) - (if (hasSizeField != 0) countLeb128(obuSize) else 0), off + obuSize)
                            break
                        }
                        if (obuSize > 0) off += obuSize
                    }
                }
                isH265() -> {
                    for (nal in splitAnnexB(au)) {
                        if (nal.isEmpty()) continue
                        when ((nal[0].toInt() shr 1) and 0x3F) { 32 -> vps = nal; 33 -> sps = nal; 34 -> pps = nal }
                    }
                }
                else -> {
                    for (nal in splitAnnexB(au)) {
                        if (nal.isEmpty()) continue
                        when (nal[0].toInt() and 0x1F) { 7 -> sps = nal; 8 -> pps = nal }
                    }
                }
            }
        }

        /** Count the number of bytes needed to encode [value] as LEB128. */
        private fun countLeb128(value: Int): Int {
            var v = value; var n = 0
            do { v = v ushr 7; n++ } while (v != 0)
            return n
        }

        private fun canConfigure(): Boolean {
            if (surface == null) return false
            return when {
                isAv1()  -> av1SeqHeader != null
                isH265() -> vps != null && sps != null && pps != null
                else     -> sps != null && pps != null
            }
        }

        private fun configure() {
            val s = surface ?: return
            val m = mime ?: return
            var lastErr: Exception? = null
            // A10: explicitly pick a hardware decoder up-front — createDecoderByType can hand
            // us a c2.android.*/omx.google.* software codec (glitches/lags). null = no HW
            // decoder found → fall back to createDecoderByType inside the loop.
            val pickedName = pickDecoderName(m)
            // Moonlight MediaCodecHelper port: try most→least aggressive low-latency
            // options; the FIRST MediaFormat that configures wins. The vendor keys (esp.
            // Qualcomm vendor.qti-ext-dec-low-latency) are what actually make HW decoders
            // emit frames immediately — KEY_LOW_LATENCY alone is widely IGNORED, so the
            // decoder otherwise buffers a deep queue = the visible lag.
            for (tryNumber in 0..5) {
                var c: MediaCodec? = null
                try {
                    // A10: instantiate the explicitly-picked HW decoder BY NAME (so the vendor
                    // low-latency keying in applyLowLatencyOptions, which switches on c.name, still
                    // targets it). On the final retry (or when none was found) fall back to
                    // createDecoderByType as a last resort so we always get *a* working decoder.
                    c = if (pickedName != null && tryNumber < 5) MediaCodec.createByCodecName(pickedName)
                        else MediaCodec.createDecoderByType(m)
                    val decName = c.name.lowercase()
                    val fmt = buildFormat(m, c)
                    applyLowLatencyOptions(fmt, decName, tryNumber)
                    c.configure(fmt, s, null, 0)
                    c.start()
                    codec = c
                    configured = true
                    Log.i(TAG, "slot $slot decoder=${c.name} configured (try=$tryNumber, m=$m)")
                    return
                } catch (e: Exception) {
                    lastErr = e
                    try { c?.release() } catch (_: Exception) {}
                }
            }
            Log.e(TAG, "slot $slot configure failed after retries: $m", lastErr)
            emitDecoderError()
            throw lastErr ?: RuntimeException("configure failed")
        }

        /**
         * A10: choose the best decoder name for [m]. Enumerate the REGULAR_CODECS list, skip
         * Google/AOSP software decoders (c2.android.* / omx.google.*), and prefer a hardware-
         * accelerated one. Returns the chosen name, or null to fall back to createDecoderByType.
         * Picked by NAME (not createDecoderByType) so the vendor low-latency keying — which
         * switches on the decoder name — still targets the HW decoder.
         */
        private fun pickDecoderName(m: String): String? {
            return try {
                var fallback: String? = null
                for (info in MediaCodecList(MediaCodecList.REGULAR_CODECS).codecInfos) {
                    if (info.isEncoder) continue
                    if (!info.supportedTypes.any { it.equals(m, ignoreCase = true) }) continue
                    val lower = info.name.lowercase()
                    if (lower.startsWith("c2.android.") || lower.startsWith("omx.google.")) continue // software
                    // isHardwareAccelerated is API 29+; on older APIs accept any non-software codec.
                    val hw = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) info.isHardwareAccelerated else true
                    if (hw) return info.name
                    if (fallback == null) fallback = info.name
                }
                fallback
            } catch (e: Exception) {
                Log.w(TAG, "slot $slot pickDecoderName error", e); null
            }
        }

        /** Base MediaFormat (csd + HDR color) — rebuilt fresh for each configure attempt.
         *  Takes the chosen [decoder] so it can query adaptive-playback support (A21). */
        private fun buildFormat(m: String, decoder: MediaCodec): MediaFormat {
            // Real stream dimensions from the SPS. Hardcoding 1920x1080 here broke ANY larger
            // stream on Qualcomm: a 2560x1440 host produced a steady 'QC2V4L2 Unsupported
            // input buffer' reject storm (works completing with no output picture) that
            // wedged the inflight gate at ~4 fps — the "frozen video" bug.
            val dims = when {
                isAv1()  -> null // decoder reads dimensions from the sequence header csd
                isH265() -> sps?.let { hevcSpsDims(it) }
                else     -> sps?.let { h264SpsDims(it) }
            }
            if (dims != null) Log.i(TAG, "slot $slot SPS dims ${dims.first}x${dims.second}")
            else Log.w(TAG, "slot $slot SPS dims unknown ($m) — adaptive max disabled")
            val fmt = MediaFormat.createVideoFormat(m, dims?.first ?: 1920, dims?.second ?: 1080)
            when {
                isAv1()  -> fmt.setByteBuffer("csd-0", ByteBuffer.wrap(av1SeqHeader!!))
                isH265() -> fmt.setByteBuffer("csd-0", ByteBuffer.wrap(withStart(vps!!) + withStart(sps!!) + withStart(pps!!)))
                else     -> { fmt.setByteBuffer("csd-0", ByteBuffer.wrap(withStart(sps!!))); fmt.setByteBuffer("csd-1", ByteBuffer.wrap(withStart(pps!!))) }
            }
            hdrMediaFormatHints?.let { hints ->
                if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.N) {
                    hints.colorStandard?.let { fmt.setInteger(MediaFormat.KEY_COLOR_STANDARD, it) }
                    hints.colorTransfer?.let { fmt.setInteger(MediaFormat.KEY_COLOR_TRANSFER, it) }
                    hints.colorRange?.let   { fmt.setInteger(MediaFormat.KEY_COLOR_RANGE,    it) }
                }
            }
            // A21: if the decoder supports adaptive playback, declare the max resolution +
            // a frame-rate hint up front. Then a mid-session host resolution change
            // (Parsec-style) is absorbed IN-PLACE by the decoder instead of a multi-100ms
            // teardown+reconfigure (a long freeze/green). Max = the actual stream size
            // (with 1080p as a floor for headroom on small streams); when the SPS parse
            // fails, DON'T declare a max — a too-small max makes Qualcomm reject every
            // input buffer, which is far worse than losing in-place res switches.
            try {
                val caps = decoder.codecInfo.getCapabilitiesForType(m)
                if (dims != null && caps != null && caps.isFeatureSupported(MediaCodecInfo.CodecCapabilities.FEATURE_AdaptivePlayback)) {
                    fmt.setInteger(MediaFormat.KEY_MAX_WIDTH, maxOf(dims.first, 1920))
                    fmt.setInteger(MediaFormat.KEY_MAX_HEIGHT, maxOf(dims.second, 1080))
                    // Frame-rate hint = the panel refresh rate (best available "fps"; the decode
                    // path carries no explicit stream fps). Best-effort, defaults to 60.
                    val fps = try {
                        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.R) (activity.display?.refreshRate ?: 60f).toInt()
                        else @Suppress("DEPRECATION") activity.windowManager.defaultDisplay.refreshRate.toInt()
                    } catch (_: Exception) { 60 }
                    fmt.setInteger(MediaFormat.KEY_FRAME_RATE, fps.coerceIn(24, 240))
                }
            } catch (_: Exception) {} // best-effort; never block configure() on the adaptive hint
            return fmt
        }

        /**
         * Port of Moonlight MediaCodecHelper.setDecoderLowLatencyOptions: apply low-latency
         * MediaFormat keys most→least aggressive (lower tryNumber = more keys). Vendor keys are
         * gated on the decoder name prefix. Unknown keys are usually ignored; ones that make
         * configure() throw are dropped by the next retry.
         */
        private fun applyLowLatencyOptions(fmt: MediaFormat, decName: String, tryNumber: Int) {
            if (tryNumber < 1 && Build.VERSION.SDK_INT >= Build.VERSION_CODES.R) {
                fmt.setInteger(MediaFormat.KEY_LOW_LATENCY, 1)
            }
            if (tryNumber < 2) {
                fmt.setInteger("vdec-lowlatency", 1) // MediaTek / Amlogic legacy AOSP key
            }
            if (tryNumber < 3 && Build.VERSION.SDK_INT >= Build.VERSION_CODES.M) {
                // Decode ASAP — don't pace output to the (faked 30fps) input PTS.
                fmt.setInteger(MediaFormat.KEY_OPERATING_RATE, Short.MAX_VALUE.toInt())
                fmt.setInteger(MediaFormat.KEY_PRIORITY, 0)
            }
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
                when {
                    decName.startsWith("omx.qcom") || decName.startsWith("c2.qti") -> {
                        // Qualcomm (Snapdragon) — THE keys that fix HEVC/AVC decoder buffering.
                        if (tryNumber < 4) fmt.setInteger("vendor.qti-ext-dec-picture-order.enable", 1)
                        if (tryNumber < 5) fmt.setInteger("vendor.qti-ext-dec-low-latency.enable", 1)
                    }
                    decName.startsWith("omx.hisi") || decName.startsWith("c2.hisi") -> {
                        if (tryNumber < 4) {
                            fmt.setInteger("vendor.hisi-ext-low-latency-video-dec.video-scene-for-low-latency-req", 1)
                            fmt.setInteger("vendor.hisi-ext-low-latency-video-dec.video-scene-for-low-latency-rdy", -1)
                        }
                    }
                    decName.startsWith("omx.exynos") || decName.startsWith("c2.exynos") -> {
                        if (tryNumber < 4) fmt.setInteger("vendor.rtc-ext-dec-low-latency.enable", 1)
                    }
                    decName.startsWith("omx.amlogic") || decName.startsWith("c2.amlogic") -> {
                        if (tryNumber < 4) fmt.setInteger("vendor.low-latency.enable", 1)
                    }
                }
            }
        }

        // ── Decode pipeline counters (cheap longs; NO per-frame allocation) ─────
        private val decInfo = MediaCodec.BufferInfo() // hoisted (was per-call alloc → GC)
        // @Volatile: framesQueued is written by the decode thread, framesOut by the render
        // thread, and the inflight gate reads both cross-thread.
        @Volatile private var framesQueued = 0L
        @Volatile private var framesOut = 0L
        // framesQueued/framesOut are DIAG-only now (the inflight feed gate was removed —
        // it wedged on works that complete without an output picture).
        private var lastDiagOut = 0L
        private var diagLastNs = 0L

        private fun feedAndDrain(au: ByteArray) {
            val c = codec ?: return
            var inIdx = c.dequeueInputBuffer(10_000)
            if (inIdx < 0) {
                // Input pool momentarily full. The render thread drains output concurrently
                // (which frees input buffers), so wait briefly + retry rather than dropping the
                // AU (a lost P/I-frame = green until the next host GOP). Do NOT drain output
                // here — only the render thread may touch the output side (concurrent
                // dequeueOutputBuffer from two threads crashes MediaCodec).
                inIdx = c.dequeueInputBuffer(10_000)
                if (inIdx < 0) { emitDecoderError(); return }
            }
            val buf = c.getInputBuffer(inIdx)!!
            buf.clear(); buf.put(au)
            // Real-time PTS (µs): monotonic + reflects the TRUE cadence, so a decoder that
            // paces output by PTS runs at the real fps. The old fixed 33_333 (=30fps) tag
            // mis-paced a 60/120fps stream. (Render still uses present-now, not PTS.)
            pts = maxOf(pts + 1, System.nanoTime() / 1000)
            c.queueInputBuffer(inIdx, 0, au.size, pts, 0)
            framesQueued++
            startRenderThread() // idempotent — the output drain/present runs on its own thread
            // Light diag (~2s): output fps + in-flight depth. No per-frame allocation.
            val now = System.nanoTime()
            if (diagLastNs == 0L) { diagLastNs = now; lastDiagOut = framesOut }
            else if (now - diagLastNs > 2_000_000_000L) {
                val secs = (now - diagLastNs) / 1e9
                Log.i(TAG, "slot $slot DIAG outFps=${"%.1f".format((framesOut - lastDiagOut) / secs)} inflight=${framesQueued - framesOut}")
                diagLastNs = now; lastDiagOut = framesOut
            }
        }

        /**
         * Moonlight model (MediaCodecDecoderRenderer): drain to the NEWEST decoded frame and
         * render only that, dropping every older frame with releaseOutputBuffer(idx, false).
         * Rendering every queued frame lets a decode/network burst pile up in the SurfaceView's
         * BufferQueue — each frame then waits a vsync, so end-to-end latency grows without bound
         * and never recovers (the "buffer keeps accumulating → jumps to ~0.5 s" symptom).
         * Keeping only the latest bounds latency to ~1 frame after any burst.
         * (Surface-output decoders write pixels straight to the Surface, so info.size is 0 for a
         * real frame — never gate render on info.size, that renders nothing → green surface.)
         *
         * A3: also called before retrying a failed input dequeue — releasing output buffers here
         * is what frees the decoder's input buffers.
         */
        private fun drainOutputAndRender() {
            val c = codec ?: return
            var renderIdx = -1
            while (true) {
                val outIdx = c.dequeueOutputBuffer(decInfo, 0)
                when {
                    outIdx >= 0 -> {
                        framesOut++
                        if (renderIdx >= 0) c.releaseOutputBuffer(renderIdx, false) // drop the older frame
                        renderIdx = outIdx
                    }
                    outIdx == MediaCodec.INFO_OUTPUT_FORMAT_CHANGED -> { lastFormat = c.outputFormat; applyAspect(c.outputFormat) }
                    else -> break
                }
            }
            // Render with an explicit present timestamp of "now" (Moonlight latency mode),
            // NOT releaseOutputBuffer(idx, true). The boolean variant queues frames to the
            // SurfaceView's BufferQueue in order with no dropping, so a burst piles up and each
            // frame waits a vsync → latency accumulates and never recovers. Submitting an explicit
            // present time lets SurfaceFlinger drop superseded frames (only the latest before each
            // vsync is shown), so latency stays bounded.
            if (renderIdx >= 0) c.releaseOutputBuffer(renderIdx, System.nanoTime())
        }

        // W5-native: HDR hints to apply during configure()
        private var hdrMediaFormatHints: HdrHints? = null

        /**
         * W5-native: Switch this pane's video surface and MediaFormat HDR mode.
         * mode: "sdr" | "hdr10" | "hlg"
         */
        fun setHdrMode(mode: String) {
            hdrMediaFormatHints = when (mode) {
                "hdr10" -> HdrHints(
                    colorStandard = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.N) MediaFormat.COLOR_STANDARD_BT2020 else null,
                    colorTransfer  = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.N) MediaFormat.COLOR_TRANSFER_ST2084 else null,
                    colorRange     = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.N) MediaFormat.COLOR_RANGE_FULL else null,
                )
                "hlg" -> HdrHints(
                    colorStandard = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.N) MediaFormat.COLOR_STANDARD_BT2020 else null,
                    colorTransfer  = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.N) MediaFormat.COLOR_TRANSFER_HLG else null,
                    colorRange     = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.N) MediaFormat.COLOR_RANGE_FULL else null,
                )
                else -> null // SDR — no HDR flags
            }

            // Update the Window color mode (API 26+) so the display pipeline
            // knows to apply HDR tone-mapping / wide colour gamut.
            // SurfaceView has no setColorMode — the color mode is a Window attribute.
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
                try {
                    activity.window.colorMode = when (mode) {
                        "hdr10", "hlg" -> ActivityInfo.COLOR_MODE_HDR
                        else           -> ActivityInfo.COLOR_MODE_DEFAULT
                    }
                } catch (_: Exception) {} // best-effort; some devices/configs may reject
            }

            // Also update the Surface's data space (API 33+) for immediate colour
            // pipeline hint — the compositor uses this to route the surface to the
            // right HW path before the first decoded frame arrives.
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU) {
                val holder = (surfaceView as? SurfaceView)?.holder
                if (holder != null) {
                    val dataSpace = when (mode) {
                        "hdr10" -> android.hardware.DataSpace.DATASPACE_BT2020_PQ
                        "hlg"   -> android.hardware.DataSpace.DATASPACE_BT2020_HLG
                        else    -> android.hardware.DataSpace.DATASPACE_SRGB
                    }
                    holder.surface?.let {
                        try {
                            // Surface.setDataSpace is a hidden API on some builds — use reflection
                            // as a best-effort; failures here are non-fatal.
                            val m = Surface::class.java.getMethod("setDataSpace", Int::class.javaPrimitiveType)
                            m.invoke(it, dataSpace)
                        } catch (_: Exception) {}
                    }
                }
            }

            Log.i(TAG, "slot $slot HDR mode set: $mode (hints=$hdrMediaFormatHints)")
        }

        /**
         * W5-native: Emit a `decoder-error` event to the webview so the JS/Rust layer
         * can trigger keyframe recovery (W5-decoder-recovery).
         * Payload: { slot: Int }
         */
        private fun emitDecoderError() {
            try {
                val payload = JSObject()
                payload.put("slot", slot)
                trigger("decoder-error", payload)
            } catch (e: Exception) {
                Log.w(TAG, "slot $slot emitDecoderError failed", e)
            }
        }

        /**
         * Compute the base (un-zoomed) destination rect for the current aspect mode
         * and apply it, then notify JS of the video size. A FORMAT_CHANGED resets to
         * fit; an active pinch is re-driven by JS via `set_video_transform` on the
         * next gesture / `video-size` event (last-writer-wins on the normalized rect).
         */
        fun applyAspect(of: MediaFormat) {
            if (splitMode) return // split sizing is owned by relayout()
            val vw = if (of.containsKey("crop-right") && of.containsKey("crop-left")) of.getInteger("crop-right") - of.getInteger("crop-left") + 1 else of.getInteger(MediaFormat.KEY_WIDTH)
            val vh = if (of.containsKey("crop-bottom") && of.containsKey("crop-top")) of.getInteger("crop-bottom") - of.getInteger("crop-top") + 1 else of.getInteger(MediaFormat.KEY_HEIGHT)
            if (vw <= 0 || vh <= 0) return
            videoW = vw; videoH = vh
            activity.runOnUiThread {
                val sv = surfaceView ?: return@runOnUiThread
                val parent = sv.parent as? ViewGroup ?: return@runOnUiThread
                val pw = parent.width; val ph = parent.height
                if (pw == 0 || ph == 0) return@runOnUiThread
                // Base dest rect in pixels per aspect mode, then normalize to [0..1].
                val (dw, dh) = when (aspectMode[slot]) {
                    "fill"    -> { val s = maxOf(pw.toFloat() / vw, ph.toFloat() / vh); Pair(vw * s, vh * s) }
                    "stretch" -> Pair(pw.toFloat(), ph.toFloat())
                    else      -> { val s = minOf(pw.toFloat() / vw, ph.toFloat() / vh); Pair(vw * s, vh * s) }
                }
                tnW = dw / pw
                tnH = dh / ph
                tnX = (pw - dw) / 2f / pw
                tnY = (ph - dh) / 2f / ph
                applyTransform()
                Log.i(TAG, "slot $slot video ${vw}x${vh} mode=${aspectMode[slot]} -> rect=($tnX,$tnY,$tnW,$tnH)")
            }
            // Tell JS the video size so it can map touches + drive aspect-correct zoom.
            try {
                val payload = JSObject()
                payload.put("slot", slot); payload.put("vw", vw); payload.put("vh", vh)
                trigger("video-size", payload)
            } catch (_: Exception) {}
        }

        private fun releaseCodec() {
            stopRenderThread() // join the output-drainer BEFORE releasing the codec (no race)
            try { codec?.stop() } catch (_: Exception) {}
            try { codec?.release() } catch (_: Exception) {}
            codec = null; configured = false
            // KEEP vps/sps/pps/av1SeqHeader: retaining the collected parameter sets lets the
            // very NEXT AU reconfigure a fresh codec after a decoder failure. Clearing them
            // forced recovery to wait for the next in-band parameter sets, which on the NVENC
            // host ride only inside IDR AUs (safety GOP 10 s) → black/frozen picture for up to
            // 10 s after any decoder hiccup. arm() (codec change) and release() (teardown)
            // clear them instead.
            // Reset the DIAG counters: frames queued into the OLD codec never produce output
            // from the NEW one.
            framesQueued = 0; framesOut = 0; lastDiagOut = 0; diagLastNs = 0
        }

        fun release() {
            stopAuSocket()
            stopDecoderThread()
            cannedThread?.interrupt(); cannedThread = null
            synchronized(lock) {
                releaseCodec(); mime = null
                vps = null; sps = null; pps = null; av1SeqHeader = null
            }
            surfaceView?.let { (it.parent as? ViewGroup)?.removeView(it) }
            surfaceView = null; surface = null; lastFormat = null
        }

        // canned clip (M3), slot 0 only
        fun playCanned() {
            val s = surface
            if (s != null) startCanned(s) else pendingCanned = true
        }

        private fun startCanned(s: Surface) {
            if (cannedThread != null) return
            val t = Thread {
                try {
                    val file = copyAssetToCache("m3_test.mp4")
                    while (!Thread.currentThread().isInterrupted) decodeOnce(file, s)
                } catch (_: InterruptedException) {} catch (e: Exception) { Log.e(TAG, "canned error", e) }
            }
            t.isDaemon = true; t.start(); cannedThread = t
        }

        private fun decodeOnce(file: File, s: Surface) {
            val ex = MediaExtractor(); ex.setDataSource(file.absolutePath)
            var track = -1; var fmt: MediaFormat? = null
            for (i in 0 until ex.trackCount) {
                val f = ex.getTrackFormat(i)
                if (f.getString(MediaFormat.KEY_MIME)?.startsWith("video/") == true) { track = i; fmt = f; break }
            }
            if (track < 0 || fmt == null) { ex.release(); return }
            ex.selectTrack(track)
            val c = MediaCodec.createDecoderByType(fmt.getString(MediaFormat.KEY_MIME)!!)
            c.configure(fmt, s, null, 0); c.start()
            // Record the size + apply aspect-fit so the canned/test clip honours aspect
            // and `getVideoSize` reports real dims (the live feed path does this in its
            // FORMAT_CHANGED branch; the canned loop has the format up-front).
            lastFormat = fmt; applyAspect(fmt)
            val info = MediaCodec.BufferInfo(); var inEos = false; var outEos = false; val startNs = System.nanoTime()
            while (!outEos && !Thread.currentThread().isInterrupted) {
                if (!inEos) {
                    val i = c.dequeueInputBuffer(10_000)
                    if (i >= 0) {
                        val n = ex.readSampleData(c.getInputBuffer(i)!!, 0)
                        if (n < 0) { c.queueInputBuffer(i, 0, 0, 0, MediaCodec.BUFFER_FLAG_END_OF_STREAM); inEos = true }
                        else { c.queueInputBuffer(i, 0, n, ex.sampleTime, 0); ex.advance() }
                    }
                }
                val o = c.dequeueOutputBuffer(info, 10_000)
                if (o >= 0) {
                    val wait = (info.presentationTimeUs - (System.nanoTime() - startNs) / 1000) / 1000
                    if (wait in 1..50) Thread.sleep(wait)
                    c.releaseOutputBuffer(o, info.size > 0)
                    if (info.flags and MediaCodec.BUFFER_FLAG_END_OF_STREAM != 0) outEos = true
                }
            }
            c.stop(); c.release(); ex.release()
        }
    }

    // ---------------- shared helpers ----------------

    private fun withStart(nal: ByteArray): ByteArray {
        val out = ByteArray(4 + nal.size); out[3] = 1; System.arraycopy(nal, 0, out, 4, nal.size); return out
    }

    private fun splitAnnexB(data: ByteArray): List<ByteArray> {
        val nals = ArrayList<ByteArray>(); val n = data.size; var p = 0; var start = -1
        while (p < n) {
            val sl = startCodeLen(data, p, n)
            if (sl > 0) { if (start in 0 until p) nals.add(data.copyOfRange(start, p)); p += sl; start = p } else p++
        }
        if (start in 0 until n) nals.add(data.copyOfRange(start, n))
        return nals
    }

    private fun startCodeLen(d: ByteArray, at: Int, n: Int): Int {
        if (at + 2 < n && d[at].toInt() == 0 && d[at + 1].toInt() == 0 && d[at + 2].toInt() == 1) return 3
        if (at + 3 < n && d[at].toInt() == 0 && d[at + 1].toInt() == 0 && d[at + 2].toInt() == 0 && d[at + 3].toInt() == 1) return 4
        return 0
    }

    private fun copyAssetToCache(name: String): File {
        val out = File(activity.cacheDir, name)
        if (!out.exists() || out.length() == 0L) activity.assets.open(name).use { i -> out.outputStream().use { i.copyTo(it) } }
        return out
    }
}
