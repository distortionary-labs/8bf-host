//! Filter presets: a filter's saved parameters, as a small JSON file.
//!
//! A preset holds the opaque parameter block the filter wrote and, for
//! scripting-aware filters, its descriptor. Running a filter with a preset
//! and no dialog reproduces the settings it was saved with.
//!
//! The format:
//!
//! ```json
//! {
//!   "format": "8bf-host-preset",
//!   "version": 1,
//!   "filter": {"name": "Unsharp", "category": "Acme", "id": "1a2b3c4d5e6f"},
//!   "parameters": "<base64>",
//!   "descriptor": [{"key": "Amnt", "type": "long", "value": 50}]
//! }
//! ```
//!
//! Older parameter files (`{"key", "parameters",
//! "descriptor"}`) load too.

use std::path::Path;

use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::catalog::filter_id;
use crate::host::descriptors::Descriptor;
use crate::host::discovery::PluginInfo;

pub const FORMAT: &str = "8bf-host-preset";
pub const VERSION: u32 = 1;

/// Which filter a preset was saved from. Informational: presets are not
/// tied to a path, so they keep working when filters move.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PresetFilter {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub category: String,
    #[serde(default)]
    pub id: String,
}

/// A filter's saved settings.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Preset {
    pub filter: PresetFilter,
    pub parameters: Option<Vec<u8>>,
    pub descriptor: Option<Descriptor>,
}

#[derive(Serialize, Deserialize)]
struct PresetFile {
    #[serde(default)]
    format: Option<String>,
    #[serde(default)]
    version: Option<u32>,
    #[serde(default)]
    filter: Option<PresetFilter>,
    /// Older parameter files stored the filter key instead.
    #[serde(default, skip_serializing)]
    key: Option<String>,
    #[serde(default)]
    parameters: Option<String>,
    #[serde(default)]
    descriptor: Option<Value>,
}

/// A preset file is not valid.
#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct PresetError(pub String);

impl Preset {
    /// Builds a preset from a finished run.
    pub fn from_run(
        info: &PluginInfo,
        parameters: Option<Vec<u8>>,
        descriptor: Option<Descriptor>,
    ) -> Self {
        Self {
            filter: PresetFilter {
                name: info.name(),
                category: info.category(),
                id: filter_id(&info.key()),
            },
            parameters,
            descriptor,
        }
    }

    /// True when the preset has neither parameters nor a descriptor.
    pub fn is_empty(&self) -> bool {
        self.parameters.is_none() && self.descriptor.is_none()
    }

    /// Serializes the preset as pretty-printed JSON.
    pub fn to_json(&self) -> String {
        let file = PresetFile {
            format: Some(FORMAT.into()),
            version: Some(VERSION),
            filter: Some(self.filter.clone()),
            key: None,
            parameters: self.parameters.as_ref().map(|bytes| BASE64.encode(bytes)),
            descriptor: self.descriptor.as_ref().map(Descriptor::to_json),
        };
        let mut text = serde_json::to_string_pretty(&file).unwrap_or_default();
        text.push('\n');
        text
    }

    /// Parses a preset.
    ///
    /// # Errors
    ///
    /// Fails for invalid JSON, a different format or newer version, bad
    /// base64, or a malformed descriptor.
    pub fn from_json(text: &str) -> Result<Self, PresetError> {
        let value: Value = serde_json::from_str(text)
            .map_err(|error| PresetError(format!("not a preset file: {error}")))?;
        if !value.is_object() {
            return Err(PresetError(
                "not a preset file: expected a JSON object".into(),
            ));
        }
        let file: PresetFile = serde_json::from_value(value)
            .map_err(|error| PresetError(format!("not a preset file: {error}")))?;
        if let Some(format) = &file.format {
            if format != FORMAT {
                return Err(PresetError(format!("unknown preset format '{format}'")));
            }
        }
        if file.version.is_some_and(|version| version > VERSION) {
            return Err(PresetError(
                "this preset was written by a newer 8bf-host; upgrade to read it".into(),
            ));
        }
        let parameters = file
            .parameters
            .map(|text| BASE64.decode(text.trim()))
            .transpose()
            .map_err(|error| PresetError(format!("parameters are not valid base64: {error}")))?;
        let descriptor = match file.descriptor {
            None | Some(Value::Null) => None,
            Some(value) => Some(
                Descriptor::from_json(&value)
                    .map_err(|error| PresetError(format!("invalid descriptor: {error}")))?,
            ),
        };
        let filter = file.filter.unwrap_or_else(|| PresetFilter {
            name: file
                .key
                .as_deref()
                .and_then(|key| key.rsplit('|').next())
                .unwrap_or_default()
                .to_string(),
            ..PresetFilter::default()
        });
        Ok(Self {
            filter,
            parameters,
            descriptor,
        })
    }

    /// Reads a preset file.
    ///
    /// # Errors
    ///
    /// Fails when the file cannot be read or is not a valid preset.
    pub fn load(path: &Path) -> Result<Self, PresetError> {
        let text = std::fs::read_to_string(path)
            .map_err(|error| PresetError(format!("could not read {}: {error}", path.display())))?;
        Self::from_json(&text).map_err(|error| PresetError(format!("{}: {error}", path.display())))
    }

    /// Writes a preset file.
    ///
    /// # Errors
    ///
    /// Fails when the file cannot be written.
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        std::fs::write(path, self.to_json())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host::descriptors::DescValue;

    #[test]
    fn round_trips_through_json() {
        let mut descriptor = Descriptor::default();
        descriptor.put(0x416d_6e74, 0x6c6f_6e67, DescValue::Integer(50));
        let preset = Preset {
            filter: PresetFilter {
                name: "Unsharp".into(),
                category: "Acme".into(),
                id: "abc".into(),
            },
            parameters: Some(vec![1, 2, 3, 0, 255]),
            descriptor: Some(descriptor),
        };
        assert_eq!(Preset::from_json(&preset.to_json()).unwrap(), preset);
    }

    #[test]
    fn reads_legacy_parameter_files() {
        let preset = Preset::from_json(
            r#"{"key": "/p/a.8bf|16000|Sharpen", "parameters": "AQI=", "descriptor": null}"#,
        )
        .unwrap();
        assert_eq!(preset.filter.name, "Sharpen");
        assert_eq!(preset.parameters, Some(vec![1, 2]));
        assert!(preset.descriptor.is_none());
    }

    #[test]
    fn rejects_other_formats_newer_versions_and_bad_data() {
        assert!(Preset::from_json("[]").is_err());
        assert!(Preset::from_json(r#"{"format": "other"}"#).is_err());
        assert!(Preset::from_json(r#"{"format": "8bf-host-preset", "version": 99}"#).is_err());
        assert!(Preset::from_json(r#"{"parameters": "***"}"#).is_err());
        assert!(Preset::from_json(r#"{"descriptor": "nope"}"#).is_err());
    }

    #[test]
    fn empty_presets_are_detected() {
        assert!(Preset::default().is_empty());
    }
}
