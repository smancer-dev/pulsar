// pulsar-vidsink.c — Pulsar's native zero-copy video sink for Linux/RK3588.
//
// Replaces the `mpv --wid` subprocess. Decodes the RTP/H.264 stream with the hardware
// rkmpp decoder (frames stay in GPU memory as AV_PIX_FMT_DRM_PRIME), imports each frame's
// dmabuf directly into an EGLImage (EGL_LINUX_DMA_BUF_EXT) and draws it as a
// GL_TEXTURE_EXTERNAL_OES — NO GPU->CPU download (the thing mpv 0.34's gpu VO can't do).
// This is Moonlight's path; PoC `rkegl.c` measured 468fps@1080p / 264fps@1440p on this Pi.
//
// Pacing follows Moonlight's pacer: each present first drains the UDP socket AND the decoder
// to the NEWEST frame, drops everything older, and draws only that one (vsync'd swap = the
// frame clock). So an occasional decode/draw spike costs one skipped frame, not a permanent
// latency ratchet -> Moonlight-class low latency, at native resolution.
//
// Spawned by Pulsar exactly like mpv was:
//   pulsar-vidsink <stream.sdp> --wid 0x<parent-xid> [--stats]
//   pulsar-vidsink <stream.sdp>            # own window (standalone test)
//
// Build (Pi):  gcc -O2 -o pulsar-vidsink pulsar-vidsink.c \
//                $(pkg-config --cflags --libs libavformat libavcodec libavutil) -lEGL -lGLESv2 -lX11 -lpthread
// (Linking EGL/GL here is fine — separate process, no WebKitGTK. Pulsar itself must not.)

#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>
#include <signal.h>
#include <unistd.h> // usleep
#include <pthread.h> // decode thread + frame mailbox

#include <X11/Xlib.h>
#include <X11/Xutil.h>
#include <EGL/egl.h>
#include <EGL/eglext.h>
#include <GLES2/gl2.h>
#include <GLES2/gl2ext.h>

#include <libavformat/avformat.h>
#include <libavcodec/avcodec.h>
#include <libavutil/hwcontext_drm.h>
#include <libavutil/pixdesc.h>

#ifndef DRM_FORMAT_MOD_INVALID
#define DRM_FORMAT_MOD_INVALID ((1ULL << 56) - 1)
#endif

static volatile sig_atomic_t g_stop = 0;
static void on_signal(int s) { (void)s; g_stop = 1; }
// Overlay "corner mode": Pulsar sends SIGUSR1 when the gaming overlay opens (shrink to a
// corner so the webview menu shows beside the STILL-RUNNING video → FPS/stats stay live)
// and SIGUSR2 when it closes (restore fullscreen). Replaces the old kill-on-overlay, which
// froze the video + FPS.
static volatile sig_atomic_t g_corner = 0;
static void on_usr(int s) { g_corner = (s == SIGUSR1) ? 1 : 0; }

static const char *VERT =
    "attribute vec2 pos;\n attribute vec2 uvin;\n varying vec2 uv;\n"
    "void main(){ uv = uvin; gl_Position = vec4(pos, 0.0, 1.0); }\n";
static const char *FRAG =
    "#extension GL_OES_EGL_image_external : require\n"
    "precision mediump float;\n varying vec2 uv;\n uniform samplerExternalOES tex;\n"
    // alpha = 1.0 so the (possibly ARGB) window presents fully opaque over the webview.
    "void main(){ gl_FragColor = vec4(texture2D(tex, uv).rgb, 1.0); }\n";

// --- Native overlay HUD (drawn over the live video; webview can't composite over the opaque
// child window on this GTK/WebKitGTK stack, so the HUD is rendered here, Moonlight-style) -----
// Solid-colour program (dim layer + HUD panel) and a text program sampling an 8x8 bitmap font.
static const char *VERT_S =
    "attribute vec2 pos;\n void main(){ gl_Position = vec4(pos, 0.0, 1.0); }\n";
static const char *FRAG_S =
    "precision mediump float;\n uniform vec4 ucol;\n void main(){ gl_FragColor = ucol; }\n";
static const char *VERT_T =
    "attribute vec2 pos;\n attribute vec2 uvin;\n varying vec2 uv;\n"
    "void main(){ uv = uvin; gl_Position = vec4(pos, 0.0, 1.0); }\n";
static const char *FRAG_T =
    "precision mediump float;\n varying vec2 uv;\n uniform sampler2D font;\n uniform vec4 ucol;\n"
    "void main(){ float a = texture2D(font, uv).a; gl_FragColor = vec4(ucol.rgb, ucol.a*a); }\n";

