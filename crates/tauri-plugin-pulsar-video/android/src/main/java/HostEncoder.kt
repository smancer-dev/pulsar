package dev.pulsar.video

import android.annotation.SuppressLint
import android.hardware.display.DisplayManager
import android.hardware.display.VirtualDisplay
import android.media.AudioAttributes
import android.media.AudioFormat
import android.media.AudioPlaybackCaptureConfiguration
import android.media.AudioRecord
import android.media.MediaCodec
import android.media.MediaCodecInfo
import android.media.MediaCodecList
import android.media.MediaFormat
import android.media.projection.MediaProjection
import android.os.Build
import android.os.Handler
import android.os.Looper
import android.util.Log
import android.view.Surface
import java.net.DatagramPacket
import java.net.DatagramSocket
import java.net.InetAddress

/**
 * Mobile HOST (task #16, Stage 2): mirror the screen via MediaProjection into a
 * MediaCodec ENCODER, packetize the encoded H.264/H.265 to RTP, and send it to a
 * loopback UDP port the Rust side forwards into the encrypted session.
 *
 * W5-native addition: companion object [probeCodecs] enumerates available hardware
 * video encoders for the `host_codecs` probe (W5-host-codecprobe).
 */
class HostEncoder(
    private val projection: MediaProjection,
    private val mime: String,
    private val width: Int,
    private val height: Int,
    private val fps: Int,
    private val bitrate: Int,
    private val densityDpi: Int,
    private val port: Int,
    private val audioPort: Int,
) {
    private val TAG = "PulsarHost"
    private var encoder: MediaCodec? = null
    private var inputSurface: Surface? = null
    private var vdisplay: VirtualDisplay? = null
    private var thread: Thread? = null
    private var audioRecord: AudioRecord? = null
    private var audioEncoder: MediaCodec? = null
    private var audioThread: Thread? = null
    @Volatile private var running = false

    fun start() {
        val fmt = MediaFormat.createVideoFormat(mime, width, height).apply {
            setInteger(MediaFormat.KEY_COLOR_FORMAT, MediaCodecInfo.CodecCapabilities.COLOR_FormatSurface)
            setInteger(MediaFormat.KEY_BIT_RATE, bitrate)
            setInteger(MediaFormat.KEY_FRAME_RATE, fps)
            setInteger(MediaFormat.KEY_I_FRAME_INTERVAL, 1)
            // Repeat SPS/PPS on EVERY keyframe (every 1s). The desktop's ffmpeg/pulsar-render
            // demuxer can't decode ("unspecified size") until it sees the parameter sets; if its
            // renderer starts late or restarts on a re-stream it misses the one-shot SPS and
            // freezes. Prepending headers to sync frames makes the stream self-describing.
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) {
                setInteger(MediaFormat.KEY_PREPEND_HEADER_TO_SYNC_FRAMES, 1)
            }
            // A surface-input encoder only emits a frame when the captured screen CHANGES.
            // A static screen would otherwise produce nothing after the first keyframe → the
            // remote sees a frozen image. Repeat the last frame every ~frame-interval so the
            // stream keeps flowing (repeated static frames compress to almost nothing).
            setLong(MediaFormat.KEY_REPEAT_PREVIOUS_FRAME_AFTER, 1_000_000L / fps)
        }
        val codec = MediaCodec.createEncoderByType(mime)
        codec.configure(fmt, null, null, MediaCodec.CONFIGURE_FLAG_ENCODE)
        val surface = codec.createInputSurface()
        codec.start()
        encoder = codec
        inputSurface = surface

        // API 34+ requires a registered callback before createVirtualDisplay.
        projection.registerCallback(object : MediaProjection.Callback() {}, Handler(Looper.getMainLooper()))
        // FLAG_PUBLIC (the standard MediaProjection screen-capture flag) keeps capturing
        // when our app is backgrounded; AUTO_MIRROR couples to the source display state and
        // pauses on the emulator when the app leaves the foreground → remote freeze.
        vdisplay = projection.createVirtualDisplay(
            "pulsar-host", width, height, densityDpi,
            DisplayManager.VIRTUAL_DISPLAY_FLAG_PUBLIC, surface, null, null
        )

        running = true
        thread = Thread { drainLoop(codec) }.also { it.isDaemon = true; it.start() }
        Log.i(TAG, "host encoder: $mime ${width}x${height}@$fps ${bitrate}bps -> 127.0.0.1:$port")

        if (audioPort > 0) startAudio()
    }

    /** Capture the device's playback audio (MediaProjection), Opus-encode, RTP → audio port. */
    @SuppressLint("MissingPermission") // playback capture is gated by the projection, not RECORD_AUDIO
    private fun startAudio() {
        try {
            val config = AudioPlaybackCaptureConfiguration.Builder(projection)
                .addMatchingUsage(AudioAttributes.USAGE_MEDIA)
                .addMatchingUsage(AudioAttributes.USAGE_GAME)
                .addMatchingUsage(AudioAttributes.USAGE_UNKNOWN)
                .build()
            val afmt = AudioFormat.Builder()
                .setEncoding(AudioFormat.ENCODING_PCM_16BIT)
                .setSampleRate(48000)
                .setChannelMask(AudioFormat.CHANNEL_IN_STEREO)
                .build()
            val minBuf = AudioRecord.getMinBufferSize(48000, AudioFormat.CHANNEL_IN_STEREO, AudioFormat.ENCODING_PCM_16BIT)
            val rec = AudioRecord.Builder()
                .setAudioFormat(afmt)
                .setBufferSizeInBytes(maxOf(minBuf, 8192))
                .setAudioPlaybackCaptureConfig(config)
                .build()
            rec.startRecording()
            audioRecord = rec

            val efmt = MediaFormat.createAudioFormat(MediaFormat.MIMETYPE_AUDIO_OPUS, 48000, 2).apply {
                setInteger(MediaFormat.KEY_BIT_RATE, 96_000)
            }
            val enc = MediaCodec.createEncoderByType(MediaFormat.MIMETYPE_AUDIO_OPUS)
            enc.configure(efmt, null, null, MediaCodec.CONFIGURE_FLAG_ENCODE)
            enc.start()
            audioEncoder = enc

            audioThread = Thread { audioLoop(rec, enc) }.also { it.isDaemon = true; it.start() }
            Log.i(TAG, "host audio: opus 48000/2 -> 127.0.0.1:$audioPort")
        } catch (e: Exception) {
            Log.e(TAG, "audio start failed (no playback-capture / opus encoder)", e)
        }
    }

    private fun audioLoop(rec: AudioRecord, enc: MediaCodec) {
        val sock = DatagramSocket()
        val addr = InetAddress.getByName("127.0.0.1")
        val info = MediaCodec.BufferInfo()
        var aseq = 1
        val assrc = byteArrayOf(0x50, 0x55, 0x4C, 0x41) // "PULA"
        try {
            while (running) {
                val inIdx = enc.dequeueInputBuffer(10_000)
                if (inIdx >= 0) {
                    val ib = enc.getInputBuffer(inIdx)
                    if (ib != null) {
                        ib.clear()
                        val n = rec.read(ib, ib.capacity())
                        enc.queueInputBuffer(inIdx, 0, if (n > 0) n else 0, System.nanoTime() / 1000, 0)
                    }
                }
                var oi = enc.dequeueOutputBuffer(info, 0)
                while (oi >= 0) {
                    val ob = enc.getOutputBuffer(oi)
                    if (ob != null && info.size > 0 && (info.flags and MediaCodec.BUFFER_FLAG_CODEC_CONFIG) == 0) {
                        val pkt = ByteArray(info.size); ob.position(info.offset); ob.get(pkt)
                        val ts = ((info.presentationTimeUs * 48) / 1000).toInt() // 48 kHz Opus clock
                        val rtpPkt = rtp(aseq, ts, true, assrc, pkt, 97)
                        try { sock.send(DatagramPacket(rtpPkt, rtpPkt.size, addr, audioPort)) } catch (_: Exception) {}
                        aseq++
                    }
                    enc.releaseOutputBuffer(oi, false)
                    oi = enc.dequeueOutputBuffer(info, 0)
                }
            }
        } catch (e: Exception) {
            Log.e(TAG, "audio loop error", e)
        } finally {
            try { sock.close() } catch (_: Exception) {}
        }
    }

    private fun drainLoop(codec: MediaCodec) {
        val sock = DatagramSocket()
        val addr = InetAddress.getByName("127.0.0.1")
        val info = MediaCodec.BufferInfo()
        var seq = 1
        val ssrc = byteArrayOf(0x50, 0x55, 0x4C, 0x53) // "PULS"
        try {
            while (running) {
                val outIdx = codec.dequeueOutputBuffer(info, 50_000)
                if (outIdx < 0) continue
                val buf = codec.getOutputBuffer(outIdx)
                if (buf != null && info.size > 0) {
                    val data = ByteArray(info.size)
                    buf.position(info.offset); buf.get(data)
                    val ts = ((info.presentationTimeUs * 90) / 1000).toInt() // 90 kHz RTP clock
                    val nals = splitNals(data)
                    for ((i, nal) in nals.withIndex()) {
                        seq = packetize(sock, addr, nal, ts, seq, ssrc, i == nals.lastIndex)
                    }
                }
                codec.releaseOutputBuffer(outIdx, false)
                if (info.flags and MediaCodec.BUFFER_FLAG_END_OF_STREAM != 0) break
            }
        } catch (e: Exception) {
            Log.e(TAG, "drain error", e)
        } finally {
            try { sock.close() } catch (_: Exception) {}
        }
    }

    private fun splitNals(d: ByteArray): List<ByteArray> {
        val out = ArrayList<ByteArray>(); val n = d.size; var p = 0; var start = -1
        fun sc(at: Int): Int {
            if (at + 2 < n && d[at].toInt() == 0 && d[at + 1].toInt() == 0 && d[at + 2].toInt() == 1) return 3
            if (at + 3 < n && d[at].toInt() == 0 && d[at + 1].toInt() == 0 && d[at + 2].toInt() == 0 && d[at + 3].toInt() == 1) return 4
            return 0
        }
        while (p < n) { val s = sc(p); if (s > 0) { if (start in 0 until p) out.add(d.copyOfRange(start, p)); p += s; start = p } else p++ }
        if (start in 0 until n) out.add(d.copyOfRange(start, n))
        return out
    }

    private val MAX = 1100

    /** RTP-packetize one NAL: single packet if small, else FU-A (H.264) / FU (H.265). */
    private fun packetize(sock: DatagramSocket, addr: InetAddress, nal: ByteArray, ts: Int, seqIn: Int, ssrc: ByteArray, lastNal: Boolean): Int {
        var seq = seqIn
        if (nal.size <= MAX) {
            send(sock, addr, rtp(seq, ts, lastNal, ssrc, nal)); return seq + 1
        }
        val h265 = mime == "video/hevc"
        if (h265) {
            val b0 = nal[0].toInt() and 0xFF; val b1 = nal[1].toInt()
            val type = (b0 shr 1) and 0x3F
            val hdr0 = ((b0 and 0x81) or (49 shl 1)).toByte()
            var off = 2; var first = true
            while (off < nal.size) {
                val chunk = minOf(MAX, nal.size - off); val last = off + chunk >= nal.size
                val fu = ByteArray(3 + chunk)
                fu[0] = hdr0; fu[1] = b1.toByte()
                fu[2] = (((if (first) 0x80 else 0) or (if (last) 0x40 else 0) or type)).toByte()
                System.arraycopy(nal, off, fu, 3, chunk)
                send(sock, addr, rtp(seq, ts, last && lastNal, ssrc, fu)); seq++; off += chunk; first = false
            }
        } else {
            val b0 = nal[0].toInt() and 0xFF
            val type = b0 and 0x1F
            val ind = ((b0 and 0xE0) or 28).toByte()
            var off = 1; var first = true
            while (off < nal.size) {
                val chunk = minOf(MAX, nal.size - off); val last = off + chunk >= nal.size
                val fu = ByteArray(2 + chunk)
                fu[0] = ind
                fu[1] = (((if (first) 0x80 else 0) or (if (last) 0x40 else 0) or type)).toByte()
                System.arraycopy(nal, off, fu, 2, chunk)
                send(sock, addr, rtp(seq, ts, last && lastNal, ssrc, fu)); seq++; off += chunk; first = false
            }
        }
        return seq
    }

    private fun rtp(seq: Int, ts: Int, marker: Boolean, ssrc: ByteArray, payload: ByteArray, pt: Int = 96): ByteArray {
        val pkt = ByteArray(12 + payload.size)
        pkt[0] = 0x80.toByte()
        pkt[1] = ((if (marker) 0x80 else 0) or pt).toByte()
        pkt[2] = ((seq shr 8) and 0xFF).toByte(); pkt[3] = (seq and 0xFF).toByte()
        pkt[4] = ((ts shr 24) and 0xFF).toByte(); pkt[5] = ((ts shr 16) and 0xFF).toByte()
        pkt[6] = ((ts shr 8) and 0xFF).toByte(); pkt[7] = (ts and 0xFF).toByte()
        System.arraycopy(ssrc, 0, pkt, 8, 4)
        System.arraycopy(payload, 0, pkt, 12, payload.size)
        return pkt
    }

    private fun send(sock: DatagramSocket, addr: InetAddress, pkt: ByteArray) {
        try { sock.send(DatagramPacket(pkt, pkt.size, addr, port)) } catch (_: Exception) {}
    }

    companion object {
        /**
         * W5-native: Probe the MediaCodecList for available **encoder** MIME types that
         * Pulsar supports as streaming codecs. Returns a list of human-friendly names in
         * preference order (best/most capable first): ["h265", "h264"].
         *
         * Called by `PulsarVideoPlugin.enumerateDecoders` (Kotlin) and mirrored on the
         * Rust side by `mobile.rs::enumerate_host_codecs` for the `host_codecs` command
         * (W5-rust-host lane).
         *
         * AV1 encoding is NOT included: most Android devices lack a hardware AV1 encoder
         * at sufficient frame rates, and software AV1 encoding is too slow for streaming.
         * The decoder side (`enumerateDecoders`) may report AV1 if the device has HW
         * decode capability.
         */
        fun probeCodecs(): List<String> {
            val supported = mutableSetOf<String>()
            try {
                val list = MediaCodecList(MediaCodecList.REGULAR_CODECS)
                for (info in list.codecInfos) {
                    if (!info.isEncoder) continue
                    for (mime in info.supportedTypes) {
                        when (mime.lowercase()) {
                            "video/hevc" -> supported.add("h265")
                            "video/avc"  -> supported.add("h264")
                            else -> {}
                        }
                    }
                }
            } catch (e: Exception) {
                Log.w("PulsarHost", "probeCodecs error", e)
            }
            // Return in preference order (h265 before h264)
            return listOf("h265", "h264").filter { it in supported }
        }
    }

    fun stop() {
        running = false
        thread?.interrupt(); thread = null
        audioThread?.interrupt(); audioThread = null
        try { audioRecord?.stop() } catch (_: Exception) {}
        try { audioRecord?.release() } catch (_: Exception) {}
        try { audioEncoder?.stop() } catch (_: Exception) {}
        try { audioEncoder?.release() } catch (_: Exception) {}
        try { vdisplay?.release() } catch (_: Exception) {}
        try { encoder?.stop() } catch (_: Exception) {}
        try { encoder?.release() } catch (_: Exception) {}
        try { inputSurface?.release() } catch (_: Exception) {}
        // NOTE: do NOT stop the projection here — a re-stream (adaptive bitrate) recreates the
        // encoder via startCapture(), which stops THIS encoder then builds a new one on the SAME
        // projection. Stopping the projection here would kill that reused projection → freeze.
        // The projection is torn down only in the plugin's stopHost (session end).
        vdisplay = null; encoder = null; inputSurface = null
        audioRecord = null; audioEncoder = null
    }
}
