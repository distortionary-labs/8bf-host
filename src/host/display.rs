//! The `displayPixels` callback: drawing filter previews into dialogs.
//!
//! Filters render their own preview by passing a [`PSPixelMap`] and a
//! platform drawing context (an `HDC` on Windows, a pointer to a
//! `PSPlatformContext` holding a `CGContextRef` on macOS). The host converts
//! the requested rectangle to 8-bit RGB and blits it.
//!
//! The conversion is split in two so the risky part stays small:
//! [`PixelMapLayout::new`] checks the map's geometry and works out exactly
//! which bytes will be read, [`pixel_map_to_rgb`] turns those bytes into a
//! slice, and [`PixelMapLayout::to_rgb`] converts a slice safely.

use std::ffi::c_void;

use super::constants as c;
use super::structs::{PSPixelMap, VRect};

/// The first `PSPixelMap` version with a `depth` field.
const PIXEL_MAP_DEPTH_VERSION: i32 = 3;
/// Bits per sample assumed when the map does not say.
const DEFAULT_DEPTH: i16 = 8;
/// Bits per sample of a 16-bit map.
const DEPTH_16: i16 = 16;
/// Bytes per output pixel (R, G, B).
const RGB_BYTES: usize = 3;
/// Shift that reduces a Photoshop 16-bit sample (0..=32768) to 8 bits.
const PS16_TO_8_SHIFT: u32 = 7;

/// A validated description of the bytes a pixel-map rectangle occupies.
///
/// All offsets are relative to the first byte read, which is
/// [`Self::start`] bytes after `baseAddr`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PixelMapLayout {
    /// Output width in pixels (non-zero).
    pub width: usize,
    /// Output height in pixels (non-zero).
    pub height: usize,
    /// 1 for grayscale maps, 3 for RGB.
    pub channels: usize,
    /// Bytes per sample: 1 or 2.
    pub bytes_per_sample: usize,
    /// Distance between rows.
    pub row_bytes: usize,
    /// Distance between pixels (one sample for planar maps).
    pub col_bytes: usize,
    /// Distance between planes; only used by planar maps.
    pub plane_bytes: usize,
    /// True when each channel lives in its own plane.
    pub planar: bool,
    /// Offset of the first byte read from `baseAddr`.
    pub start: usize,
    /// Number of bytes from the first byte read to one past the last.
    pub extent: usize,
}

/// Converts a non-negative `i32` to `usize`.
fn non_negative(value: i32) -> Option<usize> {
    usize::try_from(value).ok()
}

impl PixelMapLayout {
    /// Validates `map` and `src` and works out which bytes a copy reads.
    ///
    /// Returns `None` when the request is invalid: an unsupported depth,
    /// negative strides or coordinates, a source rectangle that leaves
    /// `map.bounds`, or sizes that overflow. Returns `Some(None)` when there
    /// is nothing to draw (an empty rectangle), which is not an error.
    /// `baseAddr` is not inspected.
    pub fn new(map: &PSPixelMap, src: &VRect) -> Option<Option<Self>> {
        let depth = if map.version >= PIXEL_MAP_DEPTH_VERSION && map.depth != 0 {
            map.depth
        } else {
            DEFAULT_DEPTH
        };
        let bytes_per_sample = match depth {
            DEFAULT_DEPTH => 1,
            DEPTH_16 => 2,
            _ => return None,
        };
        if src.right <= src.left || src.bottom <= src.top {
            return Some(None);
        }
        let bounds = map.bounds;
        let inside = src.top >= bounds.top
            && src.left >= bounds.left
            && src.bottom <= bounds.bottom
            && src.right <= bounds.right;
        if !inside {
            return None;
        }
        let gray =
            [i32::from(c::MODE_GRAYSCALE), i32::from(c::MODE_GRAY16)].contains(&map.imageMode);
        let channels = if gray { 1 } else { RGB_BYTES };
        let col_bytes = match non_negative(map.colBytes)? {
            0 => channels * bytes_per_sample,
            col => col,
        };
        let row_bytes = non_negative(map.rowBytes)?;
        let plane_bytes = non_negative(map.planeBytes)?;
        let planar = col_bytes == bytes_per_sample && channels > 1;
        let (top, left) = (non_negative(src.top)?, non_negative(src.left)?);
        let width = non_negative(src.right - src.left)?;
        let height = non_negative(src.bottom - src.top)?;
        let start = top
            .checked_mul(row_bytes)?
            .checked_add(left.checked_mul(col_bytes)?)?;
        // Bytes read from the start of the last row.
        let last_row = if planar {
            (channels - 1)
                .checked_mul(plane_bytes)?
                .checked_add(width.checked_mul(bytes_per_sample)?)?
        } else {
            (width - 1)
                .checked_mul(col_bytes)?
                .checked_add(channels * bytes_per_sample)?
        };
        let extent = (height - 1).checked_mul(row_bytes)?.checked_add(last_row)?;
        let end = start.checked_add(extent)?;
        isize::try_from(end).ok()?;
        width.checked_mul(height)?.checked_mul(RGB_BYTES)?;
        Some(Some(Self {
            width,
            height,
            channels,
            bytes_per_sample,
            row_bytes,
            col_bytes,
            plane_bytes,
            planar,
            start,
            extent,
        }))
    }