// font8x8_basic (public domain, dhepper/font8x8) — ASCII 0x20..0x7F, one byte per row, LSB=left.
static const unsigned char FONT8X8[96][8] = {
{0,0,0,0,0,0,0,0},{0x18,0x3C,0x3C,0x18,0x18,0,0x18,0},{0x36,0x36,0,0,0,0,0,0},
{0x36,0x36,0x7F,0x36,0x7F,0x36,0x36,0},{0x0C,0x3E,0x03,0x1E,0x30,0x1F,0x0C,0},{0,0x63,0x33,0x18,0x0C,0x66,0x63,0},
{0x1C,0x36,0x1C,0x6E,0x3B,0x33,0x6E,0},{0x06,0x06,0x03,0,0,0,0,0},{0x18,0x0C,0x06,0x06,0x06,0x0C,0x18,0},
{0x06,0x0C,0x18,0x18,0x18,0x0C,0x06,0},{0,0x66,0x3C,0xFF,0x3C,0x66,0,0},{0,0x0C,0x0C,0x3F,0x0C,0x0C,0,0},
{0,0,0,0,0,0x0C,0x0C,0x06},{0,0,0,0x3F,0,0,0,0},{0,0,0,0,0,0x0C,0x0C,0},
{0x60,0x30,0x18,0x0C,0x06,0x03,0x01,0},{0x3E,0x63,0x73,0x7B,0x6F,0x67,0x3E,0},{0x0C,0x0E,0x0C,0x0C,0x0C,0x0C,0x3F,0},
{0x1E,0x33,0x30,0x1C,0x06,0x33,0x3F,0},{0x1E,0x33,0x30,0x1C,0x30,0x33,0x1E,0},{0x38,0x3C,0x36,0x33,0x7F,0x30,0x78,0},
{0x3F,0x03,0x1F,0x30,0x30,0x33,0x1E,0},{0x1C,0x06,0x03,0x1F,0x33,0x33,0x1E,0},{0x3F,0x33,0x30,0x18,0x0C,0x0C,0x0C,0},
{0x1E,0x33,0x33,0x1E,0x33,0x33,0x1E,0},{0x1E,0x33,0x33,0x3E,0x30,0x18,0x0E,0},{0,0x0C,0x0C,0,0,0x0C,0x0C,0},
{0,0x0C,0x0C,0,0,0x0C,0x0C,0x06},{0x18,0x0C,0x06,0x03,0x06,0x0C,0x18,0},{0,0,0x3F,0,0,0x3F,0,0},
{0x06,0x0C,0x18,0x30,0x18,0x0C,0x06,0},{0x1E,0x33,0x30,0x18,0x0C,0,0x0C,0},{0x3E,0x63,0x7B,0x7B,0x7B,0x03,0x1E,0},
{0x0C,0x1E,0x33,0x33,0x3F,0x33,0x33,0},{0x3F,0x66,0x66,0x3E,0x66,0x66,0x3F,0},{0x3C,0x66,0x03,0x03,0x03,0x66,0x3C,0},
{0x1F,0x36,0x66,0x66,0x66,0x36,0x1F,0},{0x7F,0x46,0x16,0x1E,0x16,0x46,0x7F,0},{0x7F,0x46,0x16,0x1E,0x16,0x06,0x0F,0},
{0x3C,0x66,0x03,0x03,0x73,0x66,0x7C,0},{0x33,0x33,0x33,0x3F,0x33,0x33,0x33,0},{0x1E,0x0C,0x0C,0x0C,0x0C,0x0C,0x1E,0},
{0x78,0x30,0x30,0x30,0x33,0x33,0x1E,0},{0x67,0x66,0x36,0x1E,0x36,0x66,0x67,0},{0x0F,0x06,0x06,0x06,0x46,0x66,0x7F,0},
{0x63,0x77,0x7F,0x7F,0x6B,0x63,0x63,0},{0x63,0x67,0x6F,0x7B,0x73,0x63,0x63,0},{0x1C,0x36,0x63,0x63,0x63,0x36,0x1C,0},
{0x3F,0x66,0x66,0x3E,0x06,0x06,0x0F,0},{0x1E,0x33,0x33,0x33,0x3B,0x1E,0x38,0},{0x3F,0x66,0x66,0x3E,0x36,0x66,0x67,0},
{0x1E,0x33,0x07,0x0E,0x38,0x33,0x1E,0},{0x3F,0x2D,0x0C,0x0C,0x0C,0x0C,0x1E,0},{0x33,0x33,0x33,0x33,0x33,0x33,0x3F,0},
{0x33,0x33,0x33,0x33,0x33,0x1E,0x0C,0},{0x63,0x63,0x63,0x6B,0x7F,0x77,0x63,0},{0x63,0x63,0x36,0x1C,0x1C,0x36,0x63,0},
{0x33,0x33,0x33,0x1E,0x0C,0x0C,0x1E,0},{0x7F,0x63,0x31,0x18,0x4C,0x66,0x7F,0},{0x1E,0x06,0x06,0x06,0x06,0x06,0x1E,0},
{0x03,0x06,0x0C,0x18,0x30,0x60,0x40,0},{0x1E,0x18,0x18,0x18,0x18,0x18,0x1E,0},{0x08,0x1C,0x36,0x63,0,0,0,0},
{0,0,0,0,0,0,0,0xFF},{0x0C,0x0C,0x18,0,0,0,0,0},{0,0,0x1E,0x30,0x3E,0x33,0x6E,0},
{0x07,0x06,0x06,0x3E,0x66,0x66,0x3B,0},{0,0,0x1E,0x33,0x03,0x33,0x1E,0},{0x38,0x30,0x30,0x3E,0x33,0x33,0x6E,0},
{0,0,0x1E,0x33,0x3F,0x03,0x1E,0},{0x1C,0x36,0x06,0x0F,0x06,0x06,0x0F,0},{0,0,0x6E,0x33,0x33,0x3E,0x30,0x1F},
{0x07,0x06,0x36,0x6E,0x66,0x66,0x67,0},{0x0C,0,0x0E,0x0C,0x0C,0x0C,0x1E,0},{0x30,0,0x30,0x30,0x30,0x33,0x33,0x1E},
{0x07,0x06,0x66,0x36,0x1E,0x36,0x67,0},{0x0E,0x0C,0x0C,0x0C,0x0C,0x0C,0x1E,0},{0,0,0x33,0x7F,0x7F,0x6B,0x63,0},
{0,0,0x1F,0x33,0x33,0x33,0x33,0},{0,0,0x1E,0x33,0x33,0x33,0x1E,0},{0,0,0x3B,0x66,0x66,0x3E,0x06,0x0F},
{0,0,0x6E,0x33,0x33,0x3E,0x30,0x78},{0,0,0x3B,0x6E,0x66,0x06,0x0F,0},{0,0,0x3E,0x03,0x1E,0x30,0x1F,0},
{0x08,0x0C,0x3E,0x0C,0x0C,0x2C,0x18,0},{0,0,0x33,0x33,0x33,0x33,0x6E,0},{0,0,0x33,0x33,0x33,0x1E,0x0C,0},
{0,0,0x63,0x6B,0x7F,0x7F,0x36,0},{0,0,0x63,0x36,0x1C,0x36,0x63,0},{0,0,0x33,0x33,0x33,0x3E,0x30,0x1F},
{0,0,0x3F,0x19,0x0C,0x26,0x3F,0},{0x38,0x0C,0x0C,0x07,0x0C,0x0C,0x38,0},{0x18,0x18,0x18,0,0x18,0x18,0x18,0},
{0x07,0x0C,0x0C,0x38,0x0C,0x0C,0x07,0},{0x6E,0x3B,0,0,0,0,0,0},{0,0,0,0,0,0,0,0}};

