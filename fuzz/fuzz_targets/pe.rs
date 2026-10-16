//! Windows filters: the PE headers, the resource tree, then every PiPL in it.
#![no_main]

use eightbf_host::host::pe::{PeFile, ResourceKey};
use eightbf_host::host::pipl::{parse_pipl, ByteOrder};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Ok(image) = PeFile::parse(data.to_vec()) else {
        return;
    };
    let _ = image.arch();
    let Ok(resources) = image.resources(&ResourceKey::Name("PIPL".to_owned())) else {
        return;
    };
    for (_, payload) in resources {
        let _ = parse_pipl(&payload, ByteOrder::Little, None);
    }
});