    /// Reads the sample at `offset` as an 8-bit value.
    fn sample(&self, data: &[u8], offset: usize) -> u8 {
        if self.bytes_per_sample == 1 {
            return data.get(offset).copied().unwrap_or(0);
        }
        let Some(pair) = data.get(offset..offset + 2) else {
            return 0;
        };
        let value = u16::from_ne_bytes([pair[0], pair[1]]) >> PS16_TO_8_SHIFT;
        // `min` keeps the value within u8.
        value.min(u16::from(u8::MAX)) as u8
    }

    /// Converts the bytes described by this layout to packed 8-bit RGB.
    ///
    /// `data` starts at the first byte read and should be [`Self::extent`]
    /// bytes long. Missing bytes read as zero, so a short slice gives a
    /// partly black image rather than a panic. 16-bit samples are reduced
    /// with `min(255, v >> 7)`. Returns `None` if memory is short.
    pub fn to_rgb(&self, data: &[u8]) -> Option<Vec<u8>> {
        let size = self.width * self.height * RGB_BYTES;
        let mut out = Vec::new();
        out.try_reserve_exact(size).ok()?;
        for y in 0..self.height {
            let row = y * self.row_bytes;
            for x in 0..self.width {
                let mut pixel = [0u8; RGB_BYTES];
                for (channel, value) in pixel.iter_mut().enumerate().take(self.channels) {
                    let offset = if self.planar {
                        row + channel * self.plane_bytes + x * self.bytes_per_sample
                    } else {
                        row + x * self.col_bytes + channel * self.bytes_per_sample
                    };
                    *value = self.sample(data, offset);
                }
                if self.channels == 1 {
                    pixel = [pixel[0]; RGB_BYTES];
                }
                out.extend_from_slice(&pixel);
            }
        }
        Some(out)
    }
}

