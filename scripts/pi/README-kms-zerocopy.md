# Pi (RK3588) zero-copy KMS capture — host setup

The 1080p120 game-streaming path on an Orange Pi 5 host captures the DRM
scanout framebuffer as a DMABuf (`kmssrc`) and feeds the fd straight into the
MPP encoder — no CPU copy. `ximagesrc` tops out around ~84 fps on the X server
copy alone; this path measured ~107 fps sustained (9.3 ms/frame encoder
spacing) at 1080p H.265 15 Mbit.

The app probes the whole chain at session start (`kms_probe_pipeline`) and
falls back to `ximagesrc` automatically, so nothing below is *required* — it
only unlocks the fast path. Game mode only by default (the X HW cursor lives on
its own DRM plane and is NOT in the captured frame; remote desktop keeps
ximagesrc, which composites the cursor). `PULSAR_KMS=1` forces it for remote
sessions too (e.g. with a software cursor), `PULSAR_KMS=0` disables it.

## One-time setup on the Pi

1. **Patched gstreamer-rockchip** (the distro `gstreamer1.0-rockchip1` snapshot
   can't negotiate DMABuf into `mpph26Xenc`):

   ```sh
   pip3 install --user meson ninja
   git clone https://github.com/JeffyCN/mirrors.git -b gstreamer-rockchip ~/gstreamer-rockchip
   cd ~/gstreamer-rockchip
   git apply /path/to/gst-rockchip-kms-zerocopy.patch
   # bump the version so the user-dir plugins outrank the distro ones
   sed -i "s/version : '1.14.4'/version : '1.20.99'/" meson.build
   PATH=$HOME/.local/bin:$PATH meson setup build -Drockchipmpp=enabled -Dkmssrc=enabled -Drga=enabled -Drkximage=enabled
   PATH=$HOME/.local/bin:$PATH ninja -C build
   mkdir -p ~/.local/share/gstreamer-1.0/plugins
   cp build/gst/rockchipmpp/libgstrockchipmpp.so build/gst/kmssrc/libgstkmssrc.so \
      ~/.local/share/gstreamer-1.0/plugins/
   rm -rf ~/.cache/gstreamer-1.0
   ```

   The patch does two things (see `gst-rockchip-kms-zerocopy.patch`):
   - adds the `memory:DMABuf` caps feature to the `mpph264enc`/`mpph265enc`
     sink templates (upstream did the same for `mppjpegenc` in `da286a4`) —
     the encoder's dmabuf import path (`gst_mpp_allocator_import_gst_memory`)
     already existed, it just never negotiated;
   - makes `kmssrc` fall back to the FB geometry when `fstat` reports
     `st_size == 0` for the imported dmabuf (otherwise the buffer ends up
     zero-sized and the encoder rejects it with "input buffer too small").

2. **Privileged gst-launch copy** — DRM `GETFB2` only hands out FB handles to
   the DRM master or `CAP_SYS_ADMIN` (same rule as ffmpeg's `kmsgrab`; without
   it kmssrc silently produces EMPTY buffers):

   ```sh
   cp /usr/bin/gst-launch-1.0 ~/pulsar-gst-launch
   sudo setcap cap_sys_admin+ep ~/pulsar-gst-launch
   ```

   The host spawns `~/pulsar-gst-launch` for every gst pipeline when present
   (`process::gst_launch_bin`), plain `gst-launch-1.0` otherwise.

## Load-bearing pipeline details

- `sync-fb=false` on kmssrc: paces on the vblank alone (120 Hz panel →
  ~120 fps). The default waits for a NEW pageflip per frame, which throttles an
  idle X desktop to the compositor repaint rate (~58 fps measured).
- `dma-feature=true`: kmssrc advertises `video/x-raw(memory:DMABuf)` caps.
- The remaining gap to a flat 120 (9.3 ms vs 8.33 ms) is per-frame
  GETFB2 + PrimeHandleToFD + fstat overhead in kmssrc; caching the import per
  fb_id would close it (future kmssrc patch).
