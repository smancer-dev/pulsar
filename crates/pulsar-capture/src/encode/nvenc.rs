#![allow(non_snake_case, non_camel_case_types, non_upper_case_globals)]
#![allow(clippy::missing_safety_doc)]

// ===========================================================================
// NVENC FFI — hand-rolled bindings for nvEncodeAPI v12.x (API 12.2).
//
// Everything here is taken from the public NVENC headers (`nvEncodeAPI.h`,
// ffnvcodec is BSD). The struct `version` words are computed with the same macros
// the header uses; a wrong word ⇒ NV_ENC_ERR_INVALID_VERSION. The reserved pads
// are kept GENEROUS (oversized reserved is harmless; undersized risks the driver
// writing past [out] fields). Bitfields are modeled as plain u32 `flags` words
// with named setter helpers so we never hand-build reserved bits.
// ===========================================================================

use std::ffi::c_void;
use windows_core::GUID;

pub type NVENCSTATUS = u32;
pub const NV_ENC_SUCCESS: NVENCSTATUS = 0;

// --- API + struct version macros --------------------------------------------
// NVENCAPI_VERSION = (NVENCAPI_MAJOR_VERSION | (NVENCAPI_MINOR_VERSION << 24)).
// SDK 12.2 ⇒ major 12, minor 2.
pub const NVENCAPI_MAJOR_VERSION: u32 = 12;
pub const NVENCAPI_MINOR_VERSION: u32 = 2;
pub const NVENCAPI_VERSION: u32 = NVENCAPI_MAJOR_VERSION | (NVENCAPI_MINOR_VERSION << 24);

// NVENCAPI_STRUCT_VERSION(ver) = NVENCAPI_VERSION | ((ver)<<16) | (0x7<<28)
pub const fn struct_version(ver: u32) -> u32 {
	NVENCAPI_VERSION | (ver << 16) | (0x7 << 28)
}
// The big [in/out] structs OR the high bit (the SDK's `| (1<<31)`).
pub const fn struct_version_ex(ver: u32) -> u32 {
	struct_version(ver) | (1u32 << 31)
}

pub const NV_ENCODE_API_FUNCTION_LIST_VER: u32 = struct_version(2);
pub const NV_ENC_OPEN_ENCODE_SESSION_EX_PARAMS_VER: u32 = struct_version(1);
pub const NV_ENC_INITIALIZE_PARAMS_VER: u32 = struct_version_ex(5);
pub const NV_ENC_CONFIG_VER: u32 = struct_version_ex(8);
pub const NV_ENC_PRESET_CONFIG_VER: u32 = struct_version_ex(5);
pub const NV_ENC_REGISTER_RESOURCE_VER: u32 = struct_version(4);
pub const NV_ENC_MAP_INPUT_RESOURCE_VER: u32 = struct_version(4);
pub const NV_ENC_CREATE_BITSTREAM_BUFFER_VER: u32 = struct_version(1);
pub const NV_ENC_PIC_PARAMS_VER: u32 = struct_version_ex(7);
pub const NV_ENC_LOCK_BITSTREAM_VER: u32 = struct_version(2);
pub const NV_ENC_RECONFIGURE_PARAMS_VER: u32 = struct_version_ex(1);

/// `NV_ENC_RECONFIGURE_PARAMS` (nvEncodeAPI.h) — LIVE encoder reconfigure (Stage-3 adaptive
/// bitrate: change bitrate without a full session re-init). Embeds the FULL
/// `NV_ENC_INITIALIZE_PARAMS` by value, whose `encodeConfig` must point at a STABLE, updated
/// `NV_ENC_CONFIG` (we keep both Boxed on the `Encoder`). `flags` packs the C bitfield
/// `resetEncoder:1`(bit0) + `forceIDR:1`(bit1) + `reserved:30`.
#[repr(C)]
pub struct NV_ENC_RECONFIGURE_PARAMS {
	pub version: u32,
	pub reInitEncodeParams: NV_ENC_INITIALIZE_PARAMS,
	pub flags: u32,
}
impl NV_ENC_RECONFIGURE_PARAMS {
	pub fn set_reset_encoder(&mut self, v: bool) {
		if v {
			self.flags |= 1;
		} else {
			self.flags &= !1;
		}
	}
	pub fn set_force_idr(&mut self, v: bool) {
		if v {
			self.flags |= 1 << 1;
		} else {
			self.flags &= !(1 << 1);
		}
	}
}

pub type PFN_ReconfigureEncoder =
	unsafe extern "C" fn(*mut c_void, *mut NV_ENC_RECONFIGURE_PARAMS) -> NVENCSTATUS;