/// Returns `(width, height, rgb)` of 8-bit RGB for a rectangle of a pixel map.
///
/// Handles RGB and grayscale at 8 and 16 bits, both interleaved
/// (`colBytes` spans a whole pixel) and planar (`colBytes` is one sample
/// and `planeBytes` separates the planes). Other image modes are read as
/// RGB. An empty rectangle or a null `baseAddr` gives `(0, 0, [])`; an
/// invalid request (see [`PixelMapLayout::new`]) gives `None`.
///
/// # Safety
///
/// When `map.baseAddr` is not null, it must be valid for reads of every
/// byte the rectangle covers with the map's strides: for each row `y` of
/// `src`, the bytes from `y * rowBytes + left * colBytes` up to the end of
/// the row's last pixel (plus `planeBytes` per extra plane for planar
/// maps), all relative to `baseAddr`. Filters promise this when they call
/// `displayPixels`; the host cannot check it.
pub unsafe fn pixel_map_to_rgb(map: &PSPixelMap, src: &VRect) -> Option<(usize, usize, Vec<u8>)> {
    let Some(layout) = PixelMapLayout::new(map, src)? else {
        return Some((0, 0, Vec::new()));
    };
    if map.baseAddr.is_null() {
        return Some((0, 0, Vec::new()));
    }
    // SAFETY: `layout.start + layout.extent` fits in isize (checked by
    // `PixelMapLayout::new`), and the caller guarantees `baseAddr` is valid
    // for reads of every byte in that range, which is exactly the range the
    // rectangle covers. The slice is only read, and only during this call.
    let data = unsafe {
        let first = map.baseAddr.cast::<u8>().cast_const().add(layout.start);
        std::slice::from_raw_parts(first, layout.extent)
    };
    let rgb = layout.to_rgb(data)?;
    Some((layout.width, layout.height, rgb))
}

/// Draws a rectangle of a pixel map into a native context. Returns an `OSErr`.
///
/// Returns [`c::PARAM_ERR`] for a null context or an invalid pixel map,
/// [`c::NO_ERR`] when there is nothing to draw or drawing succeeded, and
/// [`c::ERR_PLUGIN_HOST_INSUFFICIENT`] on platforms without a native
/// drawing API.
///
/// # Safety
///
/// `map.baseAddr` must satisfy the requirements described under
/// [`pixel_map_to_rgb`], and `platform_context` must be null or a valid
/// device context (Windows) or `CGContextRef` (macOS), as described in the
/// module documentation. The filter-facing callback passes on what the
/// filter handed it.
pub unsafe fn display_pixels(
    map: &PSPixelMap,
    src: &VRect,
    dst_row: i32,
    dst_col: i32,
    platform_context: *mut c_void,
) -> i16 {
    if platform_context.is_null() {
        return c::PARAM_ERR;
    }
    // SAFETY: the filter that passed `map` to displayPixels guarantees its
    // pixel data covers `src` (see `pixel_map_to_rgb`).
    let Some((width, height, rgb)) = (unsafe { pixel_map_to_rgb(map, src) }) else {
        return c::PARAM_ERR;
    };
    if width == 0 {
        return c::NO_ERR;
    }
    platform::blit(platform_context, width, height, &rgb, dst_row, dst_col)
}

#[cfg(windows)]
mod platform {
    use std::ffi::c_void;

    use windows_sys::Win32::Graphics::Gdi::{
        SetDIBitsToDevice, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS, RGBQUAD,
    };

    use super::RGB_BYTES;
    use crate::host::constants as c;

    /// DIB rows are padded to a multiple of this many bytes.
    const DIB_ROW_ALIGN: usize = 4;
    /// Bits per pixel of a 24-bit DIB.
    const BITS_PER_PIXEL: u16 = 24;

