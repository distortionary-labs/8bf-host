//! Interleaved image buffers and the rect/plane copies filters ask for.
//!
//! Photoshop hands filters rectangles of interleaved planes: for an RGBA
//! image and planes 0..=2 a filter gets R,G,B triplets, for plane 3 alone it
//! gets the alpha samples. [`PlaneImage`] keeps an image as one interleaved
//! byte buffer and implements those copies.
//!
//! 16-bit samples are native-endian `u16` in Photoshop's 0..=32768 range.
//!
//! Every method that takes coordinates or plane numbers from a filter
//! validates them: bad requests produce an error code or are clipped, never
//! a panic.

use serde::{Deserialize, Serialize};

use super::constants as c;

/// Bits per sample of an 8-bit image.
pub const DEPTH_8: u32 = 8;
/// Bits per sample of a 16-bit image.
pub const DEPTH_16: u32 = 16;

/// The largest byte value a numeric padding mode may ask for.
const MAX_PADDING_BYTE: i16 = 0xFF;

/// A rectangle in image coordinates: `top..bottom` rows, `left..right` columns.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Rect {
    pub top: i32,
    pub left: i32,
    pub bottom: i32,
    pub right: i32,
}

impl Rect {
    /// Creates a rect from its edges, in Photoshop's top, left, bottom, right order.
    pub const fn new(top: i32, left: i32, bottom: i32, right: i32) -> Self {
        Self {
            top,
            left,
            bottom,
            right,
        }
    }

    /// Returns true when the rect covers no pixels (including inverted rects).
    pub const fn is_empty(&self) -> bool {
        self.bottom <= self.top || self.right <= self.left
    }

    /// `right - left`, saturating; negative for an inverted rect.
    pub const fn width(&self) -> i32 {
        self.right.saturating_sub(self.left)
    }

    /// `bottom - top`, saturating; negative for an inverted rect.
    pub const fn height(&self) -> i32 {
        self.bottom.saturating_sub(self.top)
    }
}

/// Errors from constructing a [`PlaneImage`].
#[derive(Debug, thiserror::Error)]
pub enum ImageError {
    #[error("depth must be 8 or 16, not {0}")]
    BadDepth(u32),
    #[error("image data is {actual} bytes, expected {expected}")]
    BadSize { actual: usize, expected: usize },
    #[error("image dimensions {width}x{height}x{channels} are too large")]
    TooLarge {
        width: usize,
        height: usize,
        channels: usize,
    },
}

/// An interleaved image of `channels` samples per pixel.
///
/// The fields are public for convenience, but code that changes `width`,
/// `height`, `channels` or `depth` must keep `data.len()` equal to
/// `width * height * channels * depth / 8`. Methods tolerate a mismatch
/// without panicking, but the results are then meaningless.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlaneImage {
    pub width: usize,
    pub height: usize,
    pub channels: usize,
    pub depth: u32,
    pub data: Vec<u8>,
}

/// Returns `width * height * channels * depth / 8`, or the error that prevents it.
fn image_size(
    width: usize,
    height: usize,
    channels: usize,
    depth: u32,
) -> Result<usize, ImageError> {
    let bytes_per_sample = match depth {
        DEPTH_8 => 1,
        DEPTH_16 => 2,
        other => return Err(ImageError::BadDepth(other)),
    };
    width
        .checked_mul(height)
        .and_then(|pixels| pixels.checked_mul(channels))
        .and_then(|samples| samples.checked_mul(bytes_per_sample))
        .filter(|&size| isize::try_from(size).is_ok())
        .ok_or(ImageError::TooLarge {
            width,
            height,
            channels,
        })
}

/// Clamps an `i64` coordinate into `0..=limit`.
fn clamp_coordinate(value: i64, limit: usize) -> usize {
    let limit_i64 = i64::try_from(limit).unwrap_or(i64::MAX);
    usize::try_from(value.clamp(0, limit_i64)).unwrap_or(0)
}

/// The fill byte for a padding mode: the value itself for 0..=255, else zero.
fn padding_byte(padding: i16) -> u8 {
    u8::try_from(padding).unwrap_or(0)
}

impl PlaneImage {
    /// Creates a zero-filled image.
    ///
    /// # Errors
    ///
    /// [`ImageError::BadDepth`] unless `depth` is 8 or 16, and
    /// [`ImageError::TooLarge`] when the buffer size overflows or cannot be
    /// allocated.
    pub fn new(
        width: usize,
        height: usize,
        channels: usize,
        depth: u32,
    ) -> Result<Self, ImageError> {
        let size = image_size(width, height, channels, depth)?;
        let mut data = Vec::new();
        data.try_reserve_exact(size)
            .map_err(|_| ImageError::TooLarge {
                width,
                height,
                channels,
            })?;
        data.resize(size, 0);
        Ok(Self {
            width,
            height,
            channels,
            depth,
            data,
        })
    }

