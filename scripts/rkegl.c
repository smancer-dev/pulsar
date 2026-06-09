// rkegl.c — proof-of-concept: zero-copy rkmpp → DRM_PRIME → EGL render on RK3588.
//
// Proves the Moonlight path that mpv 0.34's gpu VO can't do: decode H.264 with the
// hardware rkmpp decoder (frames come out as AV_PIX_FMT_DRM_PRIME = dmabuf in GPU
// memory), import each frame's dmabuf straight into an EGLImage
// (EGL_LINUX_DMA_BUF_EXT), bind it as a GL_TEXTURE_EXTERNAL_OES and draw it — NO
// GPU→CPU download. Measures sustained fps so we can see whether the Pi really does
// 1080p/1440p at high fps (it does in Moonlight) before porting this to Rust/Pulsar.
//
// Build (on the Pi):
//   gcc -O2 -o rkegl rkegl.c $(pkg-config --cflags --libs libavformat libavcodec libavutil) -lEGL -lGLESv2 -lX11
// Run:
//   ./rkegl test1080.h264        # loops a local .h264 file → max render throughput
//   ./rkegl stream.sdp           # an RTP/H.264 SDP (live, like Pulsar)
//
// (Linking -lEGL/-lGLESv2 directly is fine HERE — no WebKitGTK in this process. The
//  Pulsar binary must instead dlopen them at runtime; see native_view.rs / build.rs.)

#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>

#include <X11/Xlib.h>
#include <EGL/egl.h>
#include <EGL/eglext.h>
#include <GLES2/gl2.h>
#include <GLES2/gl2ext.h>

#include <libavformat/avformat.h>
#include <libavcodec/avcodec.h>
#include <libavutil/hwcontext.h>
#include <libavutil/hwcontext_drm.h>
#include <libavutil/pixdesc.h>

#ifndef DRM_FORMAT_MOD_INVALID
#define DRM_FORMAT_MOD_INVALID ((1ULL << 56) - 1)
#endif

static const char *VERT =
    "attribute vec2 pos;\n"
    "attribute vec2 uvin;\n"
    "varying vec2 uv;\n"
    "void main(){ uv = uvin; gl_Position = vec4(pos, 0.0, 1.0); }\n";

static const char *FRAG =
    "#extension GL_OES_EGL_image_external : require\n"
    "precision mediump float;\n"
    "varying vec2 uv;\n"
    "uniform samplerExternalOES tex;\n"
    "void main(){ gl_FragColor = texture2D(tex, uv); }\n";

static GLuint build_shader(GLenum type, const char *src) {
    GLuint s = glCreateShader(type);
    glShaderSource(s, 1, &src, NULL);
    glCompileShader(s);
    GLint ok = 0;
    glGetShaderiv(s, GL_COMPILE_STATUS, &ok);
    if (!ok) {
        char log[1024];
        glGetShaderInfoLog(s, sizeof(log), NULL, log);
        fprintf(stderr, "shader compile failed: %s\n", log);
        exit(1);
    }
    return s;
}

// Steer the rkmpp decoder to keep frames in DRM_PRIME (the GPU dmabuf) instead of
// RGA/software-converting them to yuv420p. Without this avcodec picks a software output
// format and you get "Doing slow software conversion" — the whole point is to avoid that.
static enum AVPixelFormat get_drm_prime(AVCodecContext *c, const enum AVPixelFormat *fmts) {
    (void)c;
    for (const enum AVPixelFormat *p = fmts; *p != AV_PIX_FMT_NONE; p++)
        if (*p == AV_PIX_FMT_DRM_PRIME) return *p;
    return fmts[0];
}

static double now_s(void) {
    struct timespec ts;
    clock_gettime(CLOCK_MONOTONIC, &ts);
    return ts.tv_sec + ts.tv_nsec / 1e9;
}

// EGL/GL entry points resolved at runtime.
static PFNEGLCREATEIMAGEKHRPROC eglCreateImageKHR_;
static PFNEGLDESTROYIMAGEKHRPROC eglDestroyImageKHR_;
static PFNGLEGLIMAGETARGETTEXTURE2DOESPROC glEGLImageTargetTexture2DOES_;

