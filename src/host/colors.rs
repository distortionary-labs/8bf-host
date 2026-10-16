//! Color space conversions for the color services callback.
//!
//! Component ranges follow the SDK: RGB, CMYK, gray and XYZ use 0..=255;
//! HSB and HSL use 0..=359 for hue and 0..=255 for the rest; Lab uses
//! 0..=255 for L and -128..=127 for a and b. Components are stored in
//! Photoshop's order: HSB is hue, saturation, brightness and HSL is hue,
//! saturation, lightness. Conversions go through sRGB (D65), as Photoshop
//! does for filters, so they are approximate for CMYK.
//!
//! Rounding uses round-half-to-even, matching the original host.

use super::constants as c;

/// The largest 8-bit component.
const MAX: f64 = 255.0;
/// Degrees in a hue circle.
const HUE_DEGREES: f64 = 360.0;
/// The largest hue component.
const MAX_HUE: f64 = 359.0;
/// The Lab a and b component range.
const LAB_AB_MIN: f64 = -128.0;
const LAB_AB_MAX: f64 = 127.0;
/// The Lab L component range in CIE units.
const LAB_L_SCALE: f64 = 100.0;
/// D65 reference white.
const WHITE: [f64; 3] = [0.95047, 1.0, 1.08883];
/// CIE constants for the Lab transfer function.
const LAB_EPSILON: f64 = 216.0 / 24389.0;
const LAB_KAPPA: f64 = 24389.0 / 27.0;
/// Rec. 601 luma weights for gray.
const LUMA: [f64; 3] = [0.299, 0.587, 0.114];

/// A color space number Photoshop sent that this host does not convert.
#[derive(Debug, thiserror::Error)]
#[error("unsupported color space {0}")]
pub struct UnsupportedSpace(pub i16);

/// Rounds half to even and clamps into `low..=high`.
fn clamp(value: f64, low: f64, high: f64) -> i16 {
    let rounded = value.round_ties_even();
    if rounded.is_nan() {
        return low as i16;
    }
    // The bounds are within i16, so the cast cannot truncate.
    rounded.clamp(low, high) as i16
}

/// Rounds and clamps into 0..=255.
fn clamp_byte(value: f64) -> i16 {
    clamp(value, 0.0, MAX)
}

fn srgb_to_linear(value: f64) -> f64 {
    let value = value / MAX;
    if value <= 0.04045 {
        value / 12.92
    } else {
        ((value + 0.055) / 1.055).powf(2.4)
    }
}

fn linear_to_srgb(value: f64) -> f64 {
    let value = value.clamp(0.0, 1.0);
    let encoded = if value <= 0.003_130_8 {
        value * 12.92
    } else {
        1.055 * value.powf(1.0 / 2.4) - 0.055
    };
    encoded * MAX
}

fn rgb_to_xyz(rgb: [f64; 3]) -> [f64; 3] {
    let [r, g, b] = rgb.map(srgb_to_linear);
    [
        0.4124 * r + 0.3576 * g + 0.1805 * b,
        0.2126 * r + 0.7152 * g + 0.0722 * b,
        0.0193 * r + 0.1192 * g + 0.9505 * b,
    ]
}

fn xyz_to_rgb(xyz: [f64; 3]) -> [f64; 3] {
    let [x, y, z] = xyz;
    [
        3.2406 * x - 1.5372 * y - 0.4986 * z,
        -0.9689 * x + 1.8758 * y + 0.0415 * z,
        0.0557 * x - 0.2040 * y + 1.0570 * z,
    ]
    .map(linear_to_srgb)
}

fn lab_f(t: f64) -> f64 {
    if t > LAB_EPSILON {
        t.cbrt()
    } else {
        (LAB_KAPPA * t + 16.0) / 116.0
    }
}

fn lab_f_inv(t: f64) -> f64 {
    let cubed = t * t * t;
    if cubed > LAB_EPSILON {
        cubed
    } else {
        (116.0 * t - 16.0) / LAB_KAPPA
    }
}

