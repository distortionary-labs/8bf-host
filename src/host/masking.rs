//! Selection masking applied after a filter runs (Photoshop's autoMask).
//!
//! A mask is an 8-bit, one-channel [`PlaneImage`] the size of the image,
//! where 255 means fully selected and 0 means not selected. Partially
//! selected pixels (anti-aliased or feathered edges) are blended between
//! the original and filtered values.

use super::image::{PlaneImage, Rect, DEPTH_8};

/// The mask value of a fully selected pixel.
const FULLY_SELECTED: u8 = u8::MAX;
/// The divisor of mask weights.
const WEIGHT_SCALE: i64 = FULLY_SELECTED as i64;
/// Added before dividing by [`WEIGHT_SCALE`] to round to nearest.
const WEIGHT_ROUNDING: i64 = WEIGHT_SCALE / 2;

/// Why [`try_apply_mask`] refused its inputs.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum MaskError {
    #[error("the original and filtered images differ in size or format")]
    ImageMismatch,
    #[error("the mask must be an 8-bit, one-channel image the size of the image")]
    BadMask,
}

/// Blends `filtered` back towards `original` where `mask` is below 255.
///
/// Only `rect` (the whole image when `None`) is processed; pixels outside
/// it are restored from `original`. `filtered` is modified in place.
/// Inputs that [`try_apply_mask`] rejects leave `filtered` unchanged.
pub fn apply_mask(
    original: &PlaneImage,
    filtered: &mut PlaneImage,
    mask: &PlaneImage,
    rect: Option<Rect>,
) {
    // Mismatched inputs are a host bug, not a filter error; leaving the
    // filtered image as it is matches the documented behaviour.
    let _ = try_apply_mask(original, filtered, mask, rect);
}

/// Like [`apply_mask`], but reports inputs it cannot process.
///
/// Each partially selected sample becomes
/// `before + ((after - before) * weight + 127) / 255`, with the division
/// rounding towards negative infinity, at 8 and 16 bits alike.
///
/// # Errors
///
/// [`MaskError::ImageMismatch`] when `original` and `filtered` differ in
/// dimensions, channels, depth or data length, and
/// [`MaskError::BadMask`] when `mask` is not an 8-bit, one-channel image
/// of the same dimensions.
pub fn try_apply_mask(
    original: &PlaneImage,
    filtered: &mut PlaneImage,
    mask: &PlaneImage,
    rect: Option<Rect>,
) -> Result<(), MaskError> {
    let same_shape = original.width == filtered.width
        && original.height == filtered.height
        && original.channels == filtered.channels
        && original.depth == filtered.depth
        && original.data.len() == filtered.data.len()
        && original.data.len() == original.height.saturating_mul(original.row_bytes());
    if !same_shape {
        return Err(MaskError::ImageMismatch);
    }
    let mask_ok = mask.width == original.width
        && mask.height == original.height
        && mask.channels == 1
        && mask.depth == DEPTH_8
        && mask.data.len() == mask.width.saturating_mul(mask.height);
    if !mask_ok {
        return Err(MaskError::BadMask);
    }
    let (width, height) = (original.width, original.height);
    let rect = rect.unwrap_or(Rect::new(0, 0, i32::MAX, i32::MAX));
    let clamp = |value: i32, limit: usize| usize::try_from(value.max(0)).unwrap_or(0).min(limit);
    let (top, bottom) = (clamp(rect.top, height), clamp(rect.bottom, height));
    let (left, right) = (clamp(rect.left, width), clamp(rect.right, width));
    let pixel = original.pixel_bytes();
    let row_bytes = original.row_bytes();
    let sixteen = original.bytes_per_sample() == 2;

    for y in 0..height {
        let row = y * row_bytes..(y + 1) * row_bytes;
        let before_row = &original.data[row.clone()];
        let after_row = &mut filtered.data[row];
        if y < top || y >= bottom || left >= right {
            after_row.copy_from_slice(before_row);
            continue;
        }
        after_row[..left * pixel].copy_from_slice(&before_row[..left * pixel]);
        after_row[right * pixel..].copy_from_slice(&before_row[right * pixel..]);
        let weights = &mask.data[y * width + left..y * width + right];
        for (offset, &weight) in weights.iter().enumerate() {
            if weight == FULLY_SELECTED {
                continue;
            }
            let span = (left + offset) * pixel..(left + offset + 1) * pixel;
            let before = &before_row[span.clone()];
            let after = &mut after_row[span];
            if weight == 0 {
                after.copy_from_slice(before);
            } else if sixteen {
                for (out, old) in after.chunks_exact_mut(2).zip(before.chunks_exact(2)) {
                    let old_value = u16::from_ne_bytes([old[0], old[1]]);
                    let new_value = u16::from_ne_bytes([out[0], out[1]]);
                    let value = blend(i64::from(old_value), i64::from(new_value), weight);
                    let value =
                        u16::try_from(value.clamp(0, i64::from(u16::MAX))).unwrap_or(u16::MAX);
                    out.copy_from_slice(&value.to_ne_bytes());
                }
            } else {
                for (out, &old) in after.iter_mut().zip(before) {
                    let value = blend(i64::from(old), i64::from(*out), weight);
                    *out = u8::try_from(value.clamp(0, i64::from(u8::MAX))).unwrap_or(u8::MAX);
                }
            }
        }
    }
    Ok(())
}

