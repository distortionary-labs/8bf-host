//! Sample range and depth conversions.
//!
//! Most image formats use the full 0..=65535 range for 16-bit channels,
//! while Photoshop filters expect 0..=32768. These helpers convert whole
//! buffers of native-endian `u16` samples, and convert between 8-bit and
//! Photoshop 16-bit samples.
//!
//! Buffers of 16-bit samples should have an even length. A trailing odd
//! byte is not a sample: it is copied unchanged by the range conversions
//! and dropped by [`ps16_to_ps8`].

/// The largest sample in Photoshop's 16-bit range.
const PS_MAX: u16 = super::constants::PS_16BIT_MAX;
/// Photoshop's 16-bit midpoint; values above it round down by one level.
const PS_HALF: u16 = PS_MAX / 2;
/// The largest 8-bit sample.
const U8_MAX: u16 = u8::MAX as u16;
/// Shift that reduces a Photoshop 16-bit sample to 8 bits.
const PS16_TO_8_SHIFT: u32 = 7;

/// Applies `convert` to every native-endian `u16` sample of `data`.
fn map_samples(data: &[u8], convert: impl Fn(u16) -> u16) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len());
    let samples = data.chunks_exact(2);
    let tail = samples.remainder();
    for pair in samples {
        let value = convert(u16::from_ne_bytes([pair[0], pair[1]]));
        out.extend_from_slice(&value.to_ne_bytes());
    }
    out.extend_from_slice(tail);
    out
}

/// Reads sample `index` of a 16-bit buffer, if it has one.
fn sample_at(data: &[u8], index: usize) -> Option<u16> {
    let start = index.checked_mul(2)?;
    let pair = data.get(start..start.checked_add(2)?)?;
    Some(u16::from_ne_bytes([pair[0], pair[1]]))
}

/// Maps full-range 0..=65535 samples to Photoshop's 0..=32768: `(v + 1) >> 1`.
///
/// # Examples
///
/// ```
/// use eightbf_host::host::pixels::u16_full_to_ps;
/// let data: Vec<u8> = [0u16, 65535].iter().flat_map(|v| v.to_ne_bytes()).collect();
/// let out = u16_full_to_ps(&data);
/// assert_eq!(u16::from_ne_bytes([out[2], out[3]]), 32768);
/// ```
pub fn u16_full_to_ps(data: &[u8]) -> Vec<u8> {
    // Computed in u32 so 65535 + 1 cannot overflow; the result is <= 32768.
    map_samples(data, |v| ((u32::from(v) + 1) >> 1) as u16)
}

/// Maps Photoshop's 0..=32768 samples back to 0..=65535.
///
/// Computes `round(v * 65535 / 32768)`, which for `v < 32768` equals
/// `2v - (1 if v > 16384 else 0)`. Values at or above 32768, including
/// out-of-range output from careless filters, become 65535.
pub fn u16_ps_to_full(data: &[u8]) -> Vec<u8> {
    map_samples(data, |v| {
        if v >= PS_MAX {
            return u16::MAX;
        }
        2 * v - u16::from(v > PS_HALF)
    })
}

/// Reduces Photoshop 16-bit samples (0..=32768) to 8-bit: `min(255, v >> 7)`.
pub fn ps16_to_ps8(data: &[u8]) -> Vec<u8> {
    data.chunks_exact(2)
        .map(|pair| {
            let value = u16::from_ne_bytes([pair[0], pair[1]]) >> PS16_TO_8_SHIFT;
            // `min` keeps the value within u8.
            value.min(U8_MAX) as u8
        })
        .collect()
}

/// Expands 8-bit samples to Photoshop's 16-bit range: `(v * 32768 + 127) / 255`.
///
/// 0 maps to 0 and 255 to 32768.
pub fn ps8_to_ps16(data: &[u8]) -> Vec<u8> {
    let half_step = u32::from(U8_MAX / 2);
    data.iter()
        .flat_map(|&v| {
            let value = (u32::from(v) * u32::from(PS_MAX) + half_step) / u32::from(U8_MAX);
            // The largest result is 32768, which fits in u16.
            (value as u16).to_ne_bytes()
        })
        .collect()
}