/// The hue shared by HSV and HLS, in 0..1, for `max != min`.
fn hue(r: f64, g: f64, b: f64, max: f64, range: f64) -> f64 {
    let (rc, gc, bc) = ((max - r) / range, (max - g) / range, (max - b) / range);
    let sixths = if r == max {
        bc - gc
    } else if g == max {
        2.0 + rc - bc
    } else {
        4.0 + gc - rc
    };
    (sixths / 6.0).rem_euclid(1.0)
}

/// Python's `colorsys.rgb_to_hsv` on 0..1 values.
fn rgb_to_hsv(r: f64, g: f64, b: f64) -> [f64; 3] {
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    if min == max {
        return [0.0, 0.0, max];
    }
    let range = max - min;
    [hue(r, g, b, max, range), range / max, max]
}

/// Python's `colorsys.rgb_to_hls` on 0..1 values: hue, lightness, saturation.
fn rgb_to_hls(r: f64, g: f64, b: f64) -> [f64; 3] {
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let lightness = (max + min) / 2.0;
    if min == max {
        return [0.0, lightness, 0.0];
    }
    let range = max - min;
    let saturation = if lightness <= 0.5 {
        range / (max + min)
    } else {
        range / (2.0 - max - min)
    };
    [hue(r, g, b, max, range), lightness, saturation]
}

/// Python's `colorsys.hsv_to_rgb`.
fn hsv_to_rgb(h: f64, s: f64, v: f64) -> [f64; 3] {
    if s == 0.0 {
        return [v; 3];
    }
    let sector = (h * 6.0).trunc();
    let f = h * 6.0 - sector;
    let p = v * (1.0 - s);
    let q = v * (1.0 - s * f);
    let t = v * (1.0 - s * (1.0 - f));
    // `sector` is a small integer; rem_euclid keeps it in 0..6.
    match (sector as i64).rem_euclid(6) {
        0 => [v, t, p],
        1 => [q, v, p],
        2 => [p, v, t],
        3 => [p, q, v],
        4 => [t, p, v],
        _ => [v, p, q],
    }
}

/// Python's `colorsys.hls_to_rgb`.
fn hls_to_rgb(h: f64, l: f64, s: f64) -> [f64; 3] {
    const ONE_THIRD: f64 = 1.0 / 3.0;
    const ONE_SIXTH: f64 = 1.0 / 6.0;
    const TWO_THIRDS: f64 = 2.0 / 3.0;
    if s == 0.0 {
        return [l; 3];
    }
    let m2 = if l <= 0.5 {
        l * (1.0 + s)
    } else {
        l + s - l * s
    };
    let m1 = 2.0 * l - m2;
    let channel = |hue: f64| {
        let hue = hue.rem_euclid(1.0);
        if hue < ONE_SIXTH {
            m1 + (m2 - m1) * hue * 6.0
        } else if hue < 0.5 {
            m2
        } else if hue < TWO_THIRDS {
            m1 + (m2 - m1) * (TWO_THIRDS - hue) * 6.0
        } else {
            m1
        }
    };
    [channel(h + ONE_THIRD), channel(h), channel(h - ONE_THIRD)]
}