// --- enums (passed as u32) ---------------------------------------------------
// NVENC NV_ENC_DEVICE_TYPE: DIRECTX=0x0, CUDA=0x1, OPENGL=0x2. (Was wrongly 1 → that's CUDA,
// so OpenSessionEx got a D3D11 device tagged as a CUDA context → NV_ENC_ERR_UNSUPPORTED_DEVICE.)
pub const NV_ENC_DEVICE_TYPE_DIRECTX: u32 = 0;
pub const NV_ENC_INPUT_RESOURCE_TYPE_DIRECTX: u32 = 0;
pub const NV_ENC_INPUT_IMAGE: u32 = 0; // NV_ENC_INPUT_RESOURCE_USAGE
pub const NV_ENC_BUFFER_FORMAT_NV12: u32 = 0x00000001;
pub const NV_ENC_PIC_STRUCT_FRAME: u32 = 0x01;
pub const NV_ENC_PARAMS_RC_CBR: u32 = 0x2; // NV_ENC_PARAMS_RC_MODE
										   // NV_ENC_TUNING_INFO: 1=HIGH_QUALITY, 2=LOW_LATENCY, 3=ULTRA_LOW_LATENCY, 4=LOSSLESS
pub const NV_ENC_TUNING_INFO_LOW_LATENCY: u32 = 2;
pub const NV_ENC_TUNING_INFO_ULTRA_LOW_LATENCY: u32 = 3;
pub const NV_ENC_H264_ENTROPY_CODING_MODE_CABAC: u32 = 1;
// NV_ENC_RC_FLAGS bit: zero reorder delay.
// zeroReorderDelay is bit 9 of the NV_ENC_RC_PARAMS bitfield word. (0x4 is bit 2 =
// enableInitialRCQP — the old value wrongly armed initial-QP RC; harmless only while the
// config was ignored, but wrong once encodeConfig is honored.)
pub const NV_ENC_RC_FLAG_ZERO_REORDER_DELAY: u32 = 0x200;

// --- GUIDs (stable across NVENC versions — copy the header bytes exactly) -----
// Codec: H.264 = 6BC82762-4E63-4ca4-AA85-1E50F321F6BF
pub const NV_ENC_CODEC_H264_GUID: GUID = GUID::from_values(
	0x6BC82762,
	0x4E63,
	0x4ca4,
	[0xAA, 0x85, 0x1E, 0x50, 0xF3, 0x21, 0xF6, 0xBF],
);
// Profile: H.264 High = E7CBC309-4F7A-4b89-AF2A-D537C92BE310
pub const NV_ENC_H264_PROFILE_HIGH_GUID: GUID = GUID::from_values(
	0xE7CBC309,
	0x4F7A,
	0x4b89,
	[0xAF, 0x2A, 0xD5, 0x37, 0xC9, 0x2B, 0xE3, 0x10],
);
// Codec: HEVC (H.265) = 790CDC88-4522-4d7b-9425-BDA9975F7603
pub const NV_ENC_CODEC_HEVC_GUID: GUID = GUID::from_values(
	0x790CDC88,
	0x4522,
	0x4d7b,
	[0x94, 0x25, 0xBD, 0xA9, 0x97, 0x5F, 0x76, 0x03],
);
// Profile: HEVC Main = B514C39A-B55B-40fa-878F-F1253B4DFDEC
pub const NV_ENC_HEVC_PROFILE_MAIN_GUID: GUID = GUID::from_values(
	0xB514C39A,
	0xB55B,
	0x40fa,
	[0x87, 0x8F, 0xF1, 0x25, 0x3B, 0x4D, 0xFD, 0xEC],
);
// Codec: AV1 = 0A352289-0AA7-4759-862D-5D15CD16D254
pub const NV_ENC_CODEC_AV1_GUID: GUID = GUID::from_values(
	0x0A352289,
	0x0AA7,
	0x4759,
	[0x86, 0x2D, 0x5D, 0x15, 0xCD, 0x16, 0xD2, 0x54],
);
// Profile: AV1 Main = 5f2a39f5-f14e-4f95-9a9e-b76d568fcf97
pub const NV_ENC_AV1_PROFILE_MAIN_GUID: GUID = GUID::from_values(
	0x5f2a39f5,
	0xf14e,
	0x4f95,
	[0x9a, 0x9e, 0xb7, 0x6d, 0x56, 0x8f, 0xcf, 0x97],
);
// Preset P1 (fastest) = FC0A8D3E-45F8-4CF8-80C7-298871590EBF
pub const NV_ENC_PRESET_P1_GUID: GUID = GUID::from_values(
	0xFC0A8D3E,
	0x45F8,
	0x4CF8,
	[0x80, 0xC7, 0x29, 0x88, 0x71, 0x59, 0x0E, 0xBF],
);
// Preset P4 (balanced) = 90A7B826-DF06-4862-B9D2-CD6D73A08681
pub const NV_ENC_PRESET_P4_GUID: GUID = GUID::from_values(
	0x90A7B826,
	0xDF06,
	0x4862,
	[0xB9, 0xD2, 0xCD, 0x6D, 0x73, 0xA0, 0x86, 0x81],
);

