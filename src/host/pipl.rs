//! Parsing of PiPL (Plug-in Property List) resources.
//!
//! A PiPL resource describes one plug-in: its kind, name, menu category,
//! entry points and capabilities. Windows plug-ins store PiPLs as
//! little-endian PE resources; macOS plug-ins store them big-endian in a
//! resource fork or a `.rsrc` file.
//!
//! Resource layout:
//!
//! ```text
//! int16  signature   (always 1)
//! int32  version     (always 0)
//! int32  count
//! count x {
//!     OSType vendorID      ('8BIM')
//!     OSType propertyKey
//!     int32  propertyID
//!     int32  propertyLength
//!     byte   data[propertyLength], padded to a multiple of 4
//! }
//! ```
//!
//! The data comes from untrusted files, so every read is bounds checked and
//! malformed resources produce a [`PiPLError`] instead of a panic.

use std::collections::BTreeMap;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use super::constants as c;

/// The PiPL header signature.
const PIPL_SIGNATURE: i16 = 1;
/// The only PiPL header version in use.
const PIPL_VERSION: i32 = 0;
/// Size of the header: signature, version and property count.
const HEADER_SIZE: usize = 10;
/// Size of a property header: vendor, key, id and length.
const PROPERTY_HEADER_SIZE: usize = 16;
/// Property values are padded to a multiple of this many bytes.
const PROPERTY_ALIGNMENT: usize = 4;
/// More properties than this means the resource is garbage.
const MAX_PROPERTY_COUNT: i32 = 10_000;
/// Number of filter cases described by a `fici` property.
const FILTER_CASE_COUNT: usize = 7;
/// Bytes per `FilterCaseInfo` record.
const FILTER_CASE_INFO_SIZE: usize = 4;
/// The mode bit for mode 0; mode `n` is this value shifted right by `n`.
const MODE_BIT_ZERO: u16 = 0x8000;
/// Number of image modes the `mode` property can describe.
const MODE_BIT_COUNT: i16 = 16;
/// Pascal-style entry names start with a length byte below this value.
const PASCAL_ENTRY_MAX_LENGTH: u8 = 0x20;
/// Pascal strings hold at most this many bytes.
const PASCAL_MAX_LENGTH: usize = 255;
/// Prefix of the mis-escaped bytes some plug-ins store in `fici`.
const MISESCAPE_PREFIX: &[u8] = b"/x";
/// Categories starting with this are helpers, not user-facing filters.
const HIDDEN_CATEGORY_PREFIX: &str = "**Hidden*";

/// Architecture names and the PiPL code property that holds each entry point.
///
/// The names are used throughout the host and in saved catalogs.
pub const ENTRY_POINT_KEYS: [(&str, u32); 5] = [
    ("win-x86", c::PIPL_CODE_WIN32_X86),
    ("win-x64", c::PIPL_CODE_WIN64_X86),
    ("win-arm64", c::PIPL_CODE_WIN64_ARM),
    ("mac-x64", c::PIPL_CODE_MAC_INTEL64),
    ("mac-arm64", c::PIPL_CODE_MAC_ARM64),
];

/// Returns the architecture name for a PiPL code property key, if it is one.
pub fn entry_point_arch(key: u32) -> Option<&'static str> {
    ENTRY_POINT_KEYS
        .iter()
        .find(|(_, code)| *code == key)
        .map(|(arch, _)| *arch)
}

/// The byte order of a PiPL resource.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ByteOrder {
    /// Windows resources.
    Little,
    /// Mac resources.
    Big,
}

impl ByteOrder {
    fn u16_from(self, bytes: [u8; 2]) -> u16 {
        match self {
            Self::Little => u16::from_le_bytes(bytes),
            Self::Big => u16::from_be_bytes(bytes),
        }
    }

    fn u32_from(self, bytes: [u8; 4]) -> u32 {
        match self {
            Self::Little => u32::from_le_bytes(bytes),
            Self::Big => u32::from_be_bytes(bytes),
        }
    }

    fn u16_bytes(self, value: u16) -> [u8; 2] {
        match self {
            Self::Little => value.to_le_bytes(),
            Self::Big => value.to_be_bytes(),
        }
    }

    fn u32_bytes(self, value: u32) -> [u8; 4] {
        match self {
            Self::Little => value.to_le_bytes(),
            Self::Big => value.to_be_bytes(),
        }
    }
}