/// Converts 0..=255 RGB to the components of another space.
///
/// RGB values outside 0..=255 are clamped first. The fourth component is
/// only used by CMYK (black) and is 0 otherwise.
///
/// # Errors
///
/// [`UnsupportedSpace`] for anything but the RGB, HSB, CMYK, Lab, gray,
/// HSL and XYZ spaces.
///
/// # Examples
///
/// ```
/// use eightbf_host::host::colors::rgb_to_space;
/// use eightbf_host::host::constants::COLOR_SPACE_CMYK;
/// assert_eq!(rgb_to_space([255, 0, 0], COLOR_SPACE_CMYK).unwrap(), [0, 255, 255, 0]);
/// ```
pub fn rgb_to_space(rgb: [i32; 3], space: i16) -> Result<[i16; 4], UnsupportedSpace> {
    let rgb = rgb.map(|v| f64::from(v).clamp(0.0, MAX));
    let [r, g, b] = rgb;
    let unit = rgb.map(|v| v / MAX);
    Ok(match space {
        c::COLOR_SPACE_RGB => [clamp_byte(r), clamp_byte(g), clamp_byte(b), 0],
        c::COLOR_SPACE_HSB => {
            let [h, s, v] = rgb_to_hsv(unit[0], unit[1], unit[2]);
            [
                clamp(h * HUE_DEGREES, 0.0, MAX_HUE),
                clamp_byte(s * MAX),
                clamp_byte(v * MAX),
                0,
            ]
        }
        c::COLOR_SPACE_HSL => {
            let [h, l, s] = rgb_to_hls(unit[0], unit[1], unit[2]);
            [
                clamp(h * HUE_DEGREES, 0.0, MAX_HUE),
                clamp_byte(s * MAX),
                clamp_byte(l * MAX),
                0,
            ]
        }
        c::COLOR_SPACE_CMYK => {
            let [cyan, magenta, yellow] = [MAX - r, MAX - g, MAX - b];
            let black = cyan.min(magenta).min(yellow);
            if black == MAX {
                return Ok([0, 0, 0, clamp_byte(MAX)]);
            }
            let scale = MAX / (MAX - black);
            [
                clamp_byte((cyan - black) * scale),
                clamp_byte((magenta - black) * scale),
                clamp_byte((yellow - black) * scale),
                clamp_byte(black),
            ]
        }
        c::COLOR_SPACE_GRAY => [clamp_byte(LUMA[0] * r + LUMA[1] * g + LUMA[2] * b), 0, 0, 0],
        c::COLOR_SPACE_XYZ => {
            let [x, y, z] = rgb_to_xyz(rgb);
            [
                clamp_byte(x * MAX),
                clamp_byte(y * MAX),
                clamp_byte(z * MAX),
                0,
            ]
        }
        c::COLOR_SPACE_LAB => {
            let xyz = rgb_to_xyz(rgb);
            let [fx, fy, fz] = [0, 1, 2].map(|i| lab_f(xyz[i] / WHITE[i]));
            let lightness = 116.0 * fy - 16.0;
            [
                clamp_byte(lightness * MAX / LAB_L_SCALE),
                clamp(500.0 * (fx - fy), LAB_AB_MIN, LAB_AB_MAX),
                clamp(200.0 * (fy - fz), LAB_AB_MIN, LAB_AB_MAX),
                0,
            ]
        }
        other => return Err(UnsupportedSpace(other)),
    })
}

/// Converts the components of a color space to 0..=255 RGB.
///
/// Hue wraps around modulo 360; every result is clamped to 0..=255.
///
/// # Errors
///
/// [`UnsupportedSpace`] for anything but the RGB, HSB, CMYK, Lab, gray,
/// HSL and XYZ spaces.
///
/// # Examples
///
/// ```
/// use eightbf_host::host::colors::space_to_rgb;
/// use eightbf_host::host::constants::COLOR_SPACE_HSB;
/// assert_eq!(space_to_rgb([120, 255, 255, 0], COLOR_SPACE_HSB).unwrap(), [0, 255, 0]);
/// ```
pub fn space_to_rgb(components: [i16; 4], space: i16) -> Result<[i32; 3], UnsupportedSpace> {
    let [a, b, cc, d] = components.map(f64::from);
    let rgb: [f64; 3] = match space {
        c::COLOR_SPACE_RGB => [a, b, cc],
        c::COLOR_SPACE_HSB => {
            let hue = a.rem_euclid(HUE_DEGREES) / HUE_DEGREES;
            hsv_to_rgb(hue, b / MAX, cc / MAX).map(|v| v * MAX)
        }
        c::COLOR_SPACE_HSL => {
            let hue = a.rem_euclid(HUE_DEGREES) / HUE_DEGREES;
            hls_to_rgb(hue, cc / MAX, b / MAX).map(|v| v * MAX)
        }
        c::COLOR_SPACE_CMYK => {
            let black = d / MAX;
            [a, b, cc].map(|v| MAX * (1.0 - v / MAX) * (1.0 - black))
        }
        c::COLOR_SPACE_GRAY => [a; 3],
        c::COLOR_SPACE_XYZ => xyz_to_rgb([a, b, cc].map(|v| v / MAX)),
        c::COLOR_SPACE_LAB => {
            let lightness = a * LAB_L_SCALE / MAX;
            let fy = (lightness + 16.0) / 116.0;
            let f = [fy + b / 500.0, fy, fy - cc / 200.0];
            xyz_to_rgb([0, 1, 2].map(|i| lab_f_inv(f[i]) * WHITE[i]))
        }
        other => return Err(UnsupportedSpace(other)),
    };
    Ok(rgb.map(|v| i32::from(clamp_byte(v))))
}

