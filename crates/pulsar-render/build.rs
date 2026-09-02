//! Version-gate the ffmpeg API used by `decode.rs`.
//!
//! `ffmpeg-sys-next` (`links = "ffmpeg"`) exports the libavcodec version it was
//! built against as `DEP_FFMPEG_FFMPEG_<major>_<minor>=true`.
//! `avcodec_get_supported_config()` arrived in ffmpeg 7.1 and the field it
//! replaces, `AVCodec::pix_fmts`, was removed in ffmpeg 9.0 — so pick the API by
//! the detected version instead of hard-coding either.
fn main() {
	println!("cargo:rustc-check-cfg=cfg(ffmpeg_supported_config)");
	println!("cargo:rerun-if-env-changed=DEP_FFMPEG_FFMPEG_7_1");
	if std::env::var_os("DEP_FFMPEG_FFMPEG_7_1").is_some_and(|v| v == "true") {
		println!("cargo:rustc-cfg=ffmpeg_supported_config");
	}
}