/// Raised when a PiPL resource is malformed.
#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct PiPLError(pub String);

/// How a filter wants one of the seven filter cases handled.
///
/// Serialises as a four-element list, like the Python host's `to_list()`.
/// Deserialisation accepts either the list or an object with named fields.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FilterCaseInfo {
    pub input_handling: u8,
    pub output_handling: u8,
    pub flags1: u8,
    pub flags2: u8,
}

impl FilterCaseInfo {
    /// Returns true unless the filter refuses this case outright.
    pub fn can_filter(&self) -> bool {
        self.input_handling != c::HANDLING_CANT_FILTER
    }

    /// The four values in resource order.
    pub fn to_list(&self) -> [u8; 4] {
        [
            self.input_handling,
            self.output_handling,
            self.flags1,
            self.flags2,
        ]
    }

    fn from_list(values: [u8; 4]) -> Self {
        let [input_handling, output_handling, flags1, flags2] = values;
        Self {
            input_handling,
            output_handling,
            flags1,
            flags2,
        }
    }
}

impl Serialize for FilterCaseInfo {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.to_list().serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for FilterCaseInfo {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        struct Named {
            input_handling: u8,
            output_handling: u8,
            flags1: u8,
            flags2: u8,
        }

        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Repr {
            List([u8; 4]),
            Named(Named),
        }

        Ok(match Repr::deserialize(deserializer)? {
            Repr::List(values) => Self::from_list(values),
            Repr::Named(named) => Self {
                input_handling: named.input_handling,
                output_handling: named.output_handling,
                flags1: named.flags1,
                flags2: named.flags2,
            },
        })
    }
}

/// Serialises four-character codes as text, like the Python host.
mod fourcc_serde {
    use serde::{de::Error, Deserialize, Deserializer, Serializer};

    use crate::host::constants as c;

    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Repr {
        Text(String),
        Number(u32),
    }

    fn decode<E: Error>(repr: Repr) -> Result<u32, E> {
        match repr {
            Repr::Number(value) => Ok(value),
            Repr::Text(text) => c::parse_fourcc(&text)
                .ok_or_else(|| E::custom(format!("invalid four-character code {text:?}"))),
        }
    }

    pub fn serialize<S: Serializer>(value: &u32, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&c::fourcc_str(*value))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<u32, D::Error> {
        decode(Repr::deserialize(deserializer)?)
    }

    pub mod option {
        use serde::{Deserialize, Deserializer, Serializer};

        use crate::host::constants as c;

        pub fn serialize<S: Serializer>(
            value: &Option<u32>,
            serializer: S,
        ) -> Result<S::Ok, S::Error> {
            match value {
                Some(code) => serializer.serialize_str(&c::fourcc_str(*code)),
                None => serializer.serialize_none(),
            }
        }

        pub fn deserialize<'de, D: Deserializer<'de>>(
            deserializer: D,
        ) -> Result<Option<u32>, D::Error> {
            match Option::<super::Repr>::deserialize(deserializer)? {
                None => Ok(None),
                // The Python host treats an empty code as "no requirement".
                Some(super::Repr::Text(text)) if text.is_empty() => Ok(None),
                Some(repr) => super::decode(repr).map(Some),
            }
        }
    }
}

/// The interesting properties of one PiPL resource.
///
/// Serialises with the same keys as the Python host's `to_dict()`; `kind`
/// and `required_host` are four-character text.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PiPL {
    /// The plug-in kind, such as `8BFM` for filters.
    #[serde(with = "fourcc_serde")]
    pub kind: u32,
    /// The menu name.
    pub name: String,
    /// The menu category.
    pub category: String,
    /// The plug-in version as (major, minor).
    pub version: (u16, u16),
    /// Entry point names keyed by architecture (see [`ENTRY_POINT_KEYS`]).
    pub entry_points: BTreeMap<String, String>,
    /// Supported image modes as a bit set; `None` when undeclared.
    pub image_modes: Option<u16>,
    /// Handling for the seven filter cases, when declared and well formed.
    pub filter_case_info: Option<Vec<FilterCaseInfo>>,
    /// The `enbl` expression deciding when the menu item is enabled.
    pub enable_info: String,
    /// The host signature the plug-in requires, if any.
    #[serde(with = "fourcc_serde::option")]
    pub required_host: Option<u32>,
    /// True when the plug-in declares scripting terminology (`hstm`).
    pub has_terminology: bool,
    /// True when the plug-in has no About box.
    pub no_about_box: bool,
    /// Load priority.
    pub priority: i32,
    /// The resource id the PiPL was read from.
    pub resource_id: Option<i32>,
}