/// Converts components from one color space to another through RGB.
///
/// When `source == target` the components are returned unchanged, even
/// for spaces this module cannot otherwise convert.
///
/// # Errors
///
/// [`UnsupportedSpace`] when either space is not supported.
pub fn convert(
    components: [i16; 4],
    source: i16,
    target: i16,
) -> Result<[i16; 4], UnsupportedSpace> {
    if source == target {
        return Ok(components);
    }
    rgb_to_space(space_to_rgb(components, source)?, target)
}

/// Shows the native color picker and returns the chosen 0..=255 RGB color.
///
/// Uses the Win32 common dialog (which Wine also provides), owned by
/// `parent_window` when it is non-zero. Returns `None` when the user
/// cancels and, on other platforms, always; the caller then reports the
/// picker as unavailable. Initial values outside 0..=255 are clamped.
pub fn choose_color(initial: [i32; 3], parent_window: isize) -> Option<[i32; 3]> {
    platform::choose_color(
        initial.map(|v| v.clamp(0, i32::from(u8::MAX)) as u8),
        parent_window,
    )
}

#[cfg(windows)]
mod platform {
    use std::sync::Mutex;

    use windows_sys::Win32::UI::Controls::Dialogs::{
        ChooseColorW, CC_FULLOPEN, CC_RGBINIT, CHOOSECOLORW,
    };

    /// Number of custom color slots in the dialog.
    const CUSTOM_COLORS: usize = 16;
    /// White, as a `COLORREF`.
    const WHITE: u32 = 0x00FF_FFFF;

    /// The dialog's custom colors, kept between calls like Photoshop does.
    static CUSTOM: Mutex<[u32; CUSTOM_COLORS]> = Mutex::new([WHITE; CUSTOM_COLORS]);

    pub fn choose_color(initial: [u8; 3], parent_window: isize) -> Option<[i32; 3]> {
        let [r, g, b] = initial.map(u32::from);
        let mut custom = CUSTOM
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut info = CHOOSECOLORW {
            // The struct is a few dozen bytes; the size always fits in u32.
            lStructSize: std::mem::size_of::<CHOOSECOLORW>() as u32,
            hwndOwner: parent_window as _,
            hInstance: std::ptr::null_mut(),
            rgbResult: r | (g << 8) | (b << 16),
            lpCustColors: custom.as_mut_ptr(),
            Flags: CC_RGBINIT | CC_FULLOPEN,
            lCustData: 0,
            lpfnHook: None,
            lpTemplateName: std::ptr::null(),
        };
        // SAFETY: `info` is fully initialized with its own size, and
        // `lpCustColors` points at 16 COLORREFs that stay locked and alive
        // for the whole (modal) call. No hook or template is used.
        let accepted = unsafe { ChooseColorW(&mut info) } != 0;
        if !accepted {
            return None;
        }
        let value = info.rgbResult;
        Some([value, value >> 8, value >> 16].map(|v| i32::from((v & 0xFF) as u8)))
    }
}