static GLuint build_shader(GLenum type, const char *src) {
    GLuint s = glCreateShader(type);
    glShaderSource(s, 1, &src, NULL);
    glCompileShader(s);
    GLint ok = 0; glGetShaderiv(s, GL_COMPILE_STATUS, &ok);
    if (!ok) { char l[1024]; glGetShaderInfoLog(s, sizeof(l), NULL, l); fprintf(stderr, "vidsink: shader: %s\n", l); exit(2); }
    return s;
}
static double now_s(void) { struct timespec t; clock_gettime(CLOCK_MONOTONIC, &t); return t.tv_sec + t.tv_nsec/1e9; }

// Keep rkmpp output in DRM_PRIME (zero-copy) instead of RGA/software-converting to yuv420p.
static enum AVPixelFormat get_drm_prime(AVCodecContext *c, const enum AVPixelFormat *fmts) {
    (void)c;
    for (const enum AVPixelFormat *p = fmts; *p != AV_PIX_FMT_NONE; p++)
        if (*p == AV_PIX_FMT_DRM_PRIME) return *p;
    return fmts[0];
}

// --- Decode thread + frame mailbox -------------------------------------------------------
// ffmpeg's RTP/UDP demuxer IGNORES AVFMT_FLAG_NONBLOCK — av_read_frame blocks — so the
// vsync-paced render loop can't drain the socket itself without stalling on the read. Instead a
// dedicated decode thread does the blocking read+decode and publishes the freshest frame to a
// depth-1 mailbox (drop-oldest); the main thread presents the newest at vsync. This is
// Moonlight's decouple-decode-from-present model: an occasional slow present drops a stale frame
// instead of ratcheting latency, and the socket is always drained so it never overflows.
static pthread_mutex_t g_mbx_lock = PTHREAD_MUTEX_INITIALIZER;
static AVFrame *g_mbx = NULL;   // latest decoded frame awaiting draw (mailbox-owned)
static long g_dropped = 0;      // frames superseded in the mailbox before being drawn
static volatile long g_bytes = 0; // total payload bytes read (→ live bitrate for the HUD)
static double g_mbx_ts = 0;       // monotonic time the mailbox frame was published (→ HUD ms)

struct decode_ctx { AVFormatContext *fmt; AVCodecContext *dc; int vs; };

static void *decode_thread(void *arg) {
    struct decode_ctx *c = (struct decode_ctx *)arg;
    AVPacket *pkt = av_packet_alloc();
    AVFrame *frame = av_frame_alloc();
    while (!g_stop) {
        int r = av_read_frame(c->fmt, pkt);          // BLOCKING — fine on a dedicated thread
        if (r == AVERROR_EOF) { g_stop = 1; break; }
        if (r < 0) { av_packet_unref(pkt); continue; } // transient loss — keep going
        if (pkt->stream_index == c->vs) { g_bytes += pkt->size; avcodec_send_packet(c->dc, pkt); }
        av_packet_unref(pkt);
        while (avcodec_receive_frame(c->dc, frame) == 0) {
            if (frame->format != AV_PIX_FMT_DRM_PRIME) { av_frame_unref(frame); continue; }
            AVFrame *nf = av_frame_alloc();
            av_frame_move_ref(nf, frame);            // take ownership; frame emptied for reuse
            pthread_mutex_lock(&g_mbx_lock);
            if (g_mbx) { av_frame_free(&g_mbx); g_dropped++; } // drop the older undrawn frame
            g_mbx = nf;
            g_mbx_ts = now_s();
            pthread_mutex_unlock(&g_mbx_lock);
        }
    }
    av_packet_free(&pkt);
    av_frame_free(&frame);
    return NULL;
}