impl PiPL {
    /// Returns true when the plug-in is a filter.
    pub fn is_filter(&self) -> bool {
        self.kind == c::FILTER_KIND
    }

    /// Returns true for `**Hidden**` helpers invoked by scripting plug-ins.
    pub fn is_hidden(&self) -> bool {
        self.category.starts_with(HIDDEN_CATEGORY_PREFIX)
    }

    /// Returns true when the filter declares support for an image mode.
    ///
    /// Filters without a `mode` property are assumed to support RGB and
    /// grayscale, matching Photoshop's behaviour for old plug-ins.
    pub fn supports_mode(&self, mode: i16) -> bool {
        let Some(modes) = self.image_modes else {
            return mode == c::MODE_RGB || mode == c::MODE_GRAYSCALE;
        };
        if !(0..MODE_BIT_COUNT).contains(&mode) {
            return false;
        }
        modes & (MODE_BIT_ZERO >> mode) != 0
    }

    /// Returns the handling for a filter case (1-based), if declared.
    pub fn case_info(&self, filter_case: i16) -> Option<&FilterCaseInfo> {
        let index = usize::try_from(filter_case).ok()?.checked_sub(1)?;
        self.filter_case_info.as_ref()?.get(index)
    }
}

/// Decodes text as UTF-8, falling back to Latin-1.
///
/// Classic plug-ins use MacRoman or Windows-1252; Latin-1 never fails and
/// keeps ASCII intact.
fn decode(raw: &[u8]) -> String {
    match std::str::from_utf8(raw) {
        Ok(text) => text.to_owned(),
        Err(_) => raw.iter().map(|&b| char::from(b)).collect(),
    }
}

fn pascal_string(data: &[u8]) -> String {
    let Some((&length, rest)) = data.split_first() else {
        return String::new();
    };
    let length = usize::from(length).min(rest.len());
    decode(rest.get(..length).unwrap_or_default())
}

fn c_string(data: &[u8]) -> String {
    let end = data.iter().position(|&b| b == 0).unwrap_or(data.len());
    decode(data.get(..end).unwrap_or_default())
}

/// Decodes a code descriptor's entry point name.
///
/// The SDK stores entry names as C strings. A few Mac plug-ins built with
/// old tools store a Pascal string instead, so accept both.
fn entry_name(data: &[u8]) -> String {
    if let Some((&length, rest)) = data.split_first() {
        if length > 0 && length < PASCAL_ENTRY_MAX_LENGTH {
            if let Some(candidate) = rest.get(..usize::from(length)) {
                if candidate.iter().all(|&b| b > b' ' && b < 0x7F) {
                    return decode(candidate);
                }
            }
        }
    }
    c_string(data).trim().to_owned()
}

/// Parses the `fici` property: seven 4-byte `FilterCaseInfo` records.
///
/// Some plug-ins (Alf's Power Toys, for one) store mis-escaped values such
/// as the text `/x01` instead of the byte 0x01. Those are decoded too.
fn parse_filter_case_info(data: &[u8]) -> Option<Vec<FilterCaseInfo>> {
    let wanted = FILTER_CASE_COUNT * FILTER_CASE_INFO_SIZE;
    let mut values = Vec::with_capacity(wanted);
    let mut rest = data;
    while values.len() < wanted {
        if let Some(after) = rest.strip_prefix(MISESCAPE_PREFIX) {
            let digits = after.iter().take_while(|b| b.is_ascii_hexdigit()).count();
            // Only the low byte matters, so the sum can wrap freely.
            let value = after.iter().take(digits).fold(0u8, |acc, &b| {
                let digit = char::from(b).to_digit(16).unwrap_or(0) as u8;
                acc.wrapping_mul(16).wrapping_add(digit)
            });
            values.push(value);
            rest = after.get(digits..).unwrap_or_default();
            continue;
        }
        let (&value, after) = rest.split_first()?;
        values.push(value);
        rest = after;
    }
    let records = values
        .chunks_exact(FILTER_CASE_INFO_SIZE)
        .map(|chunk| FilterCaseInfo::from_list([chunk[0], chunk[1], chunk[2], chunk[3]]))
        .collect();
    Some(records)
}