#[cfg(not(windows))]
mod platform {
    pub fn choose_color(_initial: [u8; 3], _parent_window: isize) -> Option<[i32; 3]> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SPACES: [i16; 7] = [
        c::COLOR_SPACE_RGB,
        c::COLOR_SPACE_HSB,
        c::COLOR_SPACE_CMYK,
        c::COLOR_SPACE_LAB,
        c::COLOR_SPACE_GRAY,
        c::COLOR_SPACE_HSL,
        c::COLOR_SPACE_XYZ,
    ];

    fn sample_colors() -> Vec<[i32; 3]> {
        let mut colors = Vec::new();
        for r in (0..=255).step_by(51) {
            for g in (0..=255).step_by(51) {
                for b in (0..=255).step_by(51) {
                    colors.push([r, g, b]);
                }
            }
        }
        colors.extend([[12, 200, 77], [128, 128, 128], [1, 2, 3]]);
        colors
    }

    #[test]
    fn known_values() {
        assert_eq!(
            rgb_to_space([10, 20, 30], c::COLOR_SPACE_RGB).unwrap(),
            [10, 20, 30, 0]
        );
        assert_eq!(
            rgb_to_space([255, 0, 0], c::COLOR_SPACE_HSB).unwrap(),
            [0, 255, 255, 0]
        );
        assert_eq!(
            rgb_to_space([0, 0, 255], c::COLOR_SPACE_HSL).unwrap(),
            [240, 255, 128, 0]
        );
        assert_eq!(
            rgb_to_space([0, 0, 0], c::COLOR_SPACE_CMYK).unwrap(),
            [0, 0, 0, 255]
        );
        assert_eq!(
            rgb_to_space([255, 255, 255], c::COLOR_SPACE_CMYK).unwrap(),
            [0, 0, 0, 0]
        );
        assert_eq!(
            rgb_to_space([255, 255, 255], c::COLOR_SPACE_GRAY).unwrap(),
            [255, 0, 0, 0]
        );
        assert_eq!(
            rgb_to_space([255, 255, 255], c::COLOR_SPACE_LAB).unwrap(),
            [255, 0, 0, 0]
        );
        assert_eq!(
            rgb_to_space([0, 0, 0], c::COLOR_SPACE_LAB).unwrap(),
            [0, 0, 0, 0]
        );
        let red = rgb_to_space([255, 0, 0], c::COLOR_SPACE_LAB).unwrap();
        assert_eq!(red[0], 136); // L* 53.24
        assert_eq!(red[1], 80);
        assert_eq!(red[2], 67);
        assert_eq!(
            rgb_to_space([255, 255, 255], c::COLOR_SPACE_XYZ).unwrap(),
            [242, 255, 255, 0]
        );
    }

    #[test]
    fn components_stay_in_their_ranges() {
        for rgb in sample_colors() {
            let hsb = rgb_to_space(rgb, c::COLOR_SPACE_HSB).unwrap();
            assert!((0..=359).contains(&hsb[0]));
            let lab = rgb_to_space(rgb, c::COLOR_SPACE_LAB).unwrap();
            assert!((0..=255).contains(&lab[0]));
            assert!((-128..=127).contains(&lab[1]) && (-128..=127).contains(&lab[2]));
        }
        let blue = rgb_to_space([0, 0, 255], c::COLOR_SPACE_LAB).unwrap();
        assert_eq!(blue[2], -108);
    }

    #[test]
    fn exact_round_trips() {
        for rgb in sample_colors() {
            for space in [c::COLOR_SPACE_RGB, c::COLOR_SPACE_CMYK] {
                let components = rgb_to_space(rgb, space).unwrap();
                assert_eq!(
                    space_to_rgb(components, space).unwrap(),
                    rgb,
                    "{rgb:?} via {space}"
                );
            }
        }
    }

