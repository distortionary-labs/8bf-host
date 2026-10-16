//! Image uploads and their masks, under the server's pixel limit, then the
//! trip back out as the server does it after an identity filter.
#![no_main]

use eightbf_host::imageio::{decode, decode_mask, encode};
use image::ImageFormat;
use libfuzzer_sys::fuzz_target;

/// Small enough that a decoded image can't exhaust the fuzzer's memory.
const MAX_PIXELS: u64 = 1 << 20;

fuzz_target!(|data: &[u8]| {
    let Ok(loaded) = decode(data, Some(MAX_PIXELS)) else {
        return;
    };
    let (width, height) = (loaded.image.width, loaded.image.height);
    let _ = decode_mask(data, width, height);
    let output = loaded
        .to_dynamic(&loaded.image)
        .expect("an unchanged image converts back");
    encode(output, ImageFormat::Png).expect("a decoded image encodes as PNG");
});
