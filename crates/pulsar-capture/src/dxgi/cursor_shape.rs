//! Pure-CPU decode of raw DXGI pointer-shape buffers into packed BGRA images.
//!
//! DXGI delivers a cursor as one of three shape types (COLOR / MASKED_COLOR / MONOCHROME);
//! these helpers convert each into up to two tightly-packed BGRA images — an alpha-blend
//! image and an invert/XOR image — that `cursor.rs` uploads as GPU textures. No D3D here,
//! just byte twiddling; mirrors Sunshine's `make_cursor_*_image` (display_vram.cpp). Moved
//! verbatim from the original `dxgi.rs` (behaviour unchanged).

use windows::Win32::Graphics::Dxgi::{
    DXGI_OUTDUPL_POINTER_SHAPE_INFO, DXGI_OUTDUPL_POINTER_SHAPE_TYPE,
    DXGI_OUTDUPL_POINTER_SHAPE_TYPE_COLOR, DXGI_OUTDUPL_POINTER_SHAPE_TYPE_MASKED_COLOR,
    DXGI_OUTDUPL_POINTER_SHAPE_TYPE_MONOCHROME,
};

/// Convert a raw DXGI pointer-shape buffer into the alpha-blend BGRA image. Returns the
/// packed `width*height*4` bytes (height already halved for MONOCHROME). Mirrors Sunshine
/// `make_cursor_alpha_image` (display_vram.cpp L280). A pixel is a little-endian u32 laid out
/// `0xAARRGGBB` (byte order B,G,R,A in memory).
pub(super) fn make_cursor_alpha_image(
    bytes: &[u8],
    info: &DXGI_OUTDUPL_POINTER_SHAPE_INFO,
) -> Option<(Vec<u8>, u32, u32)> {
    const BLACK: u32 = 0xFF00_0000;
    const WHITE: u32 = 0xFFFF_FFFF;
    const TRANSPARENT: u32 = 0;
    let shape = DXGI_OUTDUPL_POINTER_SHAPE_TYPE(info.Type as i32);

    match shape {
        DXGI_OUTDUPL_POINTER_SHAPE_TYPE_COLOR => {
            // Already BGRA with a real alpha channel — copy row by row honoring the source
            // pitch (which may exceed width*4), writing packed width*4 output.
            Some(repack_bgra(bytes, info.Width, info.Height, info.Pitch))
        }
        DXGI_OUTDUPL_POINTER_SHAPE_TYPE_MASKED_COLOR => {
            // BGRA but the alpha byte is a 1-bit flag: 0x00 → blend opaque here, 0xFF → handled
            // by the xor pass (transparent here).
            let (mut img, w, h) = repack_bgra(bytes, info.Width, info.Height, info.Pitch);
            for px in img.chunks_exact_mut(4) {
                let mut p = u32::from_le_bytes([px[0], px[1], px[2], px[3]]);
                if (p >> 24) == 0xFF {
                    p = TRANSPARENT;
                } else {
                    p |= 0xFF00_0000; // force opaque so the alpha blend draws it
                }
                px.copy_from_slice(&p.to_le_bytes());
            }
            Some((img, w, h))
        }
        DXGI_OUTDUPL_POINTER_SHAPE_TYPE_MONOCHROME => {
            // Two stacked 1-bpp masks (AND over XOR); real height is reported/2.
            mono_image(bytes, info, |color_type| match color_type {
                0 => BLACK,            // opaque black
                2 => WHITE,            // opaque white
                _ => TRANSPARENT,      // 1 (transparent) / 3 (invert → xor pass)
            })
        }
        _ => None, // unknown shape type → no cursor (capture continues)
    }
}

