//! Runs the C test filter through the host, in this build's FilterRecord
//! layout. `cargo test --features pack4` repeats everything with the
//! Windows layout.

mod common;

use std::cell::RefCell;
use std::rc::Rc;

use common::{default_image, make_info, pixel, rgba_image, ALL_MODES, RGB8_ONLY};
use eightbf_host::host::constants as c;
use eightbf_host::host::descriptors::{DescValue, Descriptor};
use eightbf_host::host::image::PlaneImage;
use eightbf_host::host::pipl::FilterCaseInfo;
use eightbf_host::host::session::{FilterHost, FilterRequest, FilterResult, Hooks, RunStatus};

fn run_with(
    host: &mut FilterHost,
    entry: &str,
    request: FilterRequest,
    image_modes: Option<u16>,
    cases: Option<Vec<FilterCaseInfo>>,
    hooks: Hooks,
) -> Option<FilterResult> {
    let library = common::test_filter()?;
    let info = make_info(library, entry, image_modes, cases);
    Some(host.run(&info, request, hooks, Some("native")))
}

fn run(entry: &str, request: FilterRequest) -> Option<FilterResult> {
    run_with(
        &mut FilterHost::new(),
        entry,
        request,
        None,
        None,
        Hooks::default(),
    )
}

fn request(image: PlaneImage) -> FilterRequest {
    let mut request = FilterRequest::new(image, 3, true);
    request.show_ui = true;
    request
}

fn fourcc(code: &[u8; 4]) -> u32 {
    c::fourcc(code)
}

macro_rules! ok {
    ($result:expr) => {{
        let Some(result) = $result else {
            eprintln!("skipped: needs a C compiler (gcc or clang)");
            return;
        };
        assert!(
            result.is_ok(),
            "{}\n{}",
            result.message,
            result.log.join("\n")
        );
        result
    }};
}

macro_rules! finished {
    ($result:expr) => {{
        let Some(result) = $result else {
            eprintln!("skipped: needs a C compiler (gcc or clang)");
            return;
        };
        result
    }};
}

#[test]
fn c_and_rust_agree_on_the_filter_record_layout() {
    let Some(library) = common::test_filter() else {
        return;
    };
    // SAFETY: the test filter exports these two functions with no arguments.
    unsafe {
        let lib = libloading::Library::new(library).unwrap();
        let size: libloading::Symbol<unsafe extern "C" fn() -> i32> =
            lib.get(b"TestFilterRecordSize\0").unwrap();
        let depth: libloading::Symbol<unsafe extern "C" fn() -> i32> =
            lib.get(b"TestFilterRecordOffsetOfDepth\0").unwrap();
        type Record = eightbf_host::host::structs::FilterRecord;
        assert_eq!(size() as usize, std::mem::size_of::<Record>());
        assert_eq!(depth() as usize, std::mem::offset_of!(Record, depth));
    }
}

#[test]
fn invert_tiles_color_planes_and_keeps_alpha() {
    let image = default_image();
    let result = ok!(run("InvertMain", request(image.clone())));
    let output = result.image.unwrap();
    for (x, y) in [(0, 0), (5, 10), (3, 7)] {
        let (before, after) = (pixel(&image, x, y), pixel(&output, x, y));
        assert_eq!(
            after[..3],
            before[..3].iter().map(|v| 255 - v).collect::<Vec<_>>()[..]
        );
        assert_eq!(after[3], before[3]);
    }
}

#[test]
fn invert_sixteen_bit() {
    let image = rgba_image(6, 11, None, 16);
    let mut host = FilterHost::new();
    let result = ok!(run_with(
        &mut host,
        "InvertMain",
        request(image.clone()),
        Some(ALL_MODES),
        None,
        Hooks::default()
    ));
    let output = result.image.unwrap();
    let (before, after) = (pixel(&image, 4, 9), pixel(&output, 4, 9));
    assert_eq!(
        after[..3],
        before[..3].iter().map(|v| 32768 - v).collect::<Vec<_>>()[..]
    );
    assert_eq!(after[3], before[3]);
}

#[test]
fn opaque_images_use_the_flat_case_and_three_planes() {
    let image = rgba_image(6, 11, Some(255), 8);
    let result = ok!(run("PlanarMain", request(image.clone())));
    let (before, after) = (
        pixel(&image, 2, 3),
        pixel(result.image.as_ref().unwrap(), 2, 3),
    );
    assert_eq!(
        after,
        [
            (before[0] + 10).min(255),
            (before[1] + 20).min(255),
            (before[2] + 30).min(255),
            255
        ]
    );
}

#[test]
fn grayscale() {
    let data: Vec<u8> = (0..20u8).flat_map(|i| [i * 10, 200]).collect();
    let image = PlaneImage::from_data(5, 4, 2, 8, data).unwrap();
    let mut request = FilterRequest::new(image, 1, true);
    request.show_ui = true;
    let result = ok!(run("InvertMain", request));
    assert_eq!(pixel(result.image.as_ref().unwrap(), 1, 0), [255 - 10, 200]);
}

