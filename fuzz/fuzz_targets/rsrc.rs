//! Mac filters: the resource fork, then every PiPL in it.
#![no_main]

use eightbf_host::host::pipl::{parse_pipl, ByteOrder};
use eightbf_host::host::rsrc::read_resources;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Ok(resources) = read_resources(data, b"PiPL") else {
        return;
    };
    for (id, payload) in resources {
        let _ = parse_pipl(&payload, ByteOrder::Big, Some(i32::from(id)));
    }
});