// --- The NV_ENCODE_API_FUNCTION_LIST fn-pointer table -------------------------
// Field order is FIXED by the header. We only call a subset, but the WHOLE table
// must be present and correctly ordered (the driver fills every slot). Unused
// slots are typed as opaque `*mut c_void` to avoid spelling out every signature;
// the slots we call are typed precisely. Keep the trailing reserved2 pad.
//
// All fns are `extern "C"` and take `*mut c_void` encoder + a `*mut <PARAMS>`.
pub type PFN_OpenSessionEx = unsafe extern "C" fn(
	*mut NV_ENC_OPEN_ENCODE_SESSION_EX_PARAMS,
	*mut *mut c_void,
) -> NVENCSTATUS;
pub type PFN_GetPresetConfigEx =
	unsafe extern "C" fn(*mut c_void, GUID, GUID, u32, *mut NV_ENC_PRESET_CONFIG) -> NVENCSTATUS;
pub type PFN_InitializeEncoder =
	unsafe extern "C" fn(*mut c_void, *mut NV_ENC_INITIALIZE_PARAMS) -> NVENCSTATUS;
pub type PFN_RegisterResource =
	unsafe extern "C" fn(*mut c_void, *mut NV_ENC_REGISTER_RESOURCE) -> NVENCSTATUS;
pub type PFN_UnregisterResource = unsafe extern "C" fn(*mut c_void, *mut c_void) -> NVENCSTATUS;
pub type PFN_MapInputResource =
	unsafe extern "C" fn(*mut c_void, *mut NV_ENC_MAP_INPUT_RESOURCE) -> NVENCSTATUS;
pub type PFN_UnmapInputResource = unsafe extern "C" fn(*mut c_void, *mut c_void) -> NVENCSTATUS;
pub type PFN_CreateBitstreamBuffer =
	unsafe extern "C" fn(*mut c_void, *mut NV_ENC_CREATE_BITSTREAM_BUFFER) -> NVENCSTATUS;
pub type PFN_DestroyBitstreamBuffer = unsafe extern "C" fn(*mut c_void, *mut c_void) -> NVENCSTATUS;
pub type PFN_EncodePicture =
	unsafe extern "C" fn(*mut c_void, *mut NV_ENC_PIC_PARAMS) -> NVENCSTATUS;
pub type PFN_LockBitstream =
	unsafe extern "C" fn(*mut c_void, *mut NV_ENC_LOCK_BITSTREAM) -> NVENCSTATUS;
pub type PFN_UnlockBitstream = unsafe extern "C" fn(*mut c_void, *mut c_void) -> NVENCSTATUS;
pub type PFN_DestroyEncoder = unsafe extern "C" fn(*mut c_void) -> NVENCSTATUS;
pub type PFN_GetLastErrorString = unsafe extern "C" fn(*mut c_void) -> *const i8;