    /// Wraps existing interleaved samples.
    ///
    /// # Errors
    ///
    /// [`ImageError::BadDepth`] unless `depth` is 8 or 16,
    /// [`ImageError::TooLarge`] when the expected size overflows, and
    /// [`ImageError::BadSize`] when `data` is not exactly
    /// `width * height * channels * depth / 8` bytes.
    pub fn from_data(
        width: usize,
        height: usize,
        channels: usize,
        depth: u32,
        data: Vec<u8>,
    ) -> Result<Self, ImageError> {
        let expected = image_size(width, height, channels, depth)?;
        if data.len() != expected {
            return Err(ImageError::BadSize {
                actual: data.len(),
                expected,
            });
        }
        Ok(Self {
            width,
            height,
            channels,
            depth,
            data,
        })
    }

    /// Bytes per sample: 1 for 8-bit images, 2 for 16-bit images.
    pub fn bytes_per_sample(&self) -> usize {
        if self.depth == DEPTH_16 {
            2
        } else {
            1
        }
    }

    /// Bytes per interleaved pixel.
    pub fn pixel_bytes(&self) -> usize {
        self.channels.saturating_mul(self.bytes_per_sample())
    }

    /// Bytes per row of the interleaved buffer.
    pub fn row_bytes(&self) -> usize {
        self.width.saturating_mul(self.pixel_bytes())
    }

    /// Returns a nearest-neighbour copy reduced by an integer factor.
    ///
    /// A factor of 0 or 1, or an image with no pixels, returns a copy.
    /// Each dimension of the result is at least 1.
    pub fn scaled(&self, factor: u32) -> PlaneImage {
        let factor = usize::try_from(factor).unwrap_or(usize::MAX);
        if factor <= 1 || self.width == 0 || self.height == 0 || !self.is_consistent() {
            return self.clone();
        }
        let width = (self.width / factor).max(1);
        let height = (self.height / factor).max(1);
        let pixel = self.pixel_bytes();
        let src_row_bytes = self.row_bytes();
        // The result is never larger than `self`, so this cannot overflow.
        let mut data = Vec::with_capacity(width * height * pixel);
        for y in 0..height {
            let row_start = y * factor * src_row_bytes;
            for x in 0..width {
                let start = row_start + x * factor * pixel;
                data.extend_from_slice(&self.data[start..start + pixel]);
            }
        }
        PlaneImage {
            width,
            height,
            channels: self.channels,
            depth: self.depth,
            data,
        }
    }

    /// Returns true when `data` has the length the dimensions promise.
    fn is_consistent(&self) -> bool {
        image_size(self.width, self.height, self.channels, self.depth)
            .is_ok_and(|size| size == self.data.len())
    }

    /// Bytes per output pixel for planes `lo..=hi`, or `None` when invalid.
    fn plane_span(&self, lo: usize, hi: usize) -> Option<usize> {
        let planes = hi.checked_sub(lo)?.checked_add(1)?;
        planes.checked_mul(self.bytes_per_sample())
    }

    /// Writes planes `lo..=hi` of the pixel at `index` into `out`.
    ///
    /// Planes at or beyond `channels` get `dummy_value` (its low byte for
    /// 8-bit images). `out` must be exactly one output pixel long.
    fn copy_pixel(&self, index: usize, lo: usize, hi: usize, dummy_value: u16, out: &mut [u8]) {
        let bps = self.bytes_per_sample();
        let pixel = self.pixel_bytes();
        let dummy_bytes = dummy_value.to_ne_bytes();
        let dummy: &[u8] = if bps == 1 {
            &[dummy_value.to_le_bytes()[0]]
        } else {
            &dummy_bytes
        };
        let start = index.saturating_mul(pixel);
        let source = self.data.get(start..start.saturating_add(pixel));
        for (sample, plane) in out.chunks_exact_mut(bps).zip(lo..=hi) {
            let value = (plane < self.channels)
                .then(|| source.and_then(|s| s.get(plane * bps..(plane + 1) * bps)))
                .flatten()
                .unwrap_or(dummy);
            sample.copy_from_slice(value);
        }
    }