// ---- HUD rendering state + helpers (set up once, drawn each frame while the overlay is open) --
static GLuint g_progS, g_progT, g_fontTex;
static GLint g_sPos, g_sCol, g_tPos, g_tUv, g_tCol, g_tFont;
static int g_hudW, g_hudH; // window px the HUD maps NDC against (updated per frame)

static void hud_init(void) {
    g_progS = glCreateProgram();
    glAttachShader(g_progS, build_shader(GL_VERTEX_SHADER, VERT_S));
    glAttachShader(g_progS, build_shader(GL_FRAGMENT_SHADER, FRAG_S));
    glLinkProgram(g_progS);
    g_sPos = glGetAttribLocation(g_progS, "pos");
    g_sCol = glGetUniformLocation(g_progS, "ucol");

    g_progT = glCreateProgram();
    glAttachShader(g_progT, build_shader(GL_VERTEX_SHADER, VERT_T));
    glAttachShader(g_progT, build_shader(GL_FRAGMENT_SHADER, FRAG_T));
    glLinkProgram(g_progT);
    g_tPos = glGetAttribLocation(g_progT, "pos");
    g_tUv = glGetAttribLocation(g_progT, "uvin");
    g_tCol = glGetUniformLocation(g_progT, "ucol");
    g_tFont = glGetUniformLocation(g_progT, "font");

    // Build a 768x8 ALPHA atlas: 96 glyphs side by side, 8x8 each (LSB=left bit per row).
    unsigned char *atlas = (unsigned char *)calloc(768 * 8, 1);
    for (int gi = 0; gi < 96; gi++)
        for (int r = 0; r < 8; r++)
            for (int x = 0; x < 8; x++)
                if ((FONT8X8[gi][r] >> x) & 1) atlas[r * 768 + gi * 8 + x] = 0xFF;
    glGenTextures(1, &g_fontTex);
    glBindTexture(GL_TEXTURE_2D, g_fontTex);
    glPixelStorei(GL_UNPACK_ALIGNMENT, 1);
    glTexImage2D(GL_TEXTURE_2D, 0, GL_ALPHA, 768, 8, 0, GL_ALPHA, GL_UNSIGNED_BYTE, atlas);
    glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MIN_FILTER, GL_NEAREST);
    glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MAG_FILTER, GL_NEAREST);
    glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_WRAP_S, GL_CLAMP_TO_EDGE);
    glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_WRAP_T, GL_CLAMP_TO_EDGE);
    free(atlas);
}

// Solid rect in pixel coords (y down from top-left), colour rgba.
static void hud_rect(float px, float py, float w, float h, float r, float g, float b, float a) {
    float x0 = px / g_hudW * 2 - 1, x1 = (px + w) / g_hudW * 2 - 1;
    float y0 = 1 - py / g_hudH * 2, y1 = 1 - (py + h) / g_hudH * 2;
    float v[] = { x0, y0, x1, y0, x1, y1, x0, y0, x1, y1, x0, y1 };
    glUseProgram(g_progS);
    glUniform4f(g_sCol, r, g, b, a);
    glBindBuffer(GL_ARRAY_BUFFER, 0);
    glVertexAttribPointer(g_sPos, 2, GL_FLOAT, GL_FALSE, 0, v);
    glEnableVertexAttribArray(g_sPos);
    glDrawArrays(GL_TRIANGLES, 0, 6);
}

// Monospace text in pixel coords; `s` = glyph cell size (px). Returns nothing.
static void hud_text(float px, float py, float s, float r, float g, float b, float a, const char *str) {
    float verts[64 * 6 * 4]; // up to 64 glyphs * 6 verts * (x,y,u,v)
    int nv = 0; float cx = px;
    for (const char *c = str; *c && nv < 64 * 6 * 4 - 24; c++) {
        if (*c == ' ') { cx += s * 0.9f; continue; }
        int gi = (unsigned char)*c - 32; if (gi < 0 || gi >= 96) { cx += s * 0.9f; continue; }
        float u0 = (gi * 8) / 768.0f, u1 = (gi * 8 + 8) / 768.0f;
        float x0 = cx / g_hudW * 2 - 1, x1 = (cx + s) / g_hudW * 2 - 1;
        float y0 = 1 - py / g_hudH * 2, y1 = 1 - (py + s) / g_hudH * 2;
        float q[] = { x0,y0,u0,0, x1,y0,u1,0, x1,y1,u1,1, x0,y0,u0,0, x1,y1,u1,1, x0,y1,u0,1 };
        for (int i = 0; i < 24; i++) verts[nv++] = q[i];
        cx += s * 0.9f;
    }
    glUseProgram(g_progT);
    glUniform4f(g_tCol, r, g, b, a);
    glUniform1i(g_tFont, 0);
    glActiveTexture(GL_TEXTURE0);
    glBindTexture(GL_TEXTURE_2D, g_fontTex);
    glBindBuffer(GL_ARRAY_BUFFER, 0);
    glVertexAttribPointer(g_tPos, 2, GL_FLOAT, GL_FALSE, 4 * sizeof(float), verts);
    glVertexAttribPointer(g_tUv, 2, GL_FLOAT, GL_FALSE, 4 * sizeof(float), verts + 2);
    glEnableVertexAttribArray(g_tPos);
    glEnableVertexAttribArray(g_tUv);
    glDrawArrays(GL_TRIANGLES, 0, nv / 4);
}