fn read_array<const N: usize>(data: &[u8], pos: usize) -> Option<[u8; N]> {
    data.get(pos..pos.checked_add(N)?)?.try_into().ok()
}

fn read_u32(data: &[u8], pos: usize, order: ByteOrder) -> Option<u32> {
    read_array(data, pos).map(|bytes| order.u32_from(bytes))
}

/// One raw property from a PiPL resource.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RawProperty<'a> {
    /// The vendor code, `8BIM` for Photoshop properties.
    pub vendor: u32,
    /// The property key, such as `name` or `8664`.
    pub key: u32,
    /// The property id.
    pub id: i32,
    /// The property value without padding.
    pub value: &'a [u8],
}

/// Splits a PiPL resource into its raw properties, in resource order.
///
/// # Errors
///
/// Returns [`PiPLError`] when the header is wrong, the property count is
/// implausible, or a property runs past the end of the data.
pub fn parse_properties(data: &[u8], order: ByteOrder) -> Result<Vec<RawProperty<'_>>, PiPLError> {
    let too_short = || PiPLError("PiPL resource is too short".into());
    if data.len() < HEADER_SIZE {
        return Err(too_short());
    }
    let signature = read_array(data, 0)
        .map(|bytes| order.u16_from(bytes) as i16)
        .ok_or_else(too_short)?;
    let version = read_u32(data, 2, order).ok_or_else(too_short)? as i32;
    let count = read_u32(data, 6, order).ok_or_else(too_short)? as i32;
    if signature != PIPL_SIGNATURE || version != PIPL_VERSION {
        return Err(PiPLError(format!(
            "unsupported PiPL header (signature {signature}, version {version})"
        )));
    }
    if !(0..=MAX_PROPERTY_COUNT).contains(&count) {
        return Err(PiPLError(format!("invalid PiPL property count {count}")));
    }

    let truncated = || PiPLError("PiPL property list is truncated".into());
    let overrun = || PiPLError("PiPL property overruns the resource".into());
    let mut properties = Vec::new();
    let mut pos = HEADER_SIZE;
    for _ in 0..count {
        let Some(header) = data
            .get(pos..)
            .filter(|rest| rest.len() >= PROPERTY_HEADER_SIZE)
        else {
            return Err(truncated());
        };
        let vendor = read_u32(header, 0, order).ok_or_else(truncated)?;
        let key = read_u32(header, 4, order).ok_or_else(truncated)?;
        let id = read_u32(header, 8, order).ok_or_else(truncated)? as i32;
        let length = read_u32(header, 12, order).ok_or_else(truncated)? as i32;
        pos += PROPERTY_HEADER_SIZE;
        let length = usize::try_from(length).map_err(|_| overrun())?;
        let end = pos.checked_add(length).ok_or_else(overrun)?;
        let value = data.get(pos..end).ok_or_else(overrun)?;
        properties.push(RawProperty {
            vendor,
            key,
            id,
            value,
        });
        pos = end.saturating_add(padding(length));
    }
    Ok(properties)
}

/// The number of zero bytes that pad a value of `length` bytes.
fn padding(length: usize) -> usize {
    (PROPERTY_ALIGNMENT - length % PROPERTY_ALIGNMENT) % PROPERTY_ALIGNMENT
}

/// Parses a PiPL resource.
///
/// `order` is [`ByteOrder::Little`] for Windows resources and
/// [`ByteOrder::Big`] for Mac resources. Properties from vendors other than
/// Photoshop are ignored.
///
/// # Errors
///
/// Returns [`PiPLError`] when the resource is malformed (see
/// [`parse_properties`]).
pub fn parse_pipl(
    data: &[u8],
    order: ByteOrder,
    resource_id: Option<i32>,
) -> Result<PiPL, PiPLError> {
    let mut result = PiPL {
        resource_id,
        ..PiPL::default()
    };
    for property in parse_properties(data, order)? {
        if property.vendor != c::PHOTOSHOP_SIGNATURE {
            continue;
        }
        apply_property(&mut result, property.key, property.value, order);
    }
    Ok(result)
}