    /// Fills `out` with planes `lo..=hi` of row `y` for columns `left..right`.
    ///
    /// `out` must be `(right - left) * plane_span` bytes long and `y` must
    /// be a valid row; callers check both.
    #[allow(clippy::too_many_arguments)]
    fn fill_row(
        &self,
        y: usize,
        left: i64,
        right: i64,
        lo: usize,
        hi: usize,
        padding: i16,
        dummy_value: u16,
        out: &mut [u8],
    ) {
        let Some(span) = self.plane_span(lo, hi) else {
            return;
        };
        out.fill(padding_byte(padding));
        let inner_left = clamp_coordinate(left, self.width);
        let inner_right = clamp_coordinate(right, self.width);
        let row_start = y.saturating_mul(self.width);
        let edge = padding == c::PADDING_EDGE_REPLICATION;
        if inner_left < inner_right {
            // `left <= inner_left`, so the offset is non-negative.
            let pad_left = usize::try_from(i64::try_from(inner_left).unwrap_or(0) - left)
                .unwrap_or(0)
                .saturating_mul(span);
            let count = inner_right - inner_left;
            let inner_end = pad_left + count * span;
            let full_row = lo == 0 && hi + 1 == self.channels;
            if full_row {
                let start = (row_start + inner_left) * span;
                if let Some(source) = self.data.get(start..start + count * span) {
                    out[pad_left..inner_end].copy_from_slice(source);
                }
            } else {
                for (offset, pixel) in out[pad_left..inner_end].chunks_exact_mut(span).enumerate() {
                    self.copy_pixel(row_start + inner_left + offset, lo, hi, dummy_value, pixel);
                }
            }
            if edge {
                let (before, rest) = out.split_at_mut(pad_left);
                let first = &rest[..span];
                for pixel in before.chunks_exact_mut(span) {
                    pixel.copy_from_slice(first);
                }
                let (inner, after) = out.split_at_mut(inner_end);
                let last = &inner[inner_end - span..];
                for pixel in after.chunks_exact_mut(span) {
                    pixel.copy_from_slice(last);
                }
            }
        } else if edge && self.width > 0 {
            let column = if right <= 0 { 0 } else { self.width - 1 };
            let mut pixel = vec![0u8; span];
            self.copy_pixel(row_start + column, lo, hi, dummy_value, &mut pixel);
            for chunk in out.chunks_exact_mut(span) {
                chunk.copy_from_slice(&pixel);
            }
        }
    }

    /// Returns one row of planes `lo..=hi` for columns `left..right`.
    ///
    /// Columns outside the image are padded according to `padding`: edge
    /// replication, a fixed byte value 0..=255, or zeros for any other
    /// mode. Planes at or beyond `channels` get `dummy_value`. A row `y`
    /// outside the image is treated like a row of an image with no pixels.
    /// Returns an empty vector when `hi < lo`, `right <= left` or the row
    /// size overflows.
    #[allow(clippy::too_many_arguments)]
    pub fn extract_row(
        &self,
        y: usize,
        left: i32,
        right: i32,
        lo: usize,
        hi: usize,
        padding: i16,
        dummy_value: u16,
    ) -> Vec<u8> {
        let Some(span) = self.plane_span(lo, hi) else {
            return Vec::new();
        };
        let Some(length) = usize::try_from(i64::from(right) - i64::from(left))
            .ok()
            .and_then(|width| width.checked_mul(span))
        else {
            return Vec::new();
        };
        let mut out = Vec::new();
        if out.try_reserve_exact(length).is_err() {
            return Vec::new();
        }
        out.resize(length, 0);
        if y < self.height {
            let (left, right) = (i64::from(left), i64::from(right));
            self.fill_row(y, left, right, lo, hi, padding, dummy_value, &mut out);
        } else {
            out.fill(padding_byte(padding));
        }
        out
    }

    /// Copies planes `lo..=hi` of `rect` into `dst`, one row every `row_bytes`.
    ///
    /// Row `i` of the rect starts at `dst[i * row_bytes]`. Areas outside the
    /// image are padded according to `padding` (see [`Self::extract_row`]);
    /// rows above or below the image are replicated from the nearest edge
    /// row in edge-replication mode. An empty rect copies nothing.
    ///
    /// Returns [`c::NO_ERR`] or [`c::PARAM_ERR`]. The error is returned when
    /// `hi < lo`, when `dst` is too small, when rows would overlap
    /// (`row_bytes` smaller than a row with more than one row), and when the
    /// rect leaves the image and `padding` is
    /// [`c::PADDING_ERROR_ON_BOUNDS`] or not a valid padding mode.
    #[allow(clippy::too_many_arguments)]
    pub fn read_rect(
        &self,
        rect: Rect,
        lo: usize,
        hi: usize,
        dst: &mut [u8],
        row_bytes: usize,
        padding: i16,
        dummy_value: u16,
    ) -> i16 {
        let Some(span) = self.plane_span(lo, hi) else {
            return c::PARAM_ERR;
        };
        let height = i64::try_from(self.height).unwrap_or(i64::MAX);
        let width = i64::try_from(self.width).unwrap_or(i64::MAX);
        let (top, left) = (i64::from(rect.top), i64::from(rect.left));
        let (bottom, right) = (i64::from(rect.bottom), i64::from(rect.right));
        let out_of_bounds = top < 0 || left < 0 || bottom > height || right > width;
        let valid_padding = (c::PADDING_ERROR_ON_BOUNDS + 1..=MAX_PADDING_BYTE).contains(&padding);
        if out_of_bounds && !valid_padding {
            return c::PARAM_ERR;
        }
        if rect.is_empty() {
            return c::NO_ERR;
        }
        let rows = usize::try_from(bottom - top).unwrap_or(usize::MAX);
        let Some(width_bytes) = usize::try_from(right - left)
            .ok()
            .and_then(|columns| columns.checked_mul(span))
        else {
            return c::PARAM_ERR;
        };
        if rows > 1 && row_bytes < width_bytes {
            return c::PARAM_ERR;
        }
        let required = (rows - 1)
            .checked_mul(row_bytes)
            .and_then(|offset| offset.checked_add(width_bytes));
        if required.is_none_or(|required| dst.len() < required) {
            return c::PARAM_ERR;
        }
        for (row_index, y) in (top..bottom).enumerate() {
            let start = row_index * row_bytes;
            let out = &mut dst[start..start + width_bytes];
            let source_row = if (0..height).contains(&y) {
                Some(y)
            } else if padding == c::PADDING_EDGE_REPLICATION && height > 0 {
                Some(if y < 0 { 0 } else { height - 1 })
            } else {
                None
            };
            match source_row.and_then(|row| usize::try_from(row).ok()) {
                Some(row) => self.fill_row(row, left, right, lo, hi, padding, dummy_value, out),
                None => out.fill(padding_byte(padding)),
            }
        }
        c::NO_ERR
    }