/// Returns `after` with every sample equal in `before` and `after` taken
/// from `original` instead.
///
/// 16-bit data cannot round-trip exactly through Photoshop's 0..=32768
/// range, so pixels a filter did not touch would otherwise drift by one
/// level. All three buffers hold 16-bit samples in the same layout;
/// `before` is `original` after the round trip. The result has the length
/// of `after`. Samples missing from a shorter `before` count as changed,
/// and unchanged samples missing from a shorter `original` keep their
/// `after` value.
pub fn keep_unchanged16(original: &[u8], before: &[u8], after: &[u8]) -> Vec<u8> {
    let mut out = after.to_vec();
    for (index, pair) in out.chunks_exact_mut(2).enumerate() {
        let current = u16::from_ne_bytes([pair[0], pair[1]]);
        if sample_at(before, index) != Some(current) {
            continue;
        }
        if let Some(value) = sample_at(original, index) {
            pair.copy_from_slice(&value.to_ne_bytes());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn u16s(values: &[u32]) -> Vec<u8> {
        values
            .iter()
            .flat_map(|&v| u16::try_from(v).unwrap().to_ne_bytes())
            .collect()
    }

    fn values16(data: &[u8]) -> Vec<u32> {
        data.chunks_exact(2)
            .map(|pair| u32::from(u16::from_ne_bytes([pair[0], pair[1]])))
            .collect()
    }

    #[test]
    fn full_to_ps_range() {
        let mut samples: Vec<u32> = (0..65536).step_by(257).collect();
        samples.extend([65535, 1, 2]);
        let converted = values16(&u16_full_to_ps(&u16s(&samples)));
        let expected: Vec<u32> = samples.iter().map(|v| (v + 1) >> 1).collect();
        assert_eq!(converted, expected);
        assert_eq!(
            values16(&u16_full_to_ps(&u16s(&[0, 65535]))),
            vec![0, 32768]
        );
    }

    #[test]
    fn ps_to_full_range_and_saturation() {
        let samples = [0, 1, 16384, 32767, 32768, 40000, 65535];
        let converted = values16(&u16_ps_to_full(&u16s(&samples)));
        assert_eq!(converted[4..], [65535, 65535, 65535]);
        for (ps, full) in samples[..4].iter().zip(&converted[..4]) {
            assert_eq!(*full, (ps * 65535 + 16384) / 32768);
        }
        let every: Vec<u32> = (0..=32768).collect();
        let exact: Vec<u32> = every.iter().map(|v| (v * 65535 + 16384) / 32768).collect();
        assert_eq!(values16(&u16_ps_to_full(&u16s(&every))), exact);
    }

    #[test]
    fn round_trip_is_close() {
        let samples: Vec<u32> = (0..65536).step_by(97).collect();
        let back = values16(&u16_ps_to_full(&u16_full_to_ps(&u16s(&samples))));
        let worst = samples.iter().zip(&back).map(|(a, b)| a.abs_diff(*b)).max();
        assert!(worst.unwrap() <= 2);
    }

    #[test]
    fn empty_and_odd_length_buffers() {
        assert!(u16_full_to_ps(&[]).is_empty());
        assert!(u16_ps_to_full(&[]).is_empty());
        assert!(ps16_to_ps8(&[]).is_empty());
        assert!(ps8_to_ps16(&[]).is_empty());
        assert!(keep_unchanged16(&[], &[], &[]).is_empty());
        let mut odd = u16s(&[65535]);
        odd.push(9);
        let out = u16_full_to_ps(&odd);
        assert_eq!(values16(&out), vec![32768]);
        assert_eq!(out[2], 9);
        assert_eq!(ps16_to_ps8(&odd), vec![255]);
    }

    #[test]
    fn eight_sixteen_conversions() {
        let eight: Vec<u8> = (0..=255).collect();
        let sixteen = ps8_to_ps16(&eight);
        assert_eq!(values16(&sixteen)[255], 32768);
        assert_eq!(values16(&sixteen)[0], 0);
        assert_eq!(values16(&sixteen)[128], (128 * 32768 + 127) / 255);
        assert_eq!(ps16_to_ps8(&sixteen), eight);
        assert_eq!(
            ps16_to_ps8(&u16s(&[32768, 40000, 0, 127, 128])),
            vec![255, 255, 0, 0, 1]
        );
    }

    #[test]
    fn keep_unchanged16_restores_untouched_samples() {
        let original = u16s(&[1001, 3, 65535, 40001]);
        let sent = u16_full_to_ps(&original);
        let round_trip = u16_ps_to_full(&sent);
        assert_ne!(round_trip, original, "odd values drift by one level");
        let mut edited = sent.clone();
        edited[2..4].copy_from_slice(&100u16.to_ne_bytes());
        let after = u16_ps_to_full(&edited);
        let merged = values16(&keep_unchanged16(&original, &round_trip, &after));
        assert_eq!(merged, vec![1001, 200, 65535, 40001]);
    }

    #[test]
    fn keep_unchanged16_with_short_inputs() {
        let after = u16s(&[5, 6, 7]);
        let merged = keep_unchanged16(&u16s(&[1]), &u16s(&[5, 6]), &after);
        assert_eq!(values16(&merged), vec![1, 6, 7]);
    }
}