// NV_ENCODE_API_FUNCTION_LIST as laid out in nvEncodeAPI.h (12.x). Slots we don't
// call are opaque pointers so we never mis-spell a signature; ORDER is what matters.
#[repr(C)]
pub struct NV_ENCODE_API_FUNCTION_LIST {
	pub version: u32,
	pub reserved: u32,
	pub nvEncOpenEncodeSession: *mut c_void, // deprecated entrypoint
	pub nvEncGetEncodeGUIDCount: *mut c_void,
	pub nvEncGetEncodeProfileGUIDCount: *mut c_void,
	pub nvEncGetEncodeProfileGUIDs: *mut c_void,
	pub nvEncGetEncodeGUIDs: *mut c_void,
	pub nvEncGetInputFormatCount: *mut c_void,
	pub nvEncGetInputFormats: *mut c_void,
	pub nvEncGetEncodeCaps: *mut c_void,
	pub nvEncGetEncodePresetCount: *mut c_void,
	pub nvEncGetEncodePresetGUIDs: *mut c_void,
	pub nvEncGetEncodePresetConfig: *mut c_void,
	pub nvEncInitializeEncoder: Option<PFN_InitializeEncoder>,
	pub nvEncCreateInputBuffer: *mut c_void,
	pub nvEncDestroyInputBuffer: *mut c_void,
	pub nvEncCreateBitstreamBuffer: Option<PFN_CreateBitstreamBuffer>,
	pub nvEncDestroyBitstreamBuffer: Option<PFN_DestroyBitstreamBuffer>,
	pub nvEncEncodePicture: Option<PFN_EncodePicture>,
	pub nvEncLockBitstream: Option<PFN_LockBitstream>,
	pub nvEncUnlockBitstream: Option<PFN_UnlockBitstream>,
	pub nvEncLockInputBuffer: *mut c_void,
	pub nvEncUnlockInputBuffer: *mut c_void,
	pub nvEncGetEncodeStats: *mut c_void,
	pub nvEncGetSequenceParams: *mut c_void,
	pub nvEncRegisterAsyncEvent: *mut c_void,
	pub nvEncUnregisterAsyncEvent: *mut c_void,
	pub nvEncMapInputResource: Option<PFN_MapInputResource>,
	pub nvEncUnmapInputResource: Option<PFN_UnmapInputResource>,
	pub nvEncDestroyEncoder: Option<PFN_DestroyEncoder>,
	pub nvEncInvalidateRefFrames: *mut c_void,
	pub nvEncOpenEncodeSessionEx: Option<PFN_OpenSessionEx>,
	pub nvEncRegisterResource: Option<PFN_RegisterResource>,
	pub nvEncUnregisterResource: Option<PFN_UnregisterResource>,
	pub nvEncReconfigureEncoder: Option<PFN_ReconfigureEncoder>,
	pub reserved1: *mut c_void,
	pub nvEncCreateMVBuffer: *mut c_void,
	pub nvEncDestroyMVBuffer: *mut c_void,
	pub nvEncRunMotionEstimationOnly: *mut c_void,
	pub nvEncGetLastErrorString: Option<PFN_GetLastErrorString>,
	pub nvEncSetIOCudaStreams: *mut c_void,
	pub nvEncGetEncodePresetConfigEx: Option<PFN_GetPresetConfigEx>,
	pub nvEncGetSequenceParamEx: *mut c_void,
	// Trailing reserved fn-pointer pad (the header reserves 277 slots).
	pub reserved2: [*mut c_void; 277],
}

// --- param structs -----------------------------------------------------------

#[repr(C)]
pub struct NV_ENC_OPEN_ENCODE_SESSION_EX_PARAMS {
	pub version: u32,
	pub deviceType: u32,
	pub device: *mut c_void,
	pub reserved: *mut c_void,
	pub apiVersion: u32,
	pub reserved1: [u32; 253],
	pub reserved2: [*mut c_void; 64],
}

// NV_ENC_RC_PARAMS — rate control. `flags` is the bitfield word; named setters keep
// reserved bits intact when we start from the preset config. We model the bitfields
// as one u32 `flags` plus the named scalar fields the header lists around it.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct NV_ENC_RC_PARAMS {
	pub version: u32,
	pub rateControlMode: u32,
	pub constQP: NV_ENC_QP,
	pub averageBitRate: u32,
	pub maxBitRate: u32,
	pub vbvBufferSize: u32,
	pub vbvInitialDelay: u32,
	flags: u32, // bitfield word (enableMinQP, zeroReorderDelay, enableLookahead, …)
	pub minQP: NV_ENC_QP,
	pub maxQP: NV_ENC_QP,
	pub initialRCQP: NV_ENC_QP,
	pub temporallayerIdxMask: u32,
	pub temporalLayerQP: [u8; 8],
	pub targetQuality: u8,
	pub targetQualityLSB: u8,
	pub lookaheadDepth: u16,
	pub lowDelayKeyFrameScale: u8,
	pub yDcQPIndexOffset: i8,
	pub uDcQPIndexOffset: i8,
	pub vDcQPIndexOffset: i8,
	pub qpMapMode: u32,
	pub multiPass: u32,
	pub alphaLayerBitrateRatio: u32,
	pub cbQPIndexOffset: i8,
	pub crQPIndexOffset: i8,
	pub reserved1: u16,
	pub reserved: [u32; 4],
}
impl NV_ENC_RC_PARAMS {
	/// OR named RC flag bits in (preserving the preset's reserved bits).
	pub fn set_flags(&mut self, bits: u32) {
		self.flags |= bits;
	}
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct NV_ENC_QP {
	pub qpInterP: u32,
	pub qpInterB: u32,
	pub qpIntra: u32,
}

// NV_ENC_CONFIG_H264 — H.264 codec config. The leading `flags`-style bitfield word
// holds enableTemporalSVC/… and `repeatSPSPPS` (bit 12); the entropy/chroma/refL0
// bitfields are folded into named u32 words with setters. Generous reserved pad.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct NV_ENC_CONFIG_H264 {
	flags: u32, // enableTemporalSVC, hierarchicalPFrames, …, repeatSPSPPS(bit12), …
	pub level: u32,
	pub idrPeriod: u32,
	pub separateColourPlaneFlag: u32,
	pub disableDeblockingFilterIDC: u32,
	pub numTemporalLayers: u32,
	pub spsId: u32,
	pub ppsId: u32,
	adaptiveTransformMode: u32,
	fmoMode: u32,
	bdirectMode: u32,
	entropyCodingMode: u32,
	pub stereoMode: u32,
	pub intraRefreshPeriod: u32,
	pub intraRefreshCnt: u32,
	pub maxNumRefFrames: u32,
	sliceMode: u32,
	sliceModeData: u32,
	// h264VUIParameters (NV_ENC_CONFIG_H264_VUI_PARAMETERS) — opaque pad of its size.
	h264VUIParameters: [u32; 30],
	pub ltrNumFrames: u32,
	ltrTrustMode: u32,
	pub chromaFormatIDC: u32, // we keep this nameable too (set_chromaFormatIDC mirrors it)
	pub maxTemporalLayers: u32,
	useBFramesAsRef: u32,
	numRefL0: u32,
	numRefL1: u32,
	pub reserved1: [u32; 267],
	pub reserved2: [*mut c_void; 64],
}
impl NV_ENC_CONFIG_H264 {
	pub fn set_flags(&mut self, bits: u32) {
		self.flags |= bits;
	}
	pub fn set_entropyCodingMode(&mut self, v: u32) {
		self.entropyCodingMode = v;
	}
	pub fn set_chromaFormatIDC(&mut self, v: u32) {
		self.chromaFormatIDC = v;
	}
	pub fn set_numRefL0(&mut self, v: u32) {
		self.numRefL0 = v;
	}
}