#[test]
fn images_without_alpha() {
    let data: Vec<u8> = (0..12u8).flat_map(|i| [i, i * 2, i * 3]).collect();
    let image = PlaneImage::from_data(4, 3, 3, 8, data).unwrap();
    let result = ok!(run(
        "InvertMain",
        FilterRequest::new(image.clone(), 3, false)
    ));
    let output = result.image.unwrap();
    assert_eq!(
        pixel(&output, 2, 1),
        pixel(&image, 2, 1)
            .iter()
            .map(|v| 255 - v)
            .collect::<Vec<_>>()
    );
}

#[test]
fn advance_state_inside_start() {
    let image = default_image();
    let result = ok!(run("AdvanceMain", request(image.clone())));
    let after = pixel(result.image.as_ref().unwrap(), 5, 10);
    let before = pixel(&image, 5, 10);
    assert_eq!(
        after[..3],
        before[..3].iter().map(|v| 255 - v).collect::<Vec<_>>()[..]
    );
}

#[test]
fn edge_replication_padding() {
    let image = default_image();
    let result = ok!(run("PaddingMain", request(image.clone())));
    let output = result.image.unwrap();
    for y in [0, 6] {
        assert_eq!(pixel(&output, 0, y), pixel(&image, 0, y));
        for x in 1..image.width {
            assert_eq!(pixel(&output, x, y), pixel(&image, x - 1, y));
        }
    }
}

#[test]
fn suites_and_descriptor_round_trip() {
    let image = default_image();
    let first = ok!(run("SuiteMain", request(image.clone())));
    assert_eq!(pixel(first.image.as_ref().unwrap(), 0, 0)[..3], [7, 7, 7]);
    let descriptor = first.descriptor.expect("descriptor");
    assert_eq!(
        descriptor.get(fourcc(b"Amnt")),
        Some((c::TYPE_INTEGER, &DescValue::Integer(7)))
    );

    let mut replay = Descriptor::default();
    replay.put(fourcc(b"Amnt"), c::TYPE_INTEGER, DescValue::Integer(42));
    let mut second = FilterRequest::new(image, 3, true);
    second.descriptor = Some(replay);
    second.parent_window = 3;
    let second = ok!(run("SuiteMain", second));
    assert_eq!(
        pixel(second.image.as_ref().unwrap(), 0, 0)[..3],
        [45, 45, 45]
    );
}

#[test]
fn mask_is_delivered_and_limits_the_change() {
    let image = rgba_image(6, 11, Some(255), 8);
    let mut mask = PlaneImage::new(image.width, image.height, 1, 8).unwrap();
    for y in 2..6 {
        for x in 1..4 {
            mask.data[y * image.width + x] = 100 + x as u8;
        }
    }
    let mut request = request(image.clone());
    request.mask = Some(mask);
    let result = ok!(run("MaskMain", request));
    let output = result.image.unwrap();
    assert_eq!(pixel(&output, 2, 3)[0], 102);
    // Outside the selection's bounding box nothing changes.
    assert_eq!(pixel(&output, 5, 9), pixel(&image, 5, 9));
}

#[test]
fn auto_mask_blends_partial_selection() {
    let image = rgba_image(6, 11, Some(255), 8);
    let mut mask = PlaneImage::new(image.width, image.height, 1, 8).unwrap();
    mask.data[0] = 255;
    mask.data[1] = 128;
    let mut request = request(image.clone());
    request.mask = Some(mask);
    let result = ok!(run("InvertMain", request));
    let output = result.image.unwrap();
    let inverted: Vec<u32> = pixel(&image, 0, 0)[..3].iter().map(|v| 255 - v).collect();
    assert_eq!(pixel(&output, 0, 0)[..3], inverted[..]);
    let original = pixel(&image, 1, 0)[0] as i64;
    let expected = original + ((255 - 2 * original) * 128 + 127).div_euclid(255);
    assert_eq!(i64::from(pixel(&output, 1, 0)[0]), expected);
    assert_eq!(pixel(&output, 2, 0), pixel(&image, 2, 0));
}

#[test]
fn input_sampling() {
    let image = rgba_image(8, 6, Some(255), 8);
    let result = ok!(run("SampleMain", request(image.clone())));
    let output = result.image.unwrap();
    for (x, y) in [(0, 0), (1, 1), (5, 3), (7, 5)] {
        assert_eq!(
            pixel(&output, x, y)[0],
            pixel(&image, (x / 2) * 2, (y / 2) * 2)[0]
        );
    }
}

#[test]
fn error_string_is_reported() {
    let result = finished!(run("ErrorMain", request(default_image())));
    assert_eq!(result.status, RunStatus::Error);
    assert!(
        result.message.contains("failed on purpose"),
        "{}",
        result.message
    );
    assert_eq!(result.code, c::ERR_REPORT_STRING);
}

#[test]
fn cancel_in_parameters() {
    let result = finished!(run("CancelMain", request(default_image())));
    assert_eq!(result.status, RunStatus::Cancelled);
}