    /// Draws packed RGB into an `HDC` with `SetDIBitsToDevice`.
    pub fn blit(
        hdc: *mut c_void,
        width: usize,
        height: usize,
        rgb: &[u8],
        dst_row: i32,
        dst_col: i32,
    ) -> i16 {
        let (Ok(width_i32), Ok(height_i32)) = (i32::try_from(width), i32::try_from(height)) else {
            return c::PARAM_ERR;
        };
        let Some(stride) = width
            .checked_mul(RGB_BYTES)
            .and_then(|bytes| bytes.checked_next_multiple_of(DIB_ROW_ALIGN))
        else {
            return c::PARAM_ERR;
        };
        let Some(size) = stride.checked_mul(height) else {
            return c::PARAM_ERR;
        };
        let mut bgr = Vec::new();
        if bgr.try_reserve_exact(size).is_err() {
            return c::MEM_FULL_ERR;
        }
        bgr.resize(size, 0);
        for (out_row, in_row) in bgr
            .chunks_exact_mut(stride)
            .zip(rgb.chunks_exact(width * RGB_BYTES))
        {
            for (out, pixel) in out_row
                .chunks_exact_mut(RGB_BYTES)
                .zip(in_row.chunks_exact(RGB_BYTES))
            {
                out.copy_from_slice(&[pixel[2], pixel[1], pixel[0]]);
            }
        }
        let info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                // The header is 40 bytes.
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: width_i32,
                biHeight: -height_i32, // Negative: rows run top-down.
                biPlanes: 1,
                biBitCount: BITS_PER_PIXEL,
                biCompression: BI_RGB,
                biSizeImage: 0,
                biXPelsPerMeter: 0,
                biYPelsPerMeter: 0,
                biClrUsed: 0,
                biClrImportant: 0,
            },
            bmiColors: [RGBQUAD {
                rgbBlue: 0,
                rgbGreen: 0,
                rgbRed: 0,
                rgbReserved: 0,
            }],
        };
        // Both values came from a positive i32 above.
        let (width_u32, height_u32) = (width_i32 as u32, height_i32 as u32);
        // SAFETY: `bgr` holds `height` rows of `stride` bytes, the layout
        // `info` describes; both outlive the call. `hdc` is the device
        // context the filter passed, which GDI validates itself.
        let lines = unsafe {
            SetDIBitsToDevice(
                hdc,
                dst_col,
                dst_row,
                width_u32,
                height_u32,
                0,
                0,
                0,
                height_u32,
                bgr.as_ptr().cast(),
                &info,
                DIB_RGB_COLORS,
            )
        };
        if lines == 0 {
            c::PARAM_ERR
        } else {
            c::NO_ERR
        }
    }
}

#[cfg(target_os = "macos")]
mod platform {
    use std::ffi::c_void;