// NV_ENC_CONFIG_HEVC — HEVC (H.265) codec config (nvEncodeAPI.h, 12.x). Layout:
//   level, tier, minCUSize, maxCUSize  (4 × u32; the two CU sizes are NV_ENC_HEVC_CUSIZE
//     enums = u32), then ONE bitfield word `flags` that packs (LSB→MSB):
//     useConstrainedIntraPred:1, disableDeblockAcrossSliceBoundary:1,
//     outputBufferingPeriodSEI:1, outputPictureTimingSEI:1, outputAUD:1, enableLTR:1,
//     disableSPSPPS:1, repeatSPSPPS:1, enableIntraRefresh:1, chromaFormatIDC:2,
//     pixelBitDepthMinus8:2, enableFillerDataInsertion:1, enableConstrainedEncoding:1,
//     enableAlphaLayerEncoding:1, singleSliceIntraRefresh:1, outputRecoveryPointSEI:1,
//     outputTimeCodeSEI:1, reserved:11.
//   → bit positions we use: repeatSPSPPS = bit 7, outputAUD = bit 4, chromaFormatIDC at
//     bits 9..10 (value 1 = 4:2:0 ⇒ 1<<9). Then the scalar fields begin at idrPeriod.
// Reserved pad is GENEROUS (the header reserves reserved1[218] + reserved2[64]).
#[repr(C)]
#[derive(Clone, Copy)]
pub struct NV_ENC_CONFIG_HEVC {
	pub level: u32,
	pub tier: u32,
	pub minCUSize: u32, // NV_ENC_HEVC_CUSIZE (0 = AUTOSELECT)
	pub maxCUSize: u32, // NV_ENC_HEVC_CUSIZE (0 = AUTOSELECT)
	flags: u32,         // packed bitfields (see comment above)
	pub idrPeriod: u32,
	pub intraRefreshPeriod: u32,
	pub intraRefreshCnt: u32,
	pub maxNumRefFramesInDPB: u32,
	pub ltrNumFrames: u32,
	pub vpsId: u32,
	pub spsId: u32,
	pub ppsId: u32,
	pub sliceMode: u32,
	pub sliceModeData: u32,
	pub maxTemporalLayersMinus1: u32,
	// hevcVUIParameters (NV_ENC_CONFIG_HEVC_VUI_PARAMETERS) — same layout/size as the
	// H.264 VUI struct: opaque pad of its size (30 u32s).
	hevcVUIParameters: [u32; 30],
	pub ltrTrustMode: u32,
	pub ltrRefNumFrames: u32, // (ltrTrustMode pair / numRefL fields in 12.x)
	pub numRefL0: u32,
	pub numRefL1: u32,
	pub reserved1: [u32; 214],
	pub reserved2: [*mut c_void; 64],
}
impl NV_ENC_CONFIG_HEVC {
	/// OR named HEVC bitfield bits in (preserving the preset's reserved bits).
	pub fn set_flags(&mut self, bits: u32) {
		self.flags |= bits;
	}
}
/// HEVC `repeatSPSPPS` bit (bit 7 of the HEVC bitfield word) — re-emit VPS/SPS/PPS in-band
/// on each IDR so a late-joining RTP client (no out-of-band SDP) always has parameter sets.
pub const NV_ENC_HEVC_FLAG_REPEAT_SPSPPS: u32 = 1 << 7;
/// HEVC `chromaFormatIDC` = 1 (4:2:0) packed at bits 9..10.
pub const NV_ENC_HEVC_CHROMA_420: u32 = 1 << 9;