/// Convert a raw DXGI pointer-shape buffer into the invert/XOR BGRA image, or `None` when the
/// shape needs no invert pass (COLOR). Mirrors Sunshine `make_cursor_xor_image` (L211).
pub(super) fn make_cursor_xor_image(
    bytes: &[u8],
    info: &DXGI_OUTDUPL_POINTER_SHAPE_INFO,
) -> Option<(Vec<u8>, u32, u32)> {
    const INVERTED: u32 = 0xFFFF_FFFF;
    const TRANSPARENT: u32 = 0;
    let shape = DXGI_OUTDUPL_POINTER_SHAPE_TYPE(info.Type as i32);

    match shape {
        // COLOR needs no XOR pass.
        DXGI_OUTDUPL_POINTER_SHAPE_TYPE_COLOR => None,
        DXGI_OUTDUPL_POINTER_SHAPE_TYPE_MASKED_COLOR => {
            let (mut img, w, h) = repack_bgra(bytes, info.Width, info.Height, info.Pitch);
            for px in img.chunks_exact_mut(4) {
                let p = u32::from_le_bytes([px[0], px[1], px[2], px[3]]);
                if (p >> 24) == 0xFF {
                    // keep as-is (invert-blended)
                } else {
                    px.copy_from_slice(&TRANSPARENT.to_le_bytes());
                }
            }
            Some((img, w, h))
        }
        DXGI_OUTDUPL_POINTER_SHAPE_TYPE_MONOCHROME => {
            mono_image(bytes, info, |color_type| {
                if color_type == 3 {
                    INVERTED // drives the invert blend
                } else {
                    TRANSPARENT
                }
            })
        }
        _ => None,
    }
}

/// Copy a (possibly over-pitched) source BGRA bitmap into a tightly packed `width*4` buffer.
fn repack_bgra(src: &[u8], width: u32, height: u32, pitch: u32) -> (Vec<u8>, u32, u32) {
    let w = width as usize;
    let h = height as usize;
    let pitch = pitch as usize;
    let row_bytes = w * 4;
    let mut out = vec![0u8; row_bytes * h];
    for y in 0..h {
        let src_off = y * pitch;
        let dst_off = y * row_bytes;
        // Guard against a short source buffer (defensive — DXGI should always deliver pitch*h).
        if src_off + row_bytes <= src.len() {
            out[dst_off..dst_off + row_bytes]
                .copy_from_slice(&src[src_off..src_off + row_bytes]);
        }
    }
    (out, width, height)
}

/// Decode a MONOCHROME pointer shape (two stacked 1-bpp masks: AND over XOR) into a packed
/// BGRA image, mapping each pixel's `color_type` (0..3) via `map`. The reported `info.Height`
/// is the COMBINED height, so the real height is half. Bits are MSB-first within each byte.
fn mono_image(
    bytes: &[u8],
    info: &DXGI_OUTDUPL_POINTER_SHAPE_INFO,
    map: impl Fn(u32) -> u32,
) -> Option<(Vec<u8>, u32, u32)> {
    let width = info.Width;
    let h = info.Height / 2; // real cursor height (AND mask over XOR mask)
    if width == 0 || h == 0 {
        return None;
    }
    let pitch = info.Pitch as usize;
    let bytes_per_mask = pitch * h as usize;
    // AND mask starts at 0; XOR mask starts after the AND mask.
    if bytes.len() < bytes_per_mask * 2 {
        return None; // malformed buffer → skip the cursor rather than read OOB
    }
    let and_mask = &bytes[..bytes_per_mask];
    let xor_mask = &bytes[bytes_per_mask..bytes_per_mask * 2];

    let mut out = vec![0u8; (width * h * 4) as usize];
    for y in 0..h as usize {
        for x in 0..width as usize {
            let byte = x >> 3; // index within the 1-bpp row
            let bit = 1u8 << (7 - (x & 7)); // MSB-first within the byte
            let a = (and_mask[y * pitch + byte] & bit) != 0;
            let xo = (xor_mask[y * pitch + byte] & bit) != 0;
            // color_type truth table: 0=black, 1=transparent, 2=white, 3=invert.
            let color_type = (a as u32) + 2 * (xo as u32);
            let p = map(color_type);
            let off = (y * width as usize + x) * 4;
            out[off..off + 4].copy_from_slice(&p.to_le_bytes());
        }
    }
    Some((out, width, h))
}
