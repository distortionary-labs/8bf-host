//! Descriptor handles: filters hand these back to the host between runs.
#![no_main]

use eightbf_host::host::descriptors::Descriptor;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Some(descriptor) = Descriptor::deserialize(data) else {
        return;
    };
    // Whatever the host accepts, it must be able to store and read back.
    let stored = descriptor.serialize();
    assert!(
        Descriptor::deserialize(&stored).is_some(),
        "a stored descriptor no longer reads back"
    );
});