// NV_ENC_CONFIG_AV1 — AV1 codec config (nvEncodeAPI.h, 12.x). Layout:
//   level, tier, minPartSize, maxPartSize (4 × u32), then ONE bitfield word `flags`
//   packing (LSB→MSB): outputAnnexBFormat:1, enableTimingInfo:1, enableDecoderModelInfo:1,
//   enableSeqHdrField:1, repeatSeqHdr:1, enableIntraRefresh:1, chromaFormatIDC:2,
//   enableBitstreamPadding:1, enableCustomTileConfig:1, enableFilmGrainParams:1,
//   inputPixelBitDepthMinus8:3, pixelBitDepthMinus8:3, reserved:15.
//   → repeatSeqHdr = bit 4, chromaFormatIDC at bits 6..7 (1 = 4:2:0 ⇒ 1<<6). NVENC AV1
//   emits OBUs; we leave outputAnnexBFormat (bit 0) clear (default low-overhead OBU stream).
//   Then scalar fields: idrPeriod, intraRefreshPeriod, intraRefreshCnt, maxNumRefFramesInDPB,
//   numTileColumns, numTileRows, … VUI … reserved.
// Reserved pad is GENEROUS to cover the header's reserved1/reserved2 tails.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct NV_ENC_CONFIG_AV1 {
	pub level: u32,
	pub tier: u32,
	pub minPartSize: u32, // NV_ENC_AV1_PART_SIZE (0 = AUTOSELECT)
	pub maxPartSize: u32, // NV_ENC_AV1_PART_SIZE (0 = AUTOSELECT)
	flags: u32,           // packed bitfields (see comment above)
	pub idrPeriod: u32,
	pub intraRefreshPeriod: u32,
	pub intraRefreshCnt: u32,
	pub maxNumRefFramesInDPB: u32,
	pub numTileColumns: u32,
	pub numTileRows: u32,
	pub reserved1Field: u32,
	pub idrPeriodField: u32,
	pub maxTemporalLayersMinus1: u32,
	pub colorPrimaries: u32,
	pub transferCharacteristics: u32,
	pub matrixCoefficients: u32,
	pub colorRange: u32,
	pub chromaSamplePosition: u32,
	pub useBFramesAsRef: u32,
	pub numFwdRefs: u32,
	pub numBwdRefs: u32,
	pub outputBitDepth: u32,
	pub inputBitDepth: u32,
	pub reserved1: [u32; 222],
	pub reserved2: [*mut c_void; 64],
}
impl NV_ENC_CONFIG_AV1 {
	/// OR named AV1 bitfield bits in (preserving the preset's reserved bits).
	pub fn set_flags(&mut self, bits: u32) {
		self.flags |= bits;
	}
}
/// AV1 `repeatSeqHdr` bit (bit 4 of the AV1 bitfield word) — re-emit the sequence header
/// OBU on each key frame so a late-joining client always has it (HEVC repeatSPSPPS analog).
pub const NV_ENC_AV1_FLAG_REPEAT_SEQ_HDR: u32 = 1 << 4;
/// AV1 `chromaFormatIDC` = 1 (4:2:0) packed at bits 6..7.
pub const NV_ENC_AV1_CHROMA_420: u32 = 1 << 6;