fn apply_property(result: &mut PiPL, key: u32, value: &[u8], order: ByteOrder) {
    let word = read_u32(value, 0, order);
    match key {
        c::PIPL_KIND => {
            if let Some(kind) = word {
                result.kind = kind;
            }
        }
        c::PIPL_NAME => result.name = pascal_string(value).trim().to_owned(),
        c::PIPL_CATEGORY => result.category = pascal_string(value).trim().to_owned(),
        c::PIPL_VERSION => {
            if let Some(packed) = word {
                result.version = ((packed >> 16) as u16, (packed & 0xFFFF) as u16);
            }
        }
        c::PIPL_IMAGE_MODES => {
            // Mode flags are two bytes with bit 0x80 of the first byte for mode 0,
            // independent of the resource's byte order.
            if let Some(bytes) = read_array::<2>(value, 0) {
                result.image_modes = Some(u16::from_be_bytes(bytes));
            }
        }
        c::PIPL_FILTER_CASE_INFO => result.filter_case_info = parse_filter_case_info(value),
        c::PIPL_ENABLE_INFO => result.enable_info = c_string(value),
        c::PIPL_REQUIRED_HOST => {
            if let Some(host) = word {
                result.required_host = Some(host);
            }
        }
        c::PIPL_HAS_TERMINOLOGY => result.has_terminology = true,
        c::PIPL_NO_ABOUT_BOX => result.no_about_box = true,
        c::PIPL_PRIORITY => {
            if let Some(priority) = word {
                result.priority = priority as i32;
            }
        }
        _ => {
            let Some(arch) = entry_point_arch(key) else {
                return;
            };
            let name = entry_name(value);
            if !name.is_empty() {
                result.entry_points.insert(arch.to_owned(), name);
            }
        }
    }
}

/// Serialises a PiPL resource from `(key, value)` pairs.
///
/// Every property gets the Photoshop vendor code. Used by the tests and
/// handy for writing fixtures by hand.
pub fn build_pipl(properties: &[(u32, Vec<u8>)], order: ByteOrder) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&order.u16_bytes(PIPL_SIGNATURE as u16));
    out.extend_from_slice(&order.u32_bytes(PIPL_VERSION as u32));
    out.extend_from_slice(&order.u32_bytes(properties.len() as u32));
    for (key, value) in properties {
        out.extend_from_slice(&order.u32_bytes(c::PHOTOSHOP_SIGNATURE));
        out.extend_from_slice(&order.u32_bytes(*key));
        out.extend_from_slice(&order.u32_bytes(0));
        out.extend_from_slice(&order.u32_bytes(value.len() as u32));
        out.extend_from_slice(value);
        out.resize(out.len() + padding(value.len()), 0);
    }
    out
}

/// Encodes text as a Pascal string (length byte then Latin-1 bytes).
///
/// Characters outside Latin-1 become `?` and the text is cut at 255 bytes.
pub fn pascal(text: &str) -> Vec<u8> {
    let raw: Vec<u8> = text
        .chars()
        .map(|ch| u8::try_from(u32::from(ch)).unwrap_or(b'?'))
        .take(PASCAL_MAX_LENGTH)
        .collect();
    let mut out = Vec::with_capacity(raw.len() + 1);
    out.push(raw.len() as u8);
    out.extend_from_slice(&raw);
    out
}

