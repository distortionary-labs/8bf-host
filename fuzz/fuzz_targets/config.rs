//! The TOML configuration file.
#![no_main]

use eightbf_host::config::Config;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|text: &str| {
    let Ok(config) = Config::from_toml(text) else {
        return;
    };
    let saved = config.to_toml();
    assert!(
        Config::from_toml(&saved).is_ok(),
        "a saved configuration no longer loads: {saved}"
    );
});