// The codec-config union (H264 / HEVC / AV1). NV_ENC_CONFIG_H264 with its generous
// reserved pad is the largest member, so the union is correctly sized for all three.
#[repr(C)]
#[derive(Clone, Copy)]
pub union NV_ENC_CODEC_CONFIG {
	pub h264Config: NV_ENC_CONFIG_H264,
	pub hevcConfig: NV_ENC_CONFIG_HEVC,
	pub av1Config: NV_ENC_CONFIG_AV1,
	// reserved pad guaranteeing the union is large enough for any codec config.
	pub reserved: [u32; 320],
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct NV_ENC_CONFIG {
	pub version: u32,
	pub profileGUID: GUID,
	pub gopLength: u32,
	pub frameIntervalP: i32,
	pub monoChromeEncoding: u32,
	pub frameFieldMode: u32,
	pub mvPrecision: u32,
	pub rcParams: NV_ENC_RC_PARAMS,
	pub encodeCodecConfig: NV_ENC_CODEC_CONFIG,
	pub reserved: [u32; 278],
	pub reserved2: [*mut c_void; 64],
}

#[repr(C)]
pub struct NV_ENC_PRESET_CONFIG {
	pub version: u32,
	pub presetCfg: NV_ENC_CONFIG,
	pub reserved1: [u32; 255],
	pub reserved2: [*mut c_void; 64],
}

// NV_ENC_INITIALIZE_PARAMS — the `flags`-bitfield word holds enableEncodeAsync(bit0),
// enablePTD(bit1), …; we expose named setters for the two we toggle.
#[repr(C)]
pub struct NV_ENC_INITIALIZE_PARAMS {
	pub version: u32,
	pub encodeGUID: GUID,
	pub presetGUID: GUID,
	pub encodeWidth: u32,
	pub encodeHeight: u32,
	pub darWidth: u32,
	pub darHeight: u32,
	pub frameRateNum: u32,
	pub frameRateDen: u32,
	pub enableEncodeAsync: u32,
	ptd_flags: u32, // enablePTD lives at bit 0 of this word (set via set_enablePTD)
	// reportSliceOffsets:1, enableSubFrameWrite:1, enableExternalMEHints:1, enableMEOnlyMode:1,
	// enableWeightedPrediction:1, enableOutputInVidmem:1, reservedBitFields:26 — ONE packed u32
	// in nvEncodeAPI.h. Declaring them as SEVEN separate words (the old bug) pushed `encodeConfig`
	// 24 bytes past the driver-expected offset 88 → the driver read `encodeConfig` as NULL → the
	// ENTIRE NV_ENC_CONFIG (CBR / averageBitRate / VBV) was silently dropped at init AND at every
	// nvEncReconfigureEncoder, so the encoder ran NVENC's default RC and ignored all bitrate
	// requests (no init cap, no adaptive-bitrate). Keep this ONE word; the offset assert below pins it.
	pub bitfields: u32,
	pub privDataSize: u32,
	pub privData: *mut c_void,
	pub encodeConfig: *mut NV_ENC_CONFIG,
	pub maxEncodeWidth: u32,
	pub maxEncodeHeight: u32,
	pub maxMEHintCountsPerBlock: [u32; 2],
	pub tuningInfo: u32,
	pub bufferFormat: u32,
	pub numStateBuffers: u32,
	pub outputStatsLevel: u32,
	pub reserved: [u32; 287],
	pub reserved2: [*mut c_void; 64],
}
impl NV_ENC_INITIALIZE_PARAMS {
	/// enablePTD lives at bit 0 of the bitfield word following enableEncodeAsync.
	pub fn set_enablePTD(&mut self, on: u32) {
		if on != 0 {
			self.ptd_flags |= 1;
		} else {
			self.ptd_flags &= !1;
		}
	}
}

// Pin the ABI: on x64 the driver reads `encodeConfig` at offset 88. If a field width above is
// wrong, encodeConfig moves and the driver reads it as garbage/NULL → it silently ignores the
// whole NV_ENC_CONFIG (the bitrate bug this fixed). A compile error here means the layout drifted.
#[cfg(target_pointer_width = "64")]
const _: () = assert!(core::mem::offset_of!(NV_ENC_INITIALIZE_PARAMS, encodeConfig) == 88);

#[repr(C)]
pub struct NV_ENC_REGISTER_RESOURCE {
	pub version: u32,
	pub resourceType: u32,
	pub width: u32,
	pub height: u32,
	pub pitch: u32,
	pub subResourceIndex: u32,
	pub resourceToRegister: *mut c_void,
	pub registeredResource: *mut c_void,
	pub bufferFormat: u32,
	pub bufferUsage: u32,
	pub pInputFencePoint: *mut c_void,
	pub chromaOffset: [u32; 2],
	pub reserved1: [u32; 244],
	pub reserved2: [*mut c_void; 60],
}

#[repr(C)]
pub struct NV_ENC_MAP_INPUT_RESOURCE {
	pub version: u32,
	pub subResourceIndex: u32,
	pub inputResource: *mut c_void,
	pub registeredResource: *mut c_void,
	pub mappedResource: *mut c_void,
	pub mappedBufferFmt: u32,
	pub pInputFencePoint: *mut c_void,
	pub reserved1: [u32; 251],
	pub reserved2: [*mut c_void; 63],
}

#[repr(C)]
pub struct NV_ENC_CREATE_BITSTREAM_BUFFER {
	pub version: u32,
	pub size: u32,
	pub memoryHeap: u32,
	pub reserved: u32,
	pub bitstreamBuffer: *mut c_void,
	pub bitstreamBufferPtr: *mut c_void,
	pub reserved1: [u32; 58],
	pub reserved2: [*mut c_void; 64],
}

// NV_ENC_PIC_FLAGS (encodePicFlags bits).
pub const NV_ENC_PIC_FLAG_FORCEIDR: u32 = 0x2;
pub const NV_ENC_PIC_FLAG_OUTPUT_SPSPPS: u32 = 0x4;

// NV_ENC_PIC_PARAMS — per-frame. `encodePicFlags` is a plain word (NV_ENC_PIC_FLAGS),
// so we set it directly. The codec-specific picparams union is opaque-padded.
#[repr(C)]
pub struct NV_ENC_PIC_PARAMS {
	pub version: u32,
	pub inputWidth: u32,
	pub inputHeight: u32,
	pub inputPitch: u32,
	pub encodePicFlags: u32,
	pub frameIdx: u32,
	pub inputTimeStamp: u64,
	pub inputDuration: u64,
	pub inputBuffer: *mut c_void,
	pub outputBitstream: *mut c_void,
	pub completionEvent: *mut c_void,
	pub bufferFmt: u32,
	pub pictureStruct: u32,
	pub pictureType: u32,
	// codecPicParams union (NV_ENC_CODEC_PIC_PARAMS) — opaque pad of its size.
	codecPicParams: [u32; 256],
	pub meHintCountsPerBlock: [u32; 8],
	pub meExternalHints: *mut c_void,
	pub reserved1: [u32; 6],
	pub reserved2: [*mut c_void; 2],
	pub qpDeltaMap: *mut i8,
	pub qpDeltaMapSize: u32,
	pub reservedBitFields: u32,
	pub meHintRefPicDist: [u16; 2],
	pub alphaBuffer: *mut c_void,
	pub meExternalSbHints: *mut c_void,
	pub meSbHintsCount: u32,
	pub stateBufferIdx: u32,
	pub outputReconBuffer: *mut c_void,
	pub reserved3: [u32; 284],
	pub reserved4: [*mut c_void; 57],
}

// NV_ENC_LOCK_BITSTREAM — `flags` word holds doNotWait(bit0)+ltrFrame(bit1)+reserved.
#[repr(C)]
pub struct NV_ENC_LOCK_BITSTREAM {
	pub version: u32,
	flags: u32, // doNotWait (bit0), ltrFrame (bit1), getRCStats (bit2), reservedBitFields
	pub outputBitstream: *mut c_void,
	pub sliceOffsets: *mut u32,
	pub frameIdx: u32,
	pub hwEncodeStatus: u32,
	pub numSlices: u32,
	pub bitstreamSizeInBytes: u32,
	pub outputTimeStamp: u64,
	pub outputDuration: u64,
	pub bitstreamBufferPtr: *mut c_void,
	pub pictureType: u32,
	pub pictureStruct: u32,
	pub frameAvgQP: u32,
	pub frameSatd: u32,
	pub ltrFrameIdx: u32,
	pub ltrFrameBitmap: u32,
	pub temporalId: u32,
	pub reserved: [u32; 12],
	pub intraMBCount: u32,
	pub interMBCount: u32,
	pub averageMVX: i32,
	pub averageMVY: i32,
	pub alphaLayerSizeInBytes: u32,
	pub outputStatsPtr: *mut c_void,
	pub reserved1: [u32; 218],
	pub reserved2: [*mut c_void; 64],
}
impl NV_ENC_LOCK_BITSTREAM {
	/// Set the raw flags word (0 = blocking lock, no special behaviour).
	pub fn set_flags(&mut self, v: u32) {
		self.flags = v;
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	// The codecConfig union (and thus NV_ENC_CONFIG) must be sized to its LARGEST member,
	// and we must not have made any per-codec struct larger than the union's reserved pad
	// — an undersized union would let the driver write past our buffer (silent corruption).
	#[test]
	fn codec_config_union_is_large_enough() {
		let union_sz = std::mem::size_of::<NV_ENC_CODEC_CONFIG>();
		assert!(union_sz >= std::mem::size_of::<NV_ENC_CONFIG_H264>());
		assert!(union_sz >= std::mem::size_of::<NV_ENC_CONFIG_HEVC>());
		assert!(union_sz >= std::mem::size_of::<NV_ENC_CONFIG_AV1>());
	}

	// Each codec-config bitfield word matches the nvEncodeAPI.h bit positions we rely on.
	#[test]
	fn codec_flag_bit_positions() {
		let mut h = unsafe { std::mem::zeroed::<NV_ENC_CONFIG_HEVC>() };
		h.set_flags(NV_ENC_HEVC_FLAG_REPEAT_SPSPPS | NV_ENC_HEVC_CHROMA_420);
		assert_eq!(h.flags, (1 << 7) | (1 << 9)); // repeatSPSPPS(7) + chromaFormatIDC=1(9)
		let mut a = unsafe { std::mem::zeroed::<NV_ENC_CONFIG_AV1>() };
		a.set_flags(NV_ENC_AV1_FLAG_REPEAT_SEQ_HDR | NV_ENC_AV1_CHROMA_420);
		assert_eq!(a.flags, (1 << 4) | (1 << 6)); // repeatSeqHdr(4) + chromaFormatIDC=1(6)
	}
}