    #[test]
    fn approximate_round_trips() {
        // HSB and HSL lose only rounding; Lab clamps a/b for saturated
        // colors and 8-bit XYZ loses precision in the shadows, so those
        // two are checked on moderate colors. (White's Z exceeds 1.0 and is
        // clamped in 8-bit XYZ.)
        let moderate = [
            [128, 128, 128],
            [200, 150, 100],
            [90, 120, 140],
            [230, 230, 230],
            [180, 60, 70],
        ];
        let cases: [(i16, i32, &[[i32; 3]]); 4] = [
            (c::COLOR_SPACE_HSB, 3, &sample_colors()),
            (c::COLOR_SPACE_HSL, 4, &sample_colors()),
            (c::COLOR_SPACE_LAB, 2, &moderate),
            (c::COLOR_SPACE_XYZ, 3, &moderate),
        ];
        for (space, tolerance, colors) in cases {
            for &rgb in colors {
                let back = space_to_rgb(rgb_to_space(rgb, space).unwrap(), space).unwrap();
                for (a, b) in rgb.iter().zip(back) {
                    assert!(
                        (a - b).abs() <= tolerance,
                        "{rgb:?} via {space} gave {back:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn gray_round_trip_is_neutral() {
        let gray = rgb_to_space([30, 60, 90], c::COLOR_SPACE_GRAY).unwrap();
        assert_eq!(gray[0], 54); // 0.299 * 30 + 0.587 * 60 + 0.114 * 90 = 54.45
        assert_eq!(
            space_to_rgb(gray, c::COLOR_SPACE_GRAY).unwrap(),
            [54, 54, 54]
        );
    }

    #[test]
    fn out_of_range_input_is_clamped_or_wrapped() {
        assert_eq!(
            rgb_to_space([-5, 300, 128], c::COLOR_SPACE_RGB).unwrap(),
            [0, 255, 128, 0]
        );
        assert_eq!(
            space_to_rgb([400, -2, 999, 0], c::COLOR_SPACE_RGB).unwrap(),
            [255, 0, 255]
        );
        let wrapped = space_to_rgb([360 + 120, 255, 255, 0], c::COLOR_SPACE_HSB).unwrap();
        assert_eq!(wrapped, [0, 255, 0]);
        let negative = space_to_rgb([-240, 255, 255, 0], c::COLOR_SPACE_HSB).unwrap();
        assert_eq!(negative, [0, 255, 0]);
        for space in SPACES {
            for extreme in [i16::MIN, i16::MAX] {
                let rgb = space_to_rgb([extreme; 4], space).unwrap();
                assert!(rgb.iter().all(|v| (0..=255).contains(v)));
            }
        }
    }

    #[test]
    fn rounding_is_half_to_even() {
        assert_eq!(clamp(0.5, 0.0, MAX), 0);
        assert_eq!(clamp(1.5, 0.0, MAX), 2);
        assert_eq!(clamp(f64::NAN, -128.0, 127.0), -128);
    }

    #[test]
    fn convert_goes_through_rgb() {
        let hsb = convert([255, 0, 0, 0], c::COLOR_SPACE_RGB, c::COLOR_SPACE_HSB).unwrap();
        assert_eq!(hsb, [0, 255, 255, 0]);
        let cmyk = convert(hsb, c::COLOR_SPACE_HSB, c::COLOR_SPACE_CMYK).unwrap();
        assert_eq!(cmyk, [0, 255, 255, 0]);
        let same = convert([1, 2, 3, 4], 42, 42).unwrap();
        assert_eq!(same, [1, 2, 3, 4]);
    }

    #[test]
    fn unsupported_spaces_are_errors() {
        assert_eq!(
            rgb_to_space([0; 3], c::COLOR_SPACE_CHOSEN).unwrap_err().0,
            -1
        );
        assert_eq!(space_to_rgb([0; 4], 7).unwrap_err().0, 7);
        assert!(convert([0; 4], c::COLOR_SPACE_RGB, 99).is_err());
        assert!(convert([0; 4], 99, c::COLOR_SPACE_RGB).is_err());
    }

    #[cfg(not(windows))]
    #[test]
    fn choose_color_is_unavailable_off_windows() {
        assert_eq!(choose_color([1, 2, 3], 0), None);
    }
}