/// Encodes a 32-bit integer property value.
pub fn int_property(value: i32, order: ByteOrder) -> Vec<u8> {
    order.u32_bytes(value as u32).to_vec()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host::constants::fourcc;

    const ORDERS: [ByteOrder; 2] = [ByteOrder::Little, ByteOrder::Big];

    fn sample_properties(order: ByteOrder) -> Vec<(u32, Vec<u8>)> {
        let mut fici = Vec::new();
        for _ in 0..3 {
            fici.extend_from_slice(&[1, 1, 0, 0]);
        }
        fici.extend_from_slice(&[0, 0, 0, 0]);
        for _ in 0..3 {
            fici.extend_from_slice(&[2, 9, 8, 0]);
        }
        vec![
            (fourcc(b"kind"), int_property(c::FILTER_KIND as i32, order)),
            (fourcc(b"vers"), int_property(4 << 16, order)),
            (fourcc(b"name"), pascal("Sharpen Plus")),
            (fourcc(b"catg"), pascal("Durable")),
            (fourcc(b"8664"), b"PluginMain\0".to_vec()),
            (fourcc(b"wx86"), b"PluginMain32\0".to_vec()),
            (fourcc(b"mi64"), b"MacMain\0".to_vec()),
            (fourcc(b"mode"), vec![0x50, 0x30]),
            (fourcc(b"fici"), fici),
            (fourcc(b"enbl"), b"in (PSHOP_ImageMode, RGBMode)\0".to_vec()),
            (fourcc(b"prty"), int_property(7, order)),
        ]
    }

    fn parse_le(data: &[u8]) -> Result<PiPL, PiPLError> {
        parse_pipl(data, ByteOrder::Little, None)
    }

    fn build_le(properties: &[(u32, Vec<u8>)]) -> Vec<u8> {
        build_pipl(properties, ByteOrder::Little)
    }

    #[test]
    fn round_trip_in_both_byte_orders() {
        for order in ORDERS {
            let pipl = parse_pipl(
                &build_pipl(&sample_properties(order), order),
                order,
                Some(16000),
            )
            .unwrap();
            assert!(pipl.is_filter());
            assert_eq!(pipl.name, "Sharpen Plus");
            assert_eq!(pipl.category, "Durable");
            assert_eq!(pipl.version, (4, 0));
            let expected: BTreeMap<String, String> = [
                ("win-x64", "PluginMain"),
                ("win-x86", "PluginMain32"),
                ("mac-x64", "MacMain"),
            ]
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
            assert_eq!(pipl.entry_points, expected);
            assert_eq!(pipl.priority, 7);
            assert_eq!(pipl.resource_id, Some(16000));
            assert!(pipl.enable_info.starts_with("in (PSHOP"));
            assert!(pipl
                .case_info(c::FILTER_CASE_FLAT_NO_SELECTION)
                .unwrap()
                .can_filter());
            assert!(!pipl
                .case_info(c::FILTER_CASE_EDITABLE_TRANSPARENCY_NO_SELECTION)
                .unwrap()
                .can_filter());
            assert_eq!(
                pipl.case_info(5).unwrap().output_handling,
                c::HANDLING_FILL_MASK
            );
        }
    }

    #[test]
    fn image_modes() {
        let pipl = parse_le(&build_le(&sample_properties(ByteOrder::Little))).unwrap();
        assert!(pipl.supports_mode(c::MODE_RGB));
        assert!(pipl.supports_mode(c::MODE_GRAYSCALE));
        assert!(pipl.supports_mode(c::MODE_GRAY16));
        assert!(pipl.supports_mode(c::MODE_RGB48));
        assert!(!pipl.supports_mode(c::MODE_CMYK));
        assert!(!pipl.supports_mode(c::MODE_LAB48));
    }

    #[test]
    fn out_of_range_modes_are_unsupported() {
        let pipl = PiPL {
            image_modes: Some(0xFFFF),
            ..PiPL::default()
        };
        assert!(pipl.supports_mode(0));
        assert!(pipl.supports_mode(15));
        assert!(!pipl.supports_mode(16));
        assert!(!pipl.supports_mode(-1));
        assert!(!pipl.supports_mode(i16::MAX));
    }

    #[test]
    fn missing_modes_default_to_rgb_and_gray() {
        let pipl = PiPL::default();
        assert!(pipl.supports_mode(c::MODE_RGB));
        assert!(pipl.supports_mode(c::MODE_GRAYSCALE));
        assert!(!pipl.supports_mode(c::MODE_RGB48));
    }

    #[test]
    fn case_info_bounds() {
        let pipl = parse_le(&build_le(&sample_properties(ByteOrder::Little))).unwrap();
        assert!(pipl.case_info(0).is_none());
        assert!(pipl.case_info(-1).is_none());
        assert!(pipl.case_info(7).is_some());
        assert!(pipl.case_info(8).is_none());
        assert!(PiPL::default().case_info(1).is_none());
    }

    #[test]
    fn misescaped_filter_case_info() {
        let raw = b"/x01/x01\x00\x00".repeat(7);
        let pipl = parse_le(&build_le(&[
            (
                fourcc(b"kind"),
                int_property(c::FILTER_KIND as i32, ByteOrder::Little),
            ),
            (fourcc(b"fici"), raw),
        ]))
        .unwrap();
        let lists: Vec<[u8; 4]> = pipl
            .filter_case_info
            .unwrap()
            .iter()
            .map(FilterCaseInfo::to_list)
            .collect();
        assert_eq!(lists, vec![[1, 1, 0, 0]; 7]);
    }

    #[test]
    fn misescape_edge_cases() {
        // A bare "/x" is 0, long digit runs keep the low byte.
        let mut raw = b"/x/x1ff".to_vec();
        raw.extend_from_slice(&[0; 26]);
        let info = parse_filter_case_info(&raw).unwrap();
        assert_eq!(info[0].to_list(), [0, 0xFF, 0, 0]);
    }

    #[test]
    fn short_filter_case_info_is_ignored() {
        let pipl = parse_le(&build_le(&[(fourcc(b"fici"), vec![1; 27])])).unwrap();
        assert!(pipl.filter_case_info.is_none());
    }

    #[test]
    fn pascal_entry_point_names() {
        let pipl = parse_le(&build_le(&[(
            fourcc(b"ma64"),
            b"\x08MainProc\0\0\0".to_vec(),
        )]))
        .unwrap();
        let expected: BTreeMap<String, String> =
            [("mac-arm64".to_string(), "MainProc".to_string())].into();
        assert_eq!(pipl.entry_points, expected);
    }

    #[test]
    fn empty_entry_point_names_are_skipped() {
        let pipl = parse_le(&build_le(&[
            (fourcc(b"8664"), b"\0".to_vec()),
            (fourcc(b"wx86"), Vec::new()),
        ]))
        .unwrap();
        assert!(pipl.entry_points.is_empty());
    }

    #[test]
    fn foreign_vendor_properties_are_ignored() {
        let mut data = build_le(&[(fourcc(b"name"), pascal("Mine"))]);
        data[10..14].copy_from_slice(&int_property(fourcc(b"ABCD") as i32, ByteOrder::Little));
        assert_eq!(parse_le(&data).unwrap().name, "");
    }

    #[test]
    fn hidden_category() {
        let pipl = parse_le(&build_le(&[(fourcc(b"catg"), pascal("**Hidden**"))])).unwrap();
        assert!(pipl.is_hidden());
        assert!(!PiPL::default().is_hidden());
    }

    #[test]
    fn flags_and_required_host() {
        let pipl = parse_le(&build_le(&[
            (
                fourcc(b"host"),
                int_property(fourcc(b"8BIM") as i32, ByteOrder::Little),
            ),
            (fourcc(b"nabo"), Vec::new()),
            (fourcc(b"hstm"), vec![0; 8]),
        ]))
        .unwrap();
        assert_eq!(pipl.required_host, Some(c::PHOTOSHOP_SIGNATURE));
        assert!(pipl.no_about_box);
        assert!(pipl.has_terminology);
    }

    #[test]
    fn short_values_are_ignored() {
        let pipl = parse_le(&build_le(&[
            (fourcc(b"kind"), vec![1, 2]),
            (fourcc(b"mode"), vec![0x80]),
            (fourcc(b"prty"), vec![1]),
            (fourcc(b"name"), Vec::new()),
        ]))
        .unwrap();
        assert_eq!(pipl.kind, 0);
        assert!(pipl.image_modes.is_none());
        assert_eq!(pipl.priority, 0);
        assert_eq!(pipl.name, "");
    }

    #[test]
    fn text_decoding() {
        // An overlong Pascal length is clamped; invalid UTF-8 falls back to Latin-1.
        let pipl = parse_le(&build_le(&[
            (fourcc(b"name"), vec![200, b'A', 0xE9]),
            (fourcc(b"catg"), b"\x09  Caf\xc3\xa9  ".to_vec()),
        ]))
        .unwrap();
        assert_eq!(pipl.name, "A\u{e9}");
        assert_eq!(pipl.category, "Caf\u{e9}");
    }

    #[test]
    fn pascal_encoding() {
        assert_eq!(pascal("ab"), vec![2, b'a', b'b']);
        assert_eq!(pascal("\u{e9}\u{263a}"), vec![2, 0xE9, b'?']);
        assert_eq!(pascal(&"x".repeat(300)).len(), 256);
    }

    #[test]
    fn dict_round_trip() {
        let pipl = parse_pipl(
            &build_le(&sample_properties(ByteOrder::Little)),
            ByteOrder::Little,
            Some(3),
        )
        .unwrap();
        let json = serde_json::to_value(&pipl).unwrap();
        assert_eq!(json["kind"], "8BFM");
        assert_eq!(json["version"], serde_json::json!([4, 0]));
        assert_eq!(json["filter_case_info"][0], serde_json::json!([1, 1, 0, 0]));
        assert_eq!(json["required_host"], serde_json::Value::Null);
        let back: PiPL = serde_json::from_value(json).unwrap();
        assert_eq!(back, pipl);
    }

    #[test]
    fn deserialises_python_dicts() {
        let json = serde_json::json!({
          "kind": "8BFM",
          "name": "X",
          "required_host": "8BIM",
          "filter_case_info": [{"input_handling": 1, "output_handling": 2, "flags1": 3, "flags2": 4}],
        });
        let pipl: PiPL = serde_json::from_value(json).unwrap();
        assert!(pipl.is_filter());
        assert_eq!(pipl.required_host, Some(c::PHOTOSHOP_SIGNATURE));
        assert_eq!(pipl.filter_case_info.unwrap()[0].to_list(), [1, 2, 3, 4]);
        assert!(serde_json::from_value::<PiPL>(serde_json::json!({"kind": "toolong"})).is_err());
    }

    fn overrun() -> Vec<u8> {
        let mut data = build_le(&[(fourcc(b"name"), pascal("x"))]);
        data[22..26].copy_from_slice(&int_property(100, ByteOrder::Little));
        data
    }

    #[test]
    fn malformed_resources() {
        let mut bad_signature = vec![2, 0];
        bad_signature.extend_from_slice(&[0; 8]);
        let mut bad_version = build_le(&[]);
        bad_version[2] = 1;
        let mut negative_count = build_le(&[]);
        negative_count[6..10].copy_from_slice(&(-1i32).to_le_bytes());
        let mut huge_count = build_le(&[]);
        huge_count[6..10].copy_from_slice(&10_001i32.to_le_bytes());
        let mut truncated = build_le(&[(fourcc(b"name"), pascal("x"))]);
        truncated.truncate(20);
        let mut extra_count = build_le(&[(fourcc(b"name"), pascal("x"))]);
        extra_count[6] = 2;
        let mut negative_length = build_le(&[(fourcc(b"name"), pascal("x"))]);
        negative_length[22..26].copy_from_slice(&(-4i32).to_le_bytes());
        let mut max_length = build_le(&[(fourcc(b"name"), pascal("x"))]);
        max_length[22..26].copy_from_slice(&i32::MAX.to_le_bytes());
        let cases: Vec<(&str, Vec<u8>)> = vec![
            ("empty", Vec::new()),
            ("nine bytes", vec![1, 0, 0, 0, 0, 0, 0, 0, 0]),
            ("bad signature", bad_signature),
            ("bad version", bad_version),
            ("negative count", negative_count),
            ("huge count", huge_count),
            ("truncated", truncated),
            ("extra count", extra_count),
            ("negative length", negative_length),
            ("max length", max_length),
            ("overrun", overrun()),
        ];
        for (label, data) in cases {
            assert!(parse_le(&data).is_err(), "{label} should fail");
            assert!(
                parse_pipl(&data, ByteOrder::Big, None).is_err(),
                "{label} (big) should fail"
            );
        }
    }

    #[test]
    fn boundary_resources() {
        // Header only, and a final property whose padding runs past the end.
        assert_eq!(parse_le(&build_le(&[])).unwrap(), PiPL::default());
        let mut data = build_le(&[(fourcc(b"name"), pascal("x"))]);
        data.truncate(data.len() - 2);
        assert_eq!(parse_le(&data).unwrap().name, "x");
    }

    #[test]
    fn raw_properties_keep_order() {
        let data = build_le(&sample_properties(ByteOrder::Little));
        let properties = parse_properties(&data, ByteOrder::Little).unwrap();
        assert_eq!(properties.len(), 11);
        assert_eq!(properties[4].key, fourcc(b"8664"));
        assert_eq!(properties[4].value, b"PluginMain\0");
        assert_eq!(entry_point_arch(fourcc(b"wa64")), Some("win-arm64"));
        assert_eq!(entry_point_arch(fourcc(b"name")), None);
    }
}