int main(int argc, char **argv) {
    if (argc < 2) {
        fprintf(stderr, "usage: %s <file.h264|stream.sdp>\n", argv[0]);
        return 1;
    }
    const char *input = argv[1];
    int is_sdp = strstr(input, ".sdp") != NULL;

    // ---- X11 window ----
    Display *xd = XOpenDisplay(NULL);
    if (!xd) { fprintf(stderr, "XOpenDisplay failed\n"); return 1; }
    int screen = DefaultScreen(xd);
    int W = 1280, H = 720;
    Window win = XCreateSimpleWindow(xd, RootWindow(xd, screen), 0, 0, W, H, 0,
                                     BlackPixel(xd, screen), BlackPixel(xd, screen));
    XStoreName(xd, win, "rkegl");
    XMapWindow(xd, win);
    XFlush(xd);

    // ---- EGL ----
    EGLDisplay dpy = eglGetDisplay((EGLNativeDisplayType)xd);
    EGLint vmaj, vmin;
    if (!eglInitialize(dpy, &vmaj, &vmin)) { fprintf(stderr, "eglInitialize failed\n"); return 1; }
    fprintf(stderr, "EGL %d.%d  exts: %s\n", vmaj, vmin, eglQueryString(dpy, EGL_EXTENSIONS));
    eglBindAPI(EGL_OPENGL_ES_API);
    EGLint cfgattr[] = { EGL_RENDERABLE_TYPE, EGL_OPENGL_ES2_BIT, EGL_SURFACE_TYPE, EGL_WINDOW_BIT,
                         EGL_RED_SIZE, 8, EGL_GREEN_SIZE, 8, EGL_BLUE_SIZE, 8, EGL_NONE };
    EGLConfig cfg; EGLint ncfg;
    if (!eglChooseConfig(dpy, cfgattr, &cfg, 1, &ncfg) || ncfg < 1) { fprintf(stderr, "eglChooseConfig failed\n"); return 1; }
    EGLSurface surf = eglCreateWindowSurface(dpy, cfg, (EGLNativeWindowType)win, NULL);
    EGLint ctxattr[] = { EGL_CONTEXT_CLIENT_VERSION, 2, EGL_NONE };
    EGLContext ctx = eglCreateContext(dpy, cfg, EGL_NO_CONTEXT, ctxattr);
    eglMakeCurrent(dpy, surf, surf, ctx);
    eglSwapInterval(dpy, 0); // no vsync → measure true throughput

    eglCreateImageKHR_ = (PFNEGLCREATEIMAGEKHRPROC)eglGetProcAddress("eglCreateImageKHR");
    eglDestroyImageKHR_ = (PFNEGLDESTROYIMAGEKHRPROC)eglGetProcAddress("eglDestroyImageKHR");
    glEGLImageTargetTexture2DOES_ = (PFNGLEGLIMAGETARGETTEXTURE2DOESPROC)eglGetProcAddress("glEGLImageTargetTexture2DOES");
    if (!eglCreateImageKHR_ || !glEGLImageTargetTexture2DOES_) {
        fprintf(stderr, "missing eglCreateImageKHR / glEGLImageTargetTexture2DOES\n"); return 1;
    }

    // ---- GL program + fullscreen quad ----
    GLuint prog = glCreateProgram();
    glAttachShader(prog, build_shader(GL_VERTEX_SHADER, VERT));
    glAttachShader(prog, build_shader(GL_FRAGMENT_SHADER, FRAG));
    glBindAttribLocation(prog, 0, "pos");
    glBindAttribLocation(prog, 1, "uvin");
    glLinkProgram(prog);
    glUseProgram(prog);
    // pos.xy, uv.xy — two triangles, uv flipped vertically (GL origin bottom-left)
    static const GLfloat quad[] = {
        -1,-1, 0,1,   1,-1, 1,1,   1,1, 1,0,
        -1,-1, 0,1,   1,1, 1,0,   -1,1, 0,0,
    };
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
    glUniform1i(glGetUniformLocation(prog, "tex"), 0);

    // ---- libavformat input ----
    AVFormatContext *fmt = NULL;
    AVDictionary *opts = NULL;
    if (is_sdp) {
        av_dict_set(&opts, "protocol_whitelist", "file,rtp,udp", 0);
        av_dict_set(&opts, "fflags", "nobuffer", 0);
        av_dict_set(&opts, "flags", "low_delay", 0);
    }
    if (avformat_open_input(&fmt, input, NULL, &opts) < 0) { fprintf(stderr, "open_input failed\n"); return 1; }
    avformat_find_stream_info(fmt, NULL);
    int vs = av_find_best_stream(fmt, AVMEDIA_TYPE_VIDEO, -1, -1, NULL, 0);
    if (vs < 0) { fprintf(stderr, "no video stream\n"); return 1; }

    const AVCodec *dec = avcodec_find_decoder_by_name("h264_rkmpp");
    if (!dec) { fprintf(stderr, "h264_rkmpp decoder not found\n"); return 1; }
    AVCodecContext *dc = avcodec_alloc_context3(dec);
    avcodec_parameters_to_context(dc, fmt->streams[vs]->codecpar);
    dc->get_format = get_drm_prime; // keep frames zero-copy in DRM_PRIME
    if (avcodec_open2(dc, dec, NULL) < 0) { fprintf(stderr, "avcodec_open2 failed\n"); return 1; }

    AVPacket *pkt = av_packet_alloc();
    AVFrame *frame = av_frame_alloc();
    long frames = 0; double t0 = now_s(), tlast = t0;
    int logged_fmt = 0;

    for (;;) {
        int r = av_read_frame(fmt, pkt);
        if (r < 0) {
            if (is_sdp) break;                 // stream ended
            av_seek_frame(fmt, vs, 0, AVSEEK_FLAG_BACKWARD); // loop the file
            avcodec_flush_buffers(dc);
            continue;
        }
        if (pkt->stream_index != vs) { av_packet_unref(pkt); continue; }
        if (avcodec_send_packet(dc, pkt) < 0) { av_packet_unref(pkt); continue; }
        av_packet_unref(pkt);

        while (avcodec_receive_frame(dc, frame) == 0) {
            if (frame->format != AV_PIX_FMT_DRM_PRIME) {
                if (!logged_fmt) { fprintf(stderr, "frame format = %s (NOT drm_prime!)\n",
                                           av_get_pix_fmt_name(frame->format)); logged_fmt = 1; }
                av_frame_unref(frame);
                continue;
            }
            AVDRMFrameDescriptor *d = (AVDRMFrameDescriptor*)frame->data[0];
            if (!logged_fmt) {
                fprintf(stderr, "DRM_PRIME ok: %dx%d nb_layers=%d layer0.fourcc=%.4s nb_planes=%d mod=%llx\n",
                        frame->width, frame->height, d->nb_layers, (char*)&d->layers[0].format,
                        d->layers[0].nb_planes, (unsigned long long)d->objects[0].format_modifier);
                logged_fmt = 1;
            }

            // Build the dmabuf import attribs (composed, single EGLImage).
            EGLint a[64]; int n = 0;
            a[n++] = EGL_LINUX_DRM_FOURCC_EXT;  a[n++] = (EGLint)d->layers[0].format;
            a[n++] = EGL_WIDTH;                 a[n++] = frame->width;
            a[n++] = EGL_HEIGHT;                a[n++] = frame->height;
            const EGLint plane_fd[4]   = { EGL_DMA_BUF_PLANE0_FD_EXT, EGL_DMA_BUF_PLANE1_FD_EXT, EGL_DMA_BUF_PLANE2_FD_EXT, EGL_DMA_BUF_PLANE3_FD_EXT };
            const EGLint plane_off[4]  = { EGL_DMA_BUF_PLANE0_OFFSET_EXT, EGL_DMA_BUF_PLANE1_OFFSET_EXT, EGL_DMA_BUF_PLANE2_OFFSET_EXT, EGL_DMA_BUF_PLANE3_OFFSET_EXT };
            const EGLint plane_pitch[4]= { EGL_DMA_BUF_PLANE0_PITCH_EXT, EGL_DMA_BUF_PLANE1_PITCH_EXT, EGL_DMA_BUF_PLANE2_PITCH_EXT, EGL_DMA_BUF_PLANE3_PITCH_EXT };
            const EGLint plane_ml[4]   = { EGL_DMA_BUF_PLANE0_MODIFIER_LO_EXT, EGL_DMA_BUF_PLANE1_MODIFIER_LO_EXT, EGL_DMA_BUF_PLANE2_MODIFIER_LO_EXT, EGL_DMA_BUF_PLANE3_MODIFIER_LO_EXT };
            const EGLint plane_mh[4]   = { EGL_DMA_BUF_PLANE0_MODIFIER_HI_EXT, EGL_DMA_BUF_PLANE1_MODIFIER_HI_EXT, EGL_DMA_BUF_PLANE2_MODIFIER_HI_EXT, EGL_DMA_BUF_PLANE3_MODIFIER_HI_EXT };
            for (int i = 0; i < d->layers[0].nb_planes; i++) {
                AVDRMPlaneDescriptor *p = &d->layers[0].planes[i];
                AVDRMObjectDescriptor *o = &d->objects[p->object_index];
                a[n++] = plane_fd[i];    a[n++] = o->fd;
                a[n++] = plane_off[i];   a[n++] = (EGLint)p->offset;
                a[n++] = plane_pitch[i]; a[n++] = (EGLint)p->pitch;
                if (o->format_modifier != DRM_FORMAT_MOD_INVALID) {
                    a[n++] = plane_ml[i]; a[n++] = (EGLint)(o->format_modifier & 0xFFFFFFFF);
                    a[n++] = plane_mh[i]; a[n++] = (EGLint)(o->format_modifier >> 32);
                }
            }
            a[n++] = EGL_NONE;

            EGLImageKHR img = eglCreateImageKHR_(dpy, EGL_NO_CONTEXT, EGL_LINUX_DMA_BUF_EXT, NULL, a);
            if (img == EGL_NO_IMAGE_KHR) {
                fprintf(stderr, "eglCreateImageKHR failed: 0x%x\n", eglGetError());
                av_frame_unref(frame);
                continue;
            }

            glViewport(0, 0, W, H);
            glBindTexture(GL_TEXTURE_EXTERNAL_OES, tex);
            glEGLImageTargetTexture2DOES_(GL_TEXTURE_EXTERNAL_OES, img);
            glDrawArrays(GL_TRIANGLES, 0, 6);
            eglSwapBuffers(dpy, surf);
            eglDestroyImageKHR_(dpy, img);
            av_frame_unref(frame);

            frames++;
            double t = now_s();
            if (t - tlast >= 1.0) {
                fprintf(stderr, "fps=%.1f  (%ld frames in %.1fs)\n", frames / (t - t0), frames, t - t0);
                tlast = t;
            }
        }
    }
    fprintf(stderr, "done: %ld frames, avg %.1f fps\n", frames, frames / (now_s() - t0));
    return 0;
}