    /// Stores planes `lo..=hi` from `src` into `rect`, clipped to the image.
    ///
    /// Row `i` of the rect starts at `src[i * row_bytes]`. Planes at or
    /// beyond `channels` are skipped. Invalid requests (see
    /// [`Self::try_write_rect`]) leave the image unchanged.
    pub fn write_rect(&mut self, rect: Rect, lo: usize, hi: usize, src: &[u8], row_bytes: usize) {
        // The error code only matters to callers that report it; an invalid
        // request has already been rejected without touching the image.
        let _ = self.try_write_rect(rect, lo, hi, src, row_bytes);
    }

    /// Like [`Self::write_rect`], but returns a Photoshop error code.
    ///
    /// Returns [`c::PARAM_ERR`] without changing the image when `hi < lo`
    /// or `src` is too short for the clipped rect, and [`c::NO_ERR`]
    /// otherwise (including when the rect misses the image entirely).
    pub fn try_write_rect(
        &mut self,
        rect: Rect,
        lo: usize,
        hi: usize,
        src: &[u8],
        row_bytes: usize,
    ) -> i16 {
        let Some(span) = self.plane_span(lo, hi) else {
            return c::PARAM_ERR;
        };
        if !self.is_consistent() {
            return c::PARAM_ERR;
        }
        let clip_top = clamp_coordinate(i64::from(rect.top), self.height);
        let clip_bottom = clamp_coordinate(i64::from(rect.bottom), self.height);
        let clip_left = clamp_coordinate(i64::from(rect.left), self.width);
        let clip_right = clamp_coordinate(i64::from(rect.right), self.width);
        if clip_top >= clip_bottom || clip_left >= clip_right {
            return c::NO_ERR;
        }
        let count = clip_right - clip_left;
        let skip_columns = i64::try_from(clip_left).unwrap_or(0) - i64::from(rect.left);
        let skip_rows = i64::try_from(clip_top).unwrap_or(0) - i64::from(rect.top);
        let rows = clip_bottom - clip_top;
        let layout = usize::try_from(skip_columns)
            .ok()
            .and_then(|columns| columns.checked_mul(span))
            .zip(count.checked_mul(span))
            .zip(usize::try_from(skip_rows).ok());
        let Some(((src_skip, src_len), first_row)) = layout else {
            return c::PARAM_ERR;
        };
        let last_end = (first_row + rows - 1)
            .checked_mul(row_bytes)
            .and_then(|offset| offset.checked_add(src_skip))
            .and_then(|offset| offset.checked_add(src_len));
        if last_end.is_none_or(|end| end > src.len()) {
            return c::PARAM_ERR;
        }
        let bps = self.bytes_per_sample();
        let pixel = self.pixel_bytes();
        let image_row_bytes = self.row_bytes();
        let full_row = lo == 0 && hi + 1 == self.channels;
        for row in 0..rows {
            let source_start = (first_row + row) * row_bytes + src_skip;
            let source = &src[source_start..source_start + src_len];
            let start = (clip_top + row) * image_row_bytes + clip_left * pixel;
            let target = &mut self.data[start..start + count * pixel];
            if full_row {
                target.copy_from_slice(source);
                continue;
            }
            for (out_pixel, in_pixel) in target
                .chunks_exact_mut(pixel)
                .zip(source.chunks_exact(span))
            {
                for (index, plane) in (lo..=hi).enumerate() {
                    if plane >= self.channels {
                        continue;
                    }
                    out_pixel[plane * bps..(plane + 1) * bps]
                        .copy_from_slice(&in_pixel[index * bps..(index + 1) * bps]);
                }
            }
        }
        c::NO_ERR
    }

