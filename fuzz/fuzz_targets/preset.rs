//! Preset files, which users load with `--preset` or upload to the server.
#![no_main]

use eightbf_host::preset::Preset;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|text: &str| {
    let Ok(preset) = Preset::from_json(text) else {
        return;
    };
    let saved = preset.to_json();
    assert!(
        Preset::from_json(&saved).is_ok(),
        "a saved preset no longer loads: {saved}"
    );
});