    use super::RGB_BYTES;
    use crate::host::constants as c;

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct CGPoint {
        x: f64,
        y: f64,
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct CGSize {
        width: f64,
        height: f64,
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct CGRect {
        origin: CGPoint,
        size: CGSize,
    }

    /// `kCGImageAlphaNone`.
    const IMAGE_ALPHA_NONE: u32 = 0;
    /// `kCGRenderingIntentDefault`.
    const RENDERING_INTENT_DEFAULT: i32 = 0;
    /// Bits per component of 8-bit RGB.
    const BITS_PER_COMPONENT: usize = 8;

    #[link(name = "CoreFoundation", kind = "framework")]
    extern "C" {
        fn CFDataCreate(allocator: *const c_void, bytes: *const u8, length: isize)
            -> *const c_void;
        fn CFRelease(object: *const c_void);
    }

    #[link(name = "CoreGraphics", kind = "framework")]
    extern "C" {
        fn CGColorSpaceCreateDeviceRGB() -> *mut c_void;
        fn CGColorSpaceRelease(space: *mut c_void);
        fn CGDataProviderCreateWithCFData(data: *const c_void) -> *mut c_void;
        fn CGDataProviderRelease(provider: *mut c_void);
        #[allow(clippy::too_many_arguments)]
        fn CGImageCreate(
            width: usize,
            height: usize,
            bits_per_component: usize,
            bits_per_pixel: usize,
            bytes_per_row: usize,
            space: *mut c_void,
            bitmap_info: u32,
            provider: *mut c_void,
            decode: *const f64,
            should_interpolate: bool,
            intent: i32,
        ) -> *mut c_void;
        fn CGImageRelease(image: *mut c_void);
        fn CGContextDrawImage(context: *mut c_void, rect: CGRect, image: *mut c_void);
    }

    /// Draws packed RGB into the `CGContextRef` held by a `PSPlatformContext`.
    pub fn blit(
        platform_context: *mut c_void,
        width: usize,
        height: usize,
        rgb: &[u8],
        dst_row: i32,
        dst_col: i32,
    ) -> i16 {
        // SAFETY: on macOS the filter passes a pointer to a
        // PSPlatformContext, whose first member is the CGContextRef.
        let context = unsafe { platform_context.cast::<*mut c_void>().read() };
        if context.is_null() {
            return c::PARAM_ERR;
        }
        let Ok(length) = isize::try_from(rgb.len()) else {
            return c::PARAM_ERR;
        };
        // SAFETY: `rgb` is valid for `length` bytes; CFDataCreate copies
        // them, so the image never refers to our buffer after we return.
        let data = unsafe { CFDataCreate(std::ptr::null(), rgb.as_ptr(), length) };
        if data.is_null() {
            return c::MEM_FULL_ERR;
        }
        // SAFETY: every object passed below was created here, is checked
        // for null before use, and is released exactly once. `context` is
        // the filter's live drawing context.
        unsafe {
            let provider = CGDataProviderCreateWithCFData(data);
            let space = CGColorSpaceCreateDeviceRGB();
            let mut result = c::MEM_FULL_ERR;
            if !provider.is_null() && !space.is_null() {
                let image = CGImageCreate(
                    width,
                    height,
                    BITS_PER_COMPONENT,
                    BITS_PER_COMPONENT * RGB_BYTES,
                    width * RGB_BYTES,
                    space,
                    IMAGE_ALPHA_NONE,
                    provider,
                    std::ptr::null(),
                    false,
                    RENDERING_INTENT_DEFAULT,
                );
                if !image.is_null() {
                    let rect = CGRect {
                        origin: CGPoint {
                            x: f64::from(dst_col),
                            y: f64::from(dst_row),
                        },
                        // Sizes come from i32 rect edges, so they are exact in f64.
                        size: CGSize {
                            width: width as f64,
                            height: height as f64,
                        },
                    };
                    CGContextDrawImage(context, rect, image);
                    CGImageRelease(image);
                    result = c::NO_ERR;
                }
            }
            if !space.is_null() {
                CGColorSpaceRelease(space);
            }
            if !provider.is_null() {
                CGDataProviderRelease(provider);
            }
            CFRelease(data);
            result
        }
    }
}

#[cfg(not(any(windows, target_os = "macos")))]
mod platform {
    use std::ffi::c_void;

    use crate::host::constants as c;

    /// No native drawing API: tell the filter the host cannot draw.
    pub fn blit(
        _context: *mut c_void,
        _width: usize,
        _height: usize,
        _rgb: &[u8],
        _dst_row: i32,
        _dst_col: i32,
    ) -> i16 {
        c::ERR_PLUGIN_HOST_INSUFFICIENT
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(top: i32, left: i32, bottom: i32, right: i32) -> VRect {
        VRect {
            top,
            left,
            bottom,
            right,
        }
    }

    fn pixel_map(
        data: &mut [u8],
        bounds: VRect,
        mode: i16,
        row: i32,
        col: i32,
        plane: i32,
        depth: i16,
    ) -> PSPixelMap {
        PSPixelMap {
            version: 3,
            bounds,
            imageMode: i32::from(mode),
            rowBytes: row,
            colBytes: col,
            planeBytes: plane,
            baseAddr: data.as_mut_ptr().cast(),
            mat: std::ptr::null_mut(),
            masks: std::ptr::null_mut(),
            maskPhaseRow: 0,
            maskPhaseCol: 0,
            pixelOverlays: std::ptr::null_mut(),
            colorManagementOptions: 0,
            depth,
        }
    }

    fn convert(map: &PSPixelMap, src: VRect) -> Option<(usize, usize, Vec<u8>)> {
        // SAFETY: every test map points at a buffer covering its bounds.
        unsafe { pixel_map_to_rgb(map, &src) }
    }

    #[test]
    fn interleaved_rgb_sub_rectangle() {
        // 3x2 RGB image, rows padded to 10 bytes.
        let mut data: Vec<u8> = (0..20).collect();
        let map = pixel_map(&mut data, rect(0, 0, 2, 3), c::MODE_RGB, 10, 3, 0, 8);
        let (width, height, rgb) = convert(&map, rect(0, 1, 2, 3)).unwrap();
        assert_eq!((width, height), (2, 2));
        assert_eq!(rgb, vec![3, 4, 5, 6, 7, 8, 13, 14, 15, 16, 17, 18]);
    }

    #[test]
    fn interleaved_rgba_skips_alpha() {
        let mut data = vec![1, 2, 3, 99, 4, 5, 6, 99];
        let map = pixel_map(&mut data, rect(0, 0, 1, 2), c::MODE_RGB, 8, 4, 0, 8);
        assert_eq!(
            convert(&map, rect(0, 0, 1, 2)).unwrap().2,
            vec![1, 2, 3, 4, 5, 6]
        );
    }

    #[test]
    fn planar_rgb() {
        // Two pixels per plane, planes 4 bytes apart, one row.
        let mut data = vec![10, 11, 0, 0, 20, 21, 0, 0, 30, 31];
        let map = pixel_map(&mut data, rect(0, 0, 1, 2), c::MODE_RGB, 2, 1, 4, 8);
        assert_eq!(
            convert(&map, rect(0, 0, 1, 2)).unwrap().2,
            vec![10, 20, 30, 11, 21, 31]
        );
    }

    #[test]
    fn grayscale_and_default_col_bytes() {
        let mut data = vec![7, 8, 9, 10];
        let map = pixel_map(&mut data, rect(0, 0, 2, 2), c::MODE_GRAYSCALE, 2, 0, 0, 8);
        assert_eq!(
            convert(&map, rect(1, 1, 2, 2)).unwrap(),
            (1, 1, vec![10, 10, 10])
        );
    }

    #[test]
    fn sixteen_bit_samples_are_reduced() {
        let samples: [u16; 3] = [32768, 16384, 127];
        let mut data: Vec<u8> = samples.iter().flat_map(|v| v.to_ne_bytes()).collect();
        let map = pixel_map(&mut data, rect(0, 0, 1, 1), c::MODE_RGB48, 6, 6, 0, 16);
        assert_eq!(
            convert(&map, rect(0, 0, 1, 1)).unwrap().2,
            vec![255, 128, 0]
        );
        let mut gray: Vec<u8> = 40000u16.to_ne_bytes().to_vec();
        let map = pixel_map(&mut gray, rect(0, 0, 1, 1), c::MODE_GRAY16, 2, 0, 0, 16);
        assert_eq!(convert(&map, rect(0, 0, 1, 1)).unwrap().2, vec![255; 3]);
    }

    #[test]
    fn old_versions_ignore_the_depth_field() {
        let mut data = vec![1, 2, 3];
        let mut map = pixel_map(&mut data, rect(0, 0, 1, 1), c::MODE_RGB, 3, 3, 0, 16);
        map.version = 2;
        assert_eq!(convert(&map, rect(0, 0, 1, 1)).unwrap().2, vec![1, 2, 3]);
    }

    #[test]
    fn empty_rects_and_null_data_draw_nothing() {
        let mut data = vec![0; 3];
        let map = pixel_map(&mut data, rect(0, 0, 1, 1), c::MODE_RGB, 3, 3, 0, 8);
        assert_eq!(convert(&map, rect(0, 0, 0, 1)).unwrap(), (0, 0, Vec::new()));
        assert_eq!(convert(&map, rect(0, 1, 1, 0)).unwrap(), (0, 0, Vec::new()));
        let mut null = map;
        null.baseAddr = std::ptr::null_mut();
        assert_eq!(
            convert(&null, rect(0, 0, 1, 1)).unwrap(),
            (0, 0, Vec::new())
        );
    }

    #[test]
    fn invalid_maps_are_rejected() {
        let mut data = vec![0; 64];
        let bounds = rect(0, 0, 2, 2);
        let good = pixel_map(&mut data, bounds, c::MODE_RGB, 6, 3, 0, 8);
        assert!(convert(&good, rect(0, 0, 2, 2)).is_some());
        assert!(convert(&good, rect(0, 0, 3, 2)).is_none(), "outside bounds");
        assert!(convert(&good, rect(-1, 0, 1, 2)).is_none(), "negative top");
        let mut bad = good;
        bad.depth = 32;
        assert!(convert(&bad, rect(0, 0, 1, 1)).is_none(), "depth");
        let mut bad = good;
        bad.rowBytes = -6;
        assert!(
            convert(&bad, rect(0, 0, 1, 1)).is_none(),
            "negative rowBytes"
        );
        let mut bad = good;
        bad.colBytes = -3;
        assert!(
            convert(&bad, rect(0, 0, 1, 1)).is_none(),
            "negative colBytes"
        );
        let mut bad = good;
        bad.colBytes = 1;
        bad.planeBytes = -1;
        assert!(
            convert(&bad, rect(0, 0, 1, 1)).is_none(),
            "negative planeBytes"
        );
    }

    #[test]
    fn layout_reports_exact_extent() {
        let mut data = vec![0; 64];
        let map = pixel_map(&mut data, rect(0, 0, 4, 4), c::MODE_RGB, 16, 4, 0, 8);
        let layout = PixelMapLayout::new(&map, &rect(1, 2, 3, 4))
            .unwrap()
            .unwrap();
        // Starts at row 1, column 2; ends after the RGB of the last pixel.
        assert_eq!(layout.start, 16 + 8);
        assert_eq!(layout.extent, 16 + 4 + 3);
        assert!(!layout.planar);
        let planar = pixel_map(&mut data, rect(0, 0, 2, 4), c::MODE_RGB, 4, 1, 8, 8);
        let layout = PixelMapLayout::new(&planar, &rect(0, 0, 2, 4))
            .unwrap()
            .unwrap();
        assert!(layout.planar);
        assert_eq!(layout.extent, 4 + 2 * 8 + 4);
    }

    #[test]
    fn to_rgb_tolerates_short_slices() {
        let mut data = vec![0; 3];
        let map = pixel_map(&mut data, rect(0, 0, 1, 2), c::MODE_RGB, 6, 3, 0, 8);
        let layout = PixelMapLayout::new(&map, &rect(0, 0, 1, 2))
            .unwrap()
            .unwrap();
        assert_eq!(
            layout.to_rgb(&[1, 2, 3, 4]).unwrap(),
            vec![1, 2, 3, 4, 0, 0]
        );
    }

    #[test]
    fn display_pixels_return_codes() {
        let mut data = vec![5; 12];
        let map = pixel_map(&mut data, rect(0, 0, 2, 2), c::MODE_RGB, 6, 3, 0, 8);
        let null = std::ptr::null_mut();
        // SAFETY: `map` points at `data`, which outlives every call; the
        // context is only dereferenced on Windows and macOS, where the
        // drawing case below is skipped.
        let draw = |area: VRect, context: *mut c_void| unsafe {
            display_pixels(&map, &area, 0, 0, context)
        };
        assert_eq!(draw(rect(0, 0, 2, 2), null), c::PARAM_ERR);
        let mut context = 0usize;
        let context = (&mut context as *mut usize).cast::<c_void>();
        assert_eq!(draw(rect(0, 0, 0, 0), context), c::NO_ERR);
        assert_eq!(draw(rect(0, 0, 9, 9), context), c::PARAM_ERR);
        #[cfg(not(any(windows, target_os = "macos")))]
        assert_eq!(
            draw(rect(0, 0, 2, 2), context),
            c::ERR_PLUGIN_HOST_INSUFFICIENT
        );
    }
}