    /// Returns one plane as contiguous samples.
    ///
    /// Returns an empty vector when `index` is not a channel of the image.
    pub fn plane(&self, index: usize) -> Vec<u8> {
        if index >= self.channels {
            return Vec::new();
        }
        let bps = self.bytes_per_sample();
        self.data
            .chunks_exact(self.pixel_bytes())
            .flat_map(|pixel| &pixel[index * bps..(index + 1) * bps])
            .copied()
            .collect()
    }

    /// Replaces one plane with contiguous samples.
    ///
    /// Does nothing when `index` is not a channel of the image. When
    /// `values` holds fewer samples than the image has pixels, only that
    /// many leading pixels change; extra samples are ignored.
    pub fn set_plane(&mut self, index: usize, values: &[u8]) {
        if index >= self.channels {
            return;
        }
        let bps = self.bytes_per_sample();
        let pixel = self.pixel_bytes();
        for (target, value) in self
            .data
            .chunks_exact_mut(pixel)
            .zip(values.chunks_exact(bps))
        {
            target[index * bps..(index + 1) * bps].copy_from_slice(value);
        }
    }
}

/// Returns the integer sampling factor for a 16.16 fixed-point rate.
///
/// The rate is rounded to the nearest integer; rates below one half
/// (including zero and negative rates) give 1.
///
/// # Examples
///
/// ```
/// use eightbf_host::host::image::scale_factor;
/// assert_eq!(scale_factor(0x30000), 3);
/// assert_eq!(scale_factor(0), 1);
/// ```
pub fn scale_factor(rate: i32) -> u32 {
    if rate <= 0 {
        return 1;
    }
    let one = i64::from(c::FIXED_ONE);
    let rounded = (i64::from(rate) + one / 2) / one;
    u32::try_from(rounded).unwrap_or(1).max(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    const EDGE: i16 = c::PADDING_EDGE_REPLICATION;

    fn numbered(width: usize, height: usize, channels: usize) -> PlaneImage {
        let data = (0..width * height * channels)
            .map(|i| ((i * 7) % 256) as u8)
            .collect();
        PlaneImage::from_data(width, height, channels, DEPTH_8, data).unwrap()
    }

    fn sample(image: &PlaneImage, x: usize, y: usize, channel: usize) -> u8 {
        image.data[(y * image.width + x) * image.channels + channel]
    }

    fn read(image: &PlaneImage, rect: Rect, lo: usize, hi: usize, padding: i16) -> (i16, Vec<u8>) {
        let width = usize::try_from(rect.width().max(0)).unwrap();
        let height = usize::try_from(rect.height().max(0)).unwrap();
        let row_bytes = width * (hi - lo + 1) * image.bytes_per_sample();
        let mut buffer = vec![0xEE; row_bytes * height];
        let error = image.read_rect(rect, lo, hi, &mut buffer, row_bytes, padding, 0);
        (error, buffer)
    }

    fn u16s(values: &[u16]) -> Vec<u8> {
        values.iter().flat_map(|v| v.to_ne_bytes()).collect()
    }

    fn values16(data: &[u8]) -> Vec<u16> {
        data.chunks_exact(2)
            .map(|pair| u16::from_ne_bytes([pair[0], pair[1]]))
            .collect()
    }

    #[test]
    fn constructors_validate_depth_and_size() {
        assert!(matches!(
            PlaneImage::new(2, 2, 3, 12),
            Err(ImageError::BadDepth(12))
        ));
        assert!(matches!(
            PlaneImage::from_data(2, 2, 1, 8, vec![0; 3]),
            Err(ImageError::BadSize {
                actual: 3,
                expected: 4
            })
        ));
        assert!(matches!(
            PlaneImage::new(usize::MAX, 2, 1, 8),
            Err(ImageError::TooLarge { .. })
        ));
        assert!(matches!(
            PlaneImage::new(1 << 40, 1 << 40, 4, 16),
            Err(ImageError::TooLarge { .. })
        ));
        let image = PlaneImage::new(3, 2, 4, 16).unwrap();
        assert_eq!(image.data.len(), 48);
        assert_eq!(
            (
                image.bytes_per_sample(),
                image.pixel_bytes(),
                image.row_bytes()
            ),
            (2, 8, 24)
        );
        assert!(PlaneImage::new(0, 0, 3, 8).unwrap().data.is_empty());
    }

    #[test]
    fn full_rect_all_planes_is_a_plain_copy() {
        let image = numbered(4, 3, 3);
        let (error, data) = read(&image, Rect::new(0, 0, 3, 4), 0, 2, EDGE);
        assert_eq!(error, c::NO_ERR);
        assert_eq!(data, image.data);
    }

    #[test]
    fn single_plane_extraction() {
        let image = numbered(4, 3, 3);
        let (error, data) = read(&image, Rect::new(1, 1, 3, 3), 1, 1, EDGE);
        assert_eq!(error, c::NO_ERR);
        let expected: Vec<u8> = [(1, 1), (2, 1), (1, 2), (2, 2)]
            .iter()
            .map(|&(x, y)| sample(&image, x, y, 1))
            .collect();
        assert_eq!(data, expected);
    }

    #[test]
    fn sixteen_bit_plane_extraction() {
        let values: Vec<u16> = (0..24).map(|i| i * 1000).collect();
        let image = PlaneImage::from_data(4, 2, 3, DEPTH_16, u16s(&values)).unwrap();
        let (error, raw) = read(&image, Rect::new(0, 0, 2, 4), 2, 2, EDGE);
        assert_eq!(error, c::NO_ERR);
        let expected: Vec<u16> = values.iter().skip(2).step_by(3).copied().collect();
        assert_eq!(values16(&raw), expected);
    }

    #[test]
    fn out_of_bounds_padding_modes() {
        let image = numbered(4, 3, 3);
        let first = sample(&image, 0, 0, 0);
        let rect = Rect::new(-1, 0, 2, 4);
        assert_eq!(
            read(&image, rect, 0, 2, c::PADDING_ERROR_ON_BOUNDS).0,
            c::PARAM_ERR
        );
        let (error, data) = read(&image, Rect::new(0, -2, 1, 1), 0, 0, 99);
        assert_eq!(error, c::NO_ERR);
        assert_eq!(data, vec![99, 99, first]);
        let (error, data) = read(&image, Rect::new(-1, -1, 1, 1), 0, 0, EDGE);
        assert_eq!(error, c::NO_ERR);
        assert_eq!(data, vec![first; 4]);
        let (error, data) = read(&image, Rect::new(0, 3, 1, 6), 0, 0, c::PADDING_NONE);
        assert_eq!(error, c::NO_ERR);
        assert_eq!(data, vec![sample(&image, 3, 0, 0), 0, 0]);
    }

    #[test]
    fn rows_outside_the_image_use_the_padding_mode() {
        let image = numbered(2, 2, 1);
        let rect = Rect::new(1, 0, 4, 2);
        let (_, data) = read(&image, rect, 0, 0, EDGE);
        let last_row = [sample(&image, 0, 1, 0), sample(&image, 1, 1, 0)];
        assert_eq!(data, last_row.repeat(3));
        let (_, data) = read(&image, rect, 0, 0, 7);
        assert_eq!(&data[2..], &[7, 7, 7, 7]);
        let (_, data) = read(&image, rect, 0, 0, c::PADDING_NONE);
        assert_eq!(&data[2..], &[0, 0, 0, 0]);
    }

    #[test]
    fn edge_replication_right_of_and_entirely_outside_the_image() {
        let image = numbered(3, 1, 2);
        let (_, data) = read(&image, Rect::new(0, 2, 1, 5), 1, 1, EDGE);
        assert_eq!(data, vec![sample(&image, 2, 0, 1); 3]);
        let (_, data) = read(&image, Rect::new(0, 5, 1, 7), 0, 0, EDGE);
        assert_eq!(data, vec![sample(&image, 2, 0, 0); 2]);
        let (_, data) = read(&image, Rect::new(0, -4, 1, -2), 0, 1, EDGE);
        assert_eq!(
            data,
            [sample(&image, 0, 0, 0), sample(&image, 0, 0, 1)].repeat(2)
        );
        let (_, data) = read(&image, Rect::new(0, 5, 1, 7), 0, 0, 12);
        assert_eq!(data, vec![12, 12]);
    }

    #[test]
    fn sixteen_bit_edge_replication() {
        let image = PlaneImage::from_data(2, 1, 1, DEPTH_16, u16s(&[300, 32768])).unwrap();
        let (_, raw) = read(&image, Rect::new(0, -1, 1, 3), 0, 0, EDGE);
        assert_eq!(values16(&raw), vec![300, 300, 32768, 32768]);
    }

    #[test]
    fn invalid_padding_values_fail_only_out_of_bounds() {
        let image = numbered(2, 2, 1);
        let outside = Rect::new(0, -1, 1, 1);
        assert_eq!(read(&image, outside, 0, 0, 256).0, c::PARAM_ERR);
        assert_eq!(read(&image, outside, 0, 0, -4).0, c::PARAM_ERR);
        assert_eq!(read(&image, outside, 0, 0, 255).0, c::NO_ERR);
        assert_eq!(read(&image, outside, 0, 0, 0).0, c::NO_ERR);
        assert_eq!(read(&image, Rect::new(0, 0, 2, 2), 0, 0, -4).0, c::NO_ERR);
        assert_eq!(
            read(
                &image,
                Rect::new(0, 0, 2, 2),
                0,
                0,
                c::PADDING_ERROR_ON_BOUNDS
            )
            .0,
            c::NO_ERR
        );
    }

    #[test]
    fn planes_beyond_the_image_get_the_dummy_value() {
        let image = numbered(4, 3, 3);
        let (error, data) = read(&image, Rect::new(0, 0, 1, 1), 2, 3, EDGE);
        assert_eq!(error, c::NO_ERR);
        assert_eq!(data, vec![sample(&image, 0, 0, 2), 0]);
        let mut buffer = [0u8; 2];
        let error = image.read_rect(Rect::new(0, 0, 1, 1), 2, 3, &mut buffer, 2, EDGE, 255);
        assert_eq!(error, c::NO_ERR);
        assert_eq!(buffer, [sample(&image, 0, 0, 2), 255]);
        let mut buffer = [0u8; 2];
        image.read_rect(Rect::new(0, 0, 1, 1), 7, 8, &mut buffer, 2, EDGE, 9);
        assert_eq!(buffer, [9, 9]);
        let wide = PlaneImage::new(1, 1, 1, DEPTH_16).unwrap();
        let mut buffer = [0u8; 4];
        wide.read_rect(Rect::new(0, 0, 1, 1), 0, 1, &mut buffer, 4, EDGE, 32768);
        assert_eq!(values16(&buffer), vec![0, 32768]);
    }

    #[test]
    fn invalid_requests_are_rejected() {
        let image = numbered(4, 3, 3);
        let mut buffer = vec![0u8; 64];
        let rect = Rect::new(0, 0, 2, 2);
        assert_eq!(
            image.read_rect(rect, 2, 1, &mut buffer, 8, EDGE, 0),
            c::PARAM_ERR
        );
        assert_eq!(
            image.read_rect(rect, 0, usize::MAX, &mut buffer, 8, EDGE, 0),
            c::PARAM_ERR
        );
        let mut small = vec![0u8; 11];
        assert_eq!(
            image.read_rect(rect, 0, 2, &mut small, 6, EDGE, 0),
            c::PARAM_ERR
        );
        assert_eq!(
            image.read_rect(rect, 0, 2, &mut buffer, 5, EDGE, 0),
            c::PARAM_ERR
        );
        assert_eq!(
            image.read_rect(rect, 0, 2, &mut buffer, usize::MAX, EDGE, 0),
            c::PARAM_ERR
        );
        let huge = Rect::new(0, i32::MIN, 1, i32::MAX);
        assert_eq!(
            image.read_rect(huge, 0, 2, &mut buffer, 8, EDGE, 0),
            c::PARAM_ERR
        );
        // A padded destination with spare bytes per row is fine.
        let mut padded = vec![0u8; 8 + 6];
        assert_eq!(
            image.read_rect(rect, 0, 2, &mut padded, 8, EDGE, 0),
            c::NO_ERR
        );
        assert_eq!(&padded[..6], &image.data[..6]);
        assert_eq!(&padded[8..], &image.data[12..18]);
    }

    #[test]
    fn empty_rects_copy_nothing() {
        let image = numbered(4, 3, 3);
        let mut buffer: Vec<u8> = Vec::new();
        for rect in [
            Rect::new(1, 1, 1, 3),
            Rect::new(1, 3, 2, 1),
            Rect::new(2, 0, 0, 4),
        ] {
            assert_eq!(
                image.read_rect(rect, 0, 2, &mut buffer, 0, EDGE, 0),
                c::NO_ERR
            );
        }
        // Bounds are still checked first, as Photoshop does.
        let rect = Rect::new(-1, 0, -1, 2);
        assert_eq!(
            image.read_rect(rect, 0, 2, &mut buffer, 0, c::PADDING_ERROR_ON_BOUNDS, 0),
            c::PARAM_ERR
        );
    }

    #[test]
    fn one_pixel_and_empty_images() {
        let image = PlaneImage::from_data(1, 1, 1, DEPTH_8, vec![42]).unwrap();
        let (_, data) = read(&image, Rect::new(-1, -1, 2, 2), 0, 0, EDGE);
        assert_eq!(data, vec![42; 9]);
        let empty = PlaneImage::new(0, 0, 1, DEPTH_8).unwrap();
        let (error, data) = read(&empty, Rect::new(0, 0, 1, 2), 0, 0, EDGE);
        assert_eq!(error, c::NO_ERR);
        assert_eq!(data, vec![0, 0]);
        let (_, data) = read(&empty, Rect::new(0, 0, 1, 2), 0, 0, 5);
        assert_eq!(data, vec![5, 5]);
        assert_eq!(empty.scaled(4), empty);
    }

    #[test]
    fn extract_row_handles_bad_arguments() {
        let image = numbered(3, 2, 2);
        assert_eq!(image.extract_row(0, 0, 3, 1, 0, EDGE, 0), Vec::<u8>::new());
        assert_eq!(image.extract_row(0, 3, 0, 0, 0, EDGE, 0), Vec::<u8>::new());
        assert_eq!(image.extract_row(9, 0, 2, 0, 0, 4, 0), vec![4, 4]);
        assert_eq!(image.extract_row(9, 0, 2, 0, 0, EDGE, 0), vec![0, 0]);
        let row = image.extract_row(1, -1, 4, 0, 1, EDGE, 0);
        let pixels: Vec<u8> = image.data[6..12].to_vec();
        let mut expected = pixels[..2].to_vec();
        expected.extend_from_slice(&pixels);
        expected.extend_from_slice(&pixels[4..]);
        assert_eq!(row, expected);
    }

    #[test]
    fn write_rect_clips_and_selects_planes() {
        let mut image = numbered(4, 3, 3);
        let payload = [200, 201, 202, 203, 204, 205];
        // Two rows, three columns starting one pixel left of the image, plane 1.
        image.write_rect(Rect::new(1, -1, 3, 2), 1, 1, &payload, 3);
        assert_eq!(sample(&image, 0, 1, 1), 201);
        assert_eq!(sample(&image, 1, 1, 1), 202);
        assert_eq!(sample(&image, 0, 2, 1), 204);
        assert_eq!(sample(&image, 1, 2, 1), 205);
        assert_eq!(sample(&image, 0, 1, 0), numbered(4, 3, 3).data[3 * 4]);
    }

    #[test]
    fn write_rect_full_planes_and_rows_above_the_image() {
        let mut image = numbered(2, 2, 2);
        let payload: Vec<u8> = (100..108).collect();
        // Row 0 of the payload is above the image and is skipped.
        assert_eq!(
            image.try_write_rect(Rect::new(-1, 0, 1, 2), 0, 1, &payload, 4),
            c::NO_ERR
        );
        assert_eq!(&image.data[..4], &[104, 105, 106, 107]);
    }

    #[test]
    fn write_rect_skips_planes_beyond_the_image_and_16_bit() {
        let mut image = PlaneImage::new(2, 1, 1, DEPTH_16).unwrap();
        let payload = u16s(&[1, 2, 3, 4]);
        assert_eq!(
            image.try_write_rect(Rect::new(0, 0, 1, 2), 0, 1, &payload, 8),
            c::NO_ERR
        );
        assert_eq!(values16(&image.data), vec![1, 3]);
    }

    #[test]
    fn write_rect_rejects_short_sources() {
        let mut image = numbered(4, 3, 3);
        let before = image.clone();
        assert_eq!(
            image.try_write_rect(Rect::new(0, 0, 2, 2), 0, 2, &[0; 11], 6),
            c::PARAM_ERR
        );
        assert_eq!(
            image.try_write_rect(Rect::new(0, 0, 2, 2), 2, 0, &[0; 64], 6),
            c::PARAM_ERR
        );
        image.write_rect(Rect::new(0, 0, 2, 2), 0, 2, &[0; 5], 6);
        assert_eq!(image, before);
        assert_eq!(
            image.try_write_rect(Rect::new(5, 5, 9, 9), 0, 2, &[], 0),
            c::NO_ERR
        );
        assert_eq!(image, before);
    }

    #[test]
    fn scaled_copy_uses_nearest_samples() {
        let image = numbered(6, 4, 3);
        let half = image.scaled(2);
        assert_eq!((half.width, half.height), (3, 2));
        assert_eq!(sample(&half, 2, 1, 0), sample(&image, 4, 2, 0));
        assert_eq!(image.scaled(1), image);
        assert_eq!(image.scaled(0), image);
        let tiny = image.scaled(100);
        assert_eq!((tiny.width, tiny.height), (1, 1));
        assert_eq!(tiny.data, image.data[..3]);
    }

    #[test]
    fn planes_round_trip() {
        let mut image = numbered(4, 3, 3);
        let plane = image.plane(1);
        let inverted: Vec<u8> = plane.iter().map(|v| 255 - v).collect();
        image.set_plane(1, &inverted);
        assert_eq!(image.plane(1), inverted);
        assert!(image.plane(3).is_empty());
        let before = image.clone();
        image.set_plane(3, &inverted);
        assert_eq!(image, before);
    }

    #[test]
    fn sixteen_bit_planes_round_trip() {
        let mut image = PlaneImage::from_data(2, 1, 2, DEPTH_16, u16s(&[1, 2, 3, 4])).unwrap();
        assert_eq!(values16(&image.plane(1)), vec![2, 4]);
        image.set_plane(0, &u16s(&[9, 8]));
        assert_eq!(values16(&image.data), vec![9, 2, 8, 4]);
    }

    #[test]
    fn rect_helpers() {
        assert!(Rect::new(0, 0, 0, 5).is_empty());
        assert!(Rect::new(0, 3, 2, 1).is_empty());
        assert!(!Rect::new(0, 0, 1, 1).is_empty());
        assert_eq!(Rect::new(1, 2, 4, 7).width(), 5);
        assert_eq!(Rect::new(1, 2, 4, 7).height(), 3);
        assert_eq!(scale_factor(c::FIXED_ONE), 1);
        assert_eq!(scale_factor(3 * c::FIXED_ONE), 3);
        assert_eq!(scale_factor(c::FIXED_ONE / 2 * 5), 3);
        assert_eq!(scale_factor(0), 1);
        assert_eq!(scale_factor(-5), 1);
        assert_eq!(scale_factor(1), 1);
        assert_eq!(scale_factor(i32::MAX), 32768);
    }
}