#[test]
fn progress_and_abort() {
    let seen = Rc::new(RefCell::new(Vec::new()));
    let progress_seen = Rc::clone(&seen);
    let abort_seen = Rc::clone(&seen);
    let hooks = Hooks {
        progress: Box::new(move |done, total| progress_seen.borrow_mut().push((done, total))),
        abort: Box::new(move || abort_seen.borrow().len() >= 5),
        log: Box::new(|_| {}),
    };
    let result = finished!(run_with(
        &mut FilterHost::new(),
        "ProgressMain",
        request(default_image()),
        None,
        None,
        hooks
    ));
    assert_eq!(result.status, RunStatus::Cancelled);
    assert_eq!(seen.borrow()[..2], [(0, 100), (1, 100)]);
}

#[test]
fn parameters_and_globals_persist_in_one_host() {
    let mut host = FilterHost::new();
    let image = default_image();
    let first = ok!(run_with(
        &mut host,
        "InvertMain",
        request(image.clone()),
        None,
        None,
        Hooks::default()
    ));
    let second = ok!(run_with(
        &mut host,
        "InvertMain",
        FilterRequest::new(image, 3, true),
        None,
        None,
        Hooks::default()
    ));
    let parameters = first.parameters.clone().expect("parameters");
    assert_eq!(parameters[..4], 0x1234u32.to_le_bytes());
    let runs = |result: &FilterResult| {
        result
            .descriptor
            .as_ref()
            .unwrap()
            .get(fourcc(b"Runs"))
            .map(|(_, v)| v.clone())
    };
    assert_eq!(runs(&first), Some(DescValue::Integer(1)));
    assert_eq!(runs(&second), Some(DescValue::Integer(2)));
}

#[test]
fn saved_parameters_restore_in_a_fresh_host() {
    let image = default_image();
    let first = ok!(run("InvertMain", request(image.clone())));
    let mut repeat = FilterRequest::new(image, 3, true);
    repeat.parameters = first.parameters.clone();
    let repeat = ok!(run("InvertMain", repeat));
    assert_eq!(repeat.parameters, first.parameters);
}

#[test]
fn sixteen_bit_images_fall_back_to_eight_bit() {
    let image = rgba_image(6, 11, None, 16);
    let result = ok!(run_with(
        &mut FilterHost::new(),
        "InvertMain",
        request(image.clone()),
        Some(RGB8_ONLY),
        None,
        Hooks::default()
    ));
    let output = result.image.unwrap();
    assert_eq!(output.depth, 16);
    let (before, after) = (pixel(&image, 3, 3), pixel(&output, 3, 3));
    for (b, a) in before[..3].iter().zip(&after[..3]) {
        assert!(
            (i64::from(*a) - (32768 - i64::from(*b))).abs() <= 260,
            "{b} -> {a}"
        );
    }
    assert_eq!(after[3], before[3]);
}

#[test]
fn sixteen_bit_without_fallback_reports_the_mode() {
    let mut request = request(rgba_image(6, 11, None, 16));
    request.allow_8bit_fallback = false;
    let result = finished!(run_with(
        &mut FilterHost::new(),
        "InvertMain",
        request,
        Some(RGB8_ONLY),
        None,
        Hooks::default()
    ));
    assert_eq!(result.status, RunStatus::Error);
    assert!(result.message.contains("16-bit RGB"), "{}", result.message);
}

#[test]
fn filter_case_info_can_refuse_transparency() {
    let allow = FilterCaseInfo {
        input_handling: 1,
        output_handling: 1,
        flags1: 0,
        flags2: 0,
    };
    let refuse = FilterCaseInfo {
        input_handling: 0,
        output_handling: 0,
        flags1: 0,
        flags2: 0,
    };
    let mut cases = vec![allow; 7];
    cases[(c::FILTER_CASE_EDITABLE_TRANSPARENCY_NO_SELECTION - 1) as usize] = refuse;
    let image = default_image();
    let result = ok!(run_with(
        &mut FilterHost::new(),
        "InvertMain",
        request(image.clone()),
        None,
        Some(cases),
        Hooks::default()
    ));
    // Protected transparency: color inverted, alpha preserved.
    let (before, after) = (
        pixel(&image, 2, 2),
        pixel(result.image.as_ref().unwrap(), 2, 2),
    );
    let mut expected: Vec<u32> = before[..3].iter().map(|v| 255 - v).collect();
    expected.push(before[3]);
    assert_eq!(after, expected);
}

#[test]
fn a_missing_entry_point_is_an_error_result() {
    let result = finished!(run("NoSuchEntry", request(default_image())));
    assert_eq!(result.status, RunStatus::Error);
    assert!(
        result.message.contains("does not export NoSuchEntry"),
        "{}",
        result.message
    );
}

#[test]
fn filter_rect_limits_the_area() {
    let image = rgba_image(6, 11, Some(255), 8);
    let mut request = request(image.clone());
    request.filter_rect = Some(eightbf_host::host::image::Rect::new(1, 1, 3, 3));
    let result = ok!(run("InvertMain", request));
    let output = result.image.unwrap();
    assert_ne!(pixel(&output, 1, 1), pixel(&image, 1, 1));
    assert_eq!(pixel(&output, 4, 4), pixel(&image, 4, 4));
}