static PFNEGLCREATEIMAGEKHRPROC eglCreateImageKHR_;
static PFNEGLDESTROYIMAGEKHRPROC eglDestroyImageKHR_;
static PFNGLEGLIMAGETARGETTEXTURE2DOESPROC glEGLImageTargetTexture2DOES_;

int main(int argc, char **argv) {
    const char *input = NULL;
    Window parent = 0;
    int want_stats = 0;
    int rotate = 0; // degrees clockwise to rotate the displayed video (0/90/180/270)
    for (int i = 1; i < argc; i++) {
        if (!strcmp(argv[i], "--wid") && i + 1 < argc) parent = (Window)strtoul(argv[++i], NULL, 0);
        else if (!strcmp(argv[i], "--stats")) want_stats = 1;
        else if (!strcmp(argv[i], "--rotate") && i + 1 < argc) rotate = ((atoi(argv[++i]) % 360) + 360) % 360;
        else if (!input) input = argv[i];
    }
    if (!input) { fprintf(stderr, "usage: pulsar-vidsink <stream.sdp> [--wid 0xXID] [--stats]\n"); return 1; }
    signal(SIGINT, on_signal); signal(SIGTERM, on_signal);
    signal(SIGUSR1, on_usr); signal(SIGUSR2, on_usr); // overlay corner-mode toggle

    Display *xd = XOpenDisplay(NULL);
    if (!xd) { fprintf(stderr, "vidsink: XOpenDisplay failed\n"); return 1; }
    int screen = DefaultScreen(xd);
    Window host = parent ? parent : RootWindow(xd, screen);
    int W = 1280, H = 720;
    if (parent) {
        Window r; int x, y; unsigned bw, d, pw, ph;
        XGetGeometry(xd, parent, &r, &x, &y, &pw, &ph, &bw, &d);
        W = (int)pw; H = (int)ph;
    }

    // ---- EGL / GLES2 (init BEFORE the window so we can match its visual to the config) ----
    EGLDisplay dpy = eglGetDisplay((EGLNativeDisplayType)xd);
    EGLint vmaj, vmin;
    if (!eglInitialize(dpy, &vmaj, &vmin)) { fprintf(stderr, "vidsink: eglInitialize failed\n"); return 1; }
    eglBindAPI(EGL_OPENGL_ES_API);
    EGLint cfgattr[] = { EGL_RENDERABLE_TYPE, EGL_OPENGL_ES2_BIT, EGL_SURFACE_TYPE, EGL_WINDOW_BIT,
                         EGL_RED_SIZE, 8, EGL_GREEN_SIZE, 8, EGL_BLUE_SIZE, 8, EGL_ALPHA_SIZE, 0, EGL_NONE };
    EGLConfig cfg; EGLint ncfg;
    if (!eglChooseConfig(dpy, cfgattr, &cfg, 1, &ncfg) || ncfg < 1) { fprintf(stderr, "vidsink: eglChooseConfig failed\n"); return 1; }

    // ---- X11 child window with the EGL config's OWN (opaque) visual ----
    // Inheriting the parent's visual via XCreateSimpleWindow gives a 32-bit ARGB window when
    // the Pulsar window is transparency-capable → the compositor blends it and the video shows
    // through. Create the window with the EGL config's native visual instead (opaque RGB).
    EGLint native_vid = 0;
    eglGetConfigAttrib(dpy, cfg, EGL_NATIVE_VISUAL_ID, &native_vid);
    XVisualInfo vtmpl; vtmpl.visualid = (VisualID)native_vid; int nvis = 0;
    XVisualInfo *vinfo = XGetVisualInfo(xd, VisualIDMask, &vtmpl, &nvis);
    Visual *visual = (vinfo && nvis > 0) ? vinfo->visual : DefaultVisual(xd, screen);
    int depth = (vinfo && nvis > 0) ? vinfo->depth : DefaultDepth(xd, screen);
    XSetWindowAttributes swa;
    swa.colormap = XCreateColormap(xd, host, visual, AllocNone);
    swa.background_pixel = 0;
    swa.border_pixel = 0;
    swa.event_mask = StructureNotifyMask;
    Window win = XCreateWindow(xd, host, 0, 0, W, H, 0, depth, InputOutput, visual,
                               CWColormap | CWBackPixel | CWBorderPixel | CWEventMask, &swa);
    if (vinfo) XFree(vinfo);
    if (!parent) XStoreName(xd, win, "pulsar-vidsink");
    XMapWindow(xd, win);
    XFlush(xd);

    EGLSurface surf = eglCreateWindowSurface(dpy, cfg, (EGLNativeWindowType)win, NULL);
    EGLint ctxattr[] = { EGL_CONTEXT_CLIENT_VERSION, 2, EGL_NONE };
    EGLContext ctx = eglCreateContext(dpy, cfg, EGL_NO_CONTEXT, ctxattr);
    eglMakeCurrent(dpy, surf, surf, ctx);
    // vsync: swapInterval=1 paces presentation to the display vblank so a COMPOSITED windowed
    // window doesn't drop/double frames unevenly (the "stutters but no latency" symptom). Costs
    // ~1 frame of latency (≈8 ms @120 Hz). swapInterval=0 = present immediately (can stutter under
    // a compositor, but no tearing in fullscreen/unredirected). Env-tunable: PULSAR_SWAP.
    int swap = 1;
    { const char *s = getenv("PULSAR_SWAP"); if (s) swap = atoi(s); }
    eglSwapInterval(dpy, swap);

    eglCreateImageKHR_ = (PFNEGLCREATEIMAGEKHRPROC)eglGetProcAddress("eglCreateImageKHR");
    eglDestroyImageKHR_ = (PFNEGLDESTROYIMAGEKHRPROC)eglGetProcAddress("eglDestroyImageKHR");
    glEGLImageTargetTexture2DOES_ = (PFNGLEGLIMAGETARGETTEXTURE2DOESPROC)eglGetProcAddress("glEGLImageTargetTexture2DOES");
    if (!eglCreateImageKHR_ || !glEGLImageTargetTexture2DOES_) { fprintf(stderr, "vidsink: missing EGL dmabuf import\n"); return 1; }

    GLuint prog = glCreateProgram();
    glAttachShader(prog, build_shader(GL_VERTEX_SHADER, VERT));
    glAttachShader(prog, build_shader(GL_FRAGMENT_SHADER, FRAG));
    glBindAttribLocation(prog, 0, "pos");
    glBindAttribLocation(prog, 1, "uvin");
    glLinkProgram(prog);
    glUseProgram(prog);
    GLfloat quad[] = {
        -1,-1, 0,1,   1,-1, 1,1,   1,1, 1,0,
        -1,-1, 0,1,   1,1, 1,0,   -1,1, 0,0,
    };
    // Rotate the displayed image `rotate`° CW (to match the host display orientation): spin
    // each vertex's UV around the texture centre (0.5,0.5). 180° fixes an upside-down host.
    if (rotate) {
        for (int v = 0; v < 6; v++) {
            float u = quad[v*4+2] - 0.5f, w = quad[v*4+3] - 0.5f, nu, nw;
            switch (rotate) {
                case 90:  nu =  w; nw = -u; break;
                case 180: nu = -u; nw = -w; break;
                case 270: nu = -w; nw =  u; break;
                default:  nu =  u; nw =  w; break;
            }
            quad[v*4+2] = nu + 0.5f;
            quad[v*4+3] = nw + 0.5f;
        }
    }
    GLuint vbo; glGenBuffers(1, &vbo);
    glBindBuffer(GL_ARRAY_BUFFER, vbo);
    glBufferData(GL_ARRAY_BUFFER, sizeof(quad), quad, GL_STATIC_DRAW);
    glVertexAttribPointer(0, 2, GL_FLOAT, GL_FALSE, 4*sizeof(GLfloat), (void*)0);
    glVertexAttribPointer(1, 2, GL_FLOAT, GL_FALSE, 4*sizeof(GLfloat), (void*)(2*sizeof(GLfloat)));
    glEnableVertexAttribArray(0);
    glEnableVertexAttribArray(1);
    GLuint tex; glGenTextures(1, &tex);
    glBindTexture(GL_TEXTURE_EXTERNAL_OES, tex);
    glTexParameteri(GL_TEXTURE_EXTERNAL_OES, GL_TEXTURE_MIN_FILTER, GL_LINEAR);
    glTexParameteri(GL_TEXTURE_EXTERNAL_OES, GL_TEXTURE_MAG_FILTER, GL_LINEAR);
    glTexParameteri(GL_TEXTURE_EXTERNAL_OES, GL_TEXTURE_WRAP_S, GL_CLAMP_TO_EDGE);
    glTexParameteri(GL_TEXTURE_EXTERNAL_OES, GL_TEXTURE_WRAP_T, GL_CLAMP_TO_EDGE);
    glUniform1i(glGetUniformLocation(prog, "tex"), 0);
    glClearColor(0, 0, 0, 1);
    hud_init(); // native overlay HUD (font atlas + solid/text programs)
    glBlendFunc(GL_SRC_ALPHA, GL_ONE_MINUS_SRC_ALPHA);

    // ---- libavformat input (RTP/H.264 over the SDP) ----
    AVFormatContext *fmt = NULL;
    AVDictionary *opts = NULL;
    av_dict_set(&opts, "protocol_whitelist", "file,rtp,udp", 0);
    av_dict_set(&opts, "fflags", "nobuffer+discardcorrupt", 0);
    av_dict_set(&opts, "flags", "low_delay", 0);
    av_dict_set(&opts, "reorder_queue_size", "0", 0);
    av_dict_set(&opts, "buffer_size", "1048576", 0); // 1 MiB socket; renderer drains it fast
    av_dict_set(&opts, "max_delay", "0", 0);
    if (avformat_open_input(&fmt, input, NULL, &opts) < 0) { fprintf(stderr, "vidsink: open_input failed\n"); return 1; }
    avformat_find_stream_info(fmt, NULL);
    int vs = av_find_best_stream(fmt, AVMEDIA_TYPE_VIDEO, -1, -1, NULL, 0);
    if (vs < 0) { fprintf(stderr, "vidsink: no video stream\n"); return 1; }

    const AVCodec *dec = avcodec_find_decoder_by_name("h264_rkmpp");
    if (!dec) dec = avcodec_find_decoder(fmt->streams[vs]->codecpar->codec_id);
    AVCodecContext *dc = avcodec_alloc_context3(dec);
    avcodec_parameters_to_context(dc, fmt->streams[vs]->codecpar);
    dc->get_format = get_drm_prime;
    // Size the rkmpp surface pool above our steady-state hold (newest + deferred + drain slack)
    // so draining several ready frames at once never starves the decoder.
    dc->extra_hw_frames = 8;
    if (avcodec_open2(dc, dec, NULL) < 0) { fprintf(stderr, "vidsink: avcodec_open2 failed\n"); return 1; }
    fprintf(stderr, "vidsink: decoder=%s, rendering into wid=0x%lx\n", dec->name, (unsigned long)win);

    // Decode runs on its own thread (blocking reads + decode → mailbox); the main thread below
    // presents the freshest frame at vsync. See the decode_thread comment above for why.
    struct decode_ctx dctx = { fmt, dc, vs };
    pthread_t dth;
    if (pthread_create(&dth, NULL, decode_thread, &dctx) != 0) {
        fprintf(stderr, "vidsink: pthread_create failed\n"); return 1;
    }

    AVFrame *deferred = NULL;   // frame shown last present; freed once the next one is up
    long frames = 0, flast = 0; double t0 = now_s(), tlast = t0;
    int vw = W, vh = H, geom_tick = 0;
    double hud_fps = 0, hud_mbit = 0, hud_ms = 0; // smoothed stats for the native HUD

    while (!g_stop) {
        double frame_ts = 0; // when this frame was published to the mailbox (→ HUD ms)
        // Take the freshest decoded frame; the decode thread already dropped any older ones.
        pthread_mutex_lock(&g_mbx_lock);
        AVFrame *cur = g_mbx; g_mbx = NULL; frame_ts = g_mbx_ts;
        pthread_mutex_unlock(&g_mbx_lock);
        if (!cur) { usleep(1000); continue; }   // nothing new yet — wait briefly

        // Overlay (SIGUSR1/2): the video stays FULLSCREEN and a native HUD is drawn on top
        // (Moonlight-style — the webview can't composite over this opaque child window). No
        // window resize; `g_corner` (set by the signal handler) just gates the HUD draw below.

        // Track parent window size (~every 30 frames) so the child fills it after a resize.
        if (parent && (geom_tick++ % 30) == 0) {
            Window rr; int xx, yy; unsigned pw, ph, bw, dd;
            if (XGetGeometry(xd, parent, &rr, &xx, &yy, &pw, &ph, &bw, &dd) && ((int)pw != W || (int)ph != H)) {
                W = (int)pw; H = (int)ph;
                XResizeWindow(xd, win, W, H);
            }
        }
        vw = cur->width; vh = cur->height;

        AVDRMFrameDescriptor *d = (AVDRMFrameDescriptor*)cur->data[0];
        EGLint a[64]; int n = 0;
        a[n++] = EGL_LINUX_DRM_FOURCC_EXT;  a[n++] = (EGLint)d->layers[0].format;
        a[n++] = EGL_WIDTH;                 a[n++] = vw;
        a[n++] = EGL_HEIGHT;                a[n++] = vh;
        const EGLint pf[4]={EGL_DMA_BUF_PLANE0_FD_EXT,EGL_DMA_BUF_PLANE1_FD_EXT,EGL_DMA_BUF_PLANE2_FD_EXT,EGL_DMA_BUF_PLANE3_FD_EXT};
        const EGLint po[4]={EGL_DMA_BUF_PLANE0_OFFSET_EXT,EGL_DMA_BUF_PLANE1_OFFSET_EXT,EGL_DMA_BUF_PLANE2_OFFSET_EXT,EGL_DMA_BUF_PLANE3_OFFSET_EXT};
        const EGLint pp[4]={EGL_DMA_BUF_PLANE0_PITCH_EXT,EGL_DMA_BUF_PLANE1_PITCH_EXT,EGL_DMA_BUF_PLANE2_PITCH_EXT,EGL_DMA_BUF_PLANE3_PITCH_EXT};
        const EGLint pl[4]={EGL_DMA_BUF_PLANE0_MODIFIER_LO_EXT,EGL_DMA_BUF_PLANE1_MODIFIER_LO_EXT,EGL_DMA_BUF_PLANE2_MODIFIER_LO_EXT,EGL_DMA_BUF_PLANE3_MODIFIER_LO_EXT};
        const EGLint ph_[4]={EGL_DMA_BUF_PLANE0_MODIFIER_HI_EXT,EGL_DMA_BUF_PLANE1_MODIFIER_HI_EXT,EGL_DMA_BUF_PLANE2_MODIFIER_HI_EXT,EGL_DMA_BUF_PLANE3_MODIFIER_HI_EXT};
        for (int i = 0; i < d->layers[0].nb_planes; i++) {
            AVDRMPlaneDescriptor *p = &d->layers[0].planes[i];
            AVDRMObjectDescriptor *o = &d->objects[p->object_index];
            a[n++]=pf[i]; a[n++]=o->fd;
            a[n++]=po[i]; a[n++]=(EGLint)p->offset;
            a[n++]=pp[i]; a[n++]=(EGLint)p->pitch;
            if (o->format_modifier != DRM_FORMAT_MOD_INVALID) {
                a[n++]=pl[i];  a[n++]=(EGLint)(o->format_modifier & 0xFFFFFFFF);
                a[n++]=ph_[i]; a[n++]=(EGLint)(o->format_modifier >> 32);
            }
        }
        a[n++] = EGL_NONE;

        EGLImageKHR img = eglCreateImageKHR_(dpy, EGL_NO_CONTEXT, EGL_LINUX_DMA_BUF_EXT, NULL, a);
        if (img == EGL_NO_IMAGE_KHR) { fprintf(stderr, "vidsink: eglCreateImageKHR 0x%x\n", eglGetError()); av_frame_free(&cur); continue; }

        // Letterbox: fit the video's aspect ratio inside the window. A 90°/270° rotation
        // swaps the effective width/height.
        int ew = vw, eh = vh;
        if (rotate == 90 || rotate == 270) { ew = vh; eh = vw; }
        int rw = W, rh = (int)((long)W * eh / ew);
        if (rh > H) { rh = H; rw = (int)((long)H * ew / eh); }
        glClear(GL_COLOR_BUFFER_BIT);
        glViewport((W - rw) / 2, (H - rh) / 2, rw, rh);
        // Re-bind the video program + its VBO attribs each frame (the HUD draw below leaves the
        // GL attrib pointers bound to its own client arrays).
        glUseProgram(prog);
        glBindBuffer(GL_ARRAY_BUFFER, vbo);
        glVertexAttribPointer(0, 2, GL_FLOAT, GL_FALSE, 4*sizeof(GLfloat), (void*)0);
        glVertexAttribPointer(1, 2, GL_FLOAT, GL_FALSE, 4*sizeof(GLfloat), (void*)(2*sizeof(GLfloat)));
        glEnableVertexAttribArray(0); glEnableVertexAttribArray(1);
        glBindTexture(GL_TEXTURE_EXTERNAL_OES, tex);
        glEGLImageTargetTexture2DOES_(GL_TEXTURE_EXTERNAL_OES, img);
        glDrawArrays(GL_TRIANGLES, 0, 6);

        // Live mailbox-wait latency: how long this frame sat before being presented (real ms).
        if (frame_ts > 0) { double d = (now_s() - frame_ts) * 1000.0; hud_ms = hud_ms*0.9 + d*0.1; }

        // ---- Native overlay HUD (drawn on the fullscreen video while the overlay is open) ----
        if (g_corner) {
            glViewport(0, 0, W, H);
            g_hudW = W; g_hudH = H;
            glEnable(GL_BLEND);
            hud_rect(0, 0, W, H, 0, 0, 0, 0.45f);            // dim the whole screen
            float pw = 360, ph = 196, pad = 22, lh = 30, s = 18;
            hud_rect(40, 40, pw, ph, 0.05f, 0.06f, 0.10f, 0.88f); // panel bg
            hud_rect(40, 40, pw, 4, 0.45f, 0.78f, 0.95f, 1.0f);   // cyan accent bar
            float tx = 40 + pad, ty = 40 + pad, R = 0.62f, G = 0.92f, B = 1.0f;
            char ln[80];
            hud_text(tx, ty, 22, R, G, B, 1.0f, "PULSAR"); ty += lh + 8;
            snprintf(ln, sizeof ln, "FPS      %.0f", hud_fps); hud_text(tx, ty, s, 1,1,1,1, ln); ty += lh;
            snprintf(ln, sizeof ln, "Buffer   %.0f ms", hud_ms); hud_text(tx, ty, s, 1,1,1,1, ln); ty += lh;
            snprintf(ln, sizeof ln, "Bitrate  %.1f Mbit", hud_mbit); hud_text(tx, ty, s, 1,1,1,1, ln); ty += lh;
            snprintf(ln, sizeof ln, "%dx%d", vw, vh); hud_text(tx, ty, s, 1,1,1,1, ln); ty += lh + 4;
            hud_text(tx, ty, 15, 0.6f,0.65f,0.7f,1.0f, "Ctrl+Shift+Q cik  M kapat");
            glDisable(GL_BLEND);
        }

        eglSwapBuffers(dpy, surf);          // vsync wait = our frame pacing
        eglDestroyImageKHR_(dpy, img);

        // Deferred free: release the frame shown LAST present (now safely off-screen) and hold
        // THIS one until the next — the rkmpp dmabuf may still be sampled by the GPU/display,
        // and returning it to the small pool early causes tearing/garbage.
        if (deferred) av_frame_free(&deferred);
        deferred = cur;

        frames++;
        {
            double t = now_s();
            if (t - tlast >= 1.0) {
                static long blast = 0;
                hud_fps = (frames - flast) / (t - tlast);
                hud_mbit = (g_bytes - blast) * 8.0 / 1e6 / (t - tlast);
                blast = g_bytes;
                if (want_stats) {
                    // fps WxH bitrate(Mbit/s) buffer(ms) → Pulsar perf HUD (overlay shows them).
                    printf("vidsink-fps %.1f %dx%d %.1f %.0f\n", hud_fps, vw, vh, hud_mbit, hud_ms);
                    fflush(stdout);
                    fprintf(stderr, "vidsink: %.1f fps %dx%d %.1fMbit (%ld dropped)\n",
                            hud_fps, vw, vh, hud_mbit, g_dropped); // → run log
                    fflush(stderr);
                }
                tlast = t; flast = frames;
            }
        }
    }

    g_stop = 1;
    pthread_join(dth, NULL);
    if (deferred) av_frame_free(&deferred);
    if (g_mbx) av_frame_free(&g_mbx);
    fprintf(stderr, "vidsink: exiting (%ld frames, %.1f fps avg)\n", frames, frames / (now_s() - t0));
    avcodec_free_context(&dc); avformat_close_input(&fmt);
    eglDestroyContext(dpy, ctx); eglDestroySurface(dpy, surf); eglTerminate(dpy);
    XDestroyWindow(xd, win); XCloseDisplay(xd);
    return 0;
}