/// `before + floor(((after - before) * weight + 127) / 255)`.
fn blend(before: i64, after: i64, weight: u8) -> i64 {
    before + ((after - before) * i64::from(weight) + WEIGHT_ROUNDING).div_euclid(WEIGHT_SCALE)
}

/// Returns the bounding rect of the non-zero pixels of `mask`.
///
/// A pixel counts as non-zero when any of its bytes is non-zero, so the
/// function works for masks of any channel count and depth. Returns an
/// all-zero rect when the mask is empty.
pub fn mask_bounds(mask: &PlaneImage) -> Rect {
    let pixel = mask.pixel_bytes();
    let row_bytes = mask.row_bytes();
    if pixel == 0 || row_bytes == 0 {
        return Rect::default();
    }
    let mut bounds: Option<(usize, usize, usize, usize)> = None;
    for (y, row) in mask
        .data
        .chunks_exact(row_bytes)
        .take(mask.height)
        .enumerate()
    {
        let selected = |pixel_bytes: &[u8]| pixel_bytes.iter().any(|&b| b != 0);
        let mut pixels = row.chunks_exact(pixel);
        let Some(first) = pixels.position(selected) else {
            continue;
        };
        let last = row.chunks_exact(pixel).rposition(selected).unwrap_or(first);
        bounds = Some(match bounds {
            None => (y, first, y + 1, last + 1),
            Some((top, left, _, right)) => (top, left.min(first), y + 1, right.max(last + 1)),
        });
    }
    let Some((top, left, bottom, right)) = bounds else {
        return Rect::default();
    };
    let edge = |value: usize| i32::try_from(value).unwrap_or(i32::MAX);
    Rect::new(edge(top), edge(left), edge(bottom), edge(right))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host::image::DEPTH_16;

    fn gray(width: usize, data: &[u8]) -> PlaneImage {
        PlaneImage::from_data(width, data.len() / width, 1, DEPTH_8, data.to_vec()).unwrap()
    }

    #[test]
    fn mask_bounds_finds_selected_pixels() {
        let mut mask = PlaneImage::new(5, 4, 1, DEPTH_8).unwrap();
        assert_eq!(mask_bounds(&mask), Rect::default());
        mask.data[5 + 2] = 1;
        mask.data[2 * 5 + 3] = 255;
        assert_eq!(mask_bounds(&mask), Rect::new(1, 2, 3, 4));
        mask.data[3 * 5] = 9;
        assert_eq!(mask_bounds(&mask), Rect::new(1, 0, 4, 4));
    }

    #[test]
    fn mask_bounds_of_empty_and_wide_masks() {
        assert_eq!(
            mask_bounds(&PlaneImage::new(0, 0, 1, DEPTH_8).unwrap()),
            Rect::default()
        );
        let mut wide = PlaneImage::new(3, 1, 1, DEPTH_16).unwrap();
        wide.data[5] = 1;
        assert_eq!(mask_bounds(&wide), Rect::new(0, 2, 1, 3));
        assert_eq!(mask_bounds(&gray(1, &[7])), Rect::new(0, 0, 1, 1));
    }

    #[test]
    fn apply_mask_blends_and_restores_outside_the_rect() {
        let original = gray(4, &[0, 0, 0, 0]);
        let mut filtered = gray(4, &[255, 255, 255, 255]);
        let mask = gray(4, &[255, 0, 128, 255]);
        apply_mask(&original, &mut filtered, &mask, Some(Rect::new(0, 0, 1, 3)));
        assert_eq!(filtered.data, vec![255, 0, 128, 0]);
    }

    #[test]
    fn partial_weights_round_like_photoshop_in_both_directions() {
        let original = gray(3, &[200, 10, 100]);
        let mut filtered = gray(3, &[10, 200, 101]);
        let mask = gray(3, &[1, 1, 128]);
        apply_mask(&original, &mut filtered, &mask, None);
        // 200 + floor((-190 + 127) / 255) = 199; 10 + floor((190 + 127) / 255) = 11.
        assert_eq!(filtered.data, vec![199, 11, 101]);
    }

    #[test]
    fn sixteen_bit_blending() {
        let to_bytes =
            |values: &[u16]| -> Vec<u8> { values.iter().flat_map(|v| v.to_ne_bytes()).collect() };
        let original = PlaneImage::from_data(2, 1, 1, DEPTH_16, to_bytes(&[0, 32768])).unwrap();
        let mut filtered = PlaneImage::from_data(2, 1, 1, DEPTH_16, to_bytes(&[32768, 0])).unwrap();
        let mask = gray(2, &[128, 64]);
        apply_mask(&original, &mut filtered, &mask, None);
        let expected_first = (32768i64 * 128 + 127).div_euclid(255);
        let expected_second = 32768 + (-32768i64 * 64 + 127).div_euclid(255);
        let expected = [expected_first, expected_second].map(|v| u16::try_from(v).unwrap());
        assert_eq!(filtered.data, to_bytes(&expected));
    }

    #[test]
    fn rects_outside_the_image_restore_everything() {
        let original = gray(2, &[1, 2, 3, 4]);
        let mask = gray(2, &[255; 4]);
        let mut filtered = gray(2, &[9; 4]);
        apply_mask(&original, &mut filtered, &mask, Some(Rect::new(5, 5, 9, 9)));
        assert_eq!(filtered.data, original.data);
        let mut filtered = gray(2, &[9; 4]);
        apply_mask(
            &original,
            &mut filtered,
            &mask,
            Some(Rect::new(-3, -3, 1, 9)),
        );
        assert_eq!(filtered.data, vec![9, 9, 3, 4]);
    }

    #[test]
    fn mismatched_inputs_are_rejected() {
        let original = gray(2, &[1, 2]);
        let mut filtered = gray(2, &[9, 9]);
        let wrong_size = gray(1, &[255]);
        assert_eq!(
            try_apply_mask(&original, &mut filtered, &wrong_size, None),
            Err(MaskError::BadMask)
        );
        let mut other = gray(1, &[9, 9]);
        assert_eq!(
            try_apply_mask(&original, &mut other, &gray(2, &[0, 0]), None),
            Err(MaskError::ImageMismatch)
        );
        apply_mask(&original, &mut filtered, &wrong_size, None);
        assert_eq!(filtered.data, vec![9, 9]);
    }
}
