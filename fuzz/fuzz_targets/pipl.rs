//! A bare PiPL resource, as found in a native filter's `.pipl` sidecar.
//!
//! The first byte picks the byte order; the rest is the resource.
#![no_main]

use eightbf_host::host::pipl::{parse_pipl, ByteOrder};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Some((&selector, resource)) = data.split_first() else {
        return;
    };
    let order = if selector & 1 == 0 {
        ByteOrder::Little
    } else {
        ByteOrder::Big
    };
    let Ok(pipl) = parse_pipl(resource, order, None) else {
        return;
    };
    // The catalog cache stores PiPLs as JSON.
    let json = serde_json::to_string(&pipl).expect("a parsed PiPL serializes");
    let _: eightbf_host::host::pipl::PiPL =
        serde_json::from_str(&json).expect("a serialized PiPL reads back");
    let _ = (pipl.is_filter(), pipl.is_hidden());
    for mode in 0..16 {
        let _ = pipl.supports_mode(mode);
    }
    for filter_case in 0..8 {
        let _ = pipl.case_info(filter_case);
    }
});
