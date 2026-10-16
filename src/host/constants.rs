//! Numeric constants from the Photoshop plug-in API.
//!
//! Values come from the public Photoshop SDK headers (`PIFilter.h`,
//! `PIGeneral.h`, `PIActions.h`, `SPErrorCodes.h`). Only the constants the
//! host uses are listed.

/// Returns the 32-bit integer for a four-character code such as `8BIM`.
///
/// # Examples
///
/// ```
/// use eightbf_host::host::constants::fourcc;
/// assert_eq!(fourcc(b"8BIM"), 0x3842_494D);
/// ```
pub const fn fourcc(code: &[u8; 4]) -> u32 {
    u32::from_be_bytes(*code)
}

/// Returns the four-character text for a 32-bit code.
///
/// # Examples
///
/// ```
/// use eightbf_host::host::constants::fourcc_str;
/// assert_eq!(fourcc_str(0x3842_494D), "8BIM");
/// ```
pub fn fourcc_str(value: u32) -> String {
    value.to_be_bytes().iter().map(|&b| b as char).collect()
}

/// Parses four characters of Latin-1 text into a code.
///
/// Returns `None` unless the text is exactly four Latin-1 characters.
///
/// # Examples
///
/// ```
/// use eightbf_host::host::constants::parse_fourcc;
/// assert_eq!(parse_fourcc("8BIM"), Some(0x3842_494D));
/// assert_eq!(parse_fourcc("toolong"), None);
/// ```
pub fn parse_fourcc(text: &str) -> Option<u32> {
    let mut bytes = [0u8; 4];
    let mut count = 0;
    for ch in text.chars() {
        let code = u32::from(ch);
        if count == 4 || code > 0xFF {
            return None;
        }
        bytes[count] = code as u8;
        count += 1;
    }
    (count == 4).then(|| u32::from_be_bytes(bytes))
}

// Filter selectors.
pub const SELECTOR_ABOUT: i16 = 0;
pub const SELECTOR_PARAMETERS: i16 = 1;
pub const SELECTOR_PREPARE: i16 = 2;
pub const SELECTOR_START: i16 = 3;
pub const SELECTOR_CONTINUE: i16 = 4;
pub const SELECTOR_FINISH: i16 = 5;

// Error codes (OSErr).
pub const NO_ERR: i16 = 0;
pub const USER_CANCELED_ERR: i16 = -128;
pub const MEM_FULL_ERR: i16 = -108;
pub const NIL_HANDLE_ERR: i16 = -109;
pub const PARAM_ERR: i16 = -50;
pub const READ_ERR: i16 = -19;
pub const WRITE_ERR: i16 = -20;
pub const FILTER_BAD_PARAMETERS: i16 = -30100;
pub const FILTER_BAD_MODE: i16 = -30101;
pub const ERR_PLUGIN_HOST_INSUFFICIENT: i16 = -30900;
pub const ERR_PLUGIN_PROPERTY_UNDEFINED: i16 = -30901;
pub const ERR_HOST_DOES_NOT_SUPPORT_COLOR_STUFF: i16 = -30902;
pub const ERR_INVALID_SAMPLE_POINT: i16 = -30903;
pub const ERR_REPORT_STRING: i16 = -30904;
pub const ERR_UNKNOWN_PORT: i16 = -30905;
pub const ERR_UNSUPPORTED_COLOR_CONVERSION: i16 = -30906;

/// Returns a readable message for a Photoshop error code, if it has one.
pub fn error_message(code: i16) -> Option<&'static str> {
    Some(match code {
        USER_CANCELED_ERR => "The operation was cancelled.",
        MEM_FULL_ERR => "Not enough memory.",
        NIL_HANDLE_ERR => "The plug-in used an invalid memory handle.",
        PARAM_ERR => "The plug-in received an invalid parameter.",
        READ_ERR => "A read error occurred.",
        WRITE_ERR => "A write error occurred.",
        FILTER_BAD_PARAMETERS => "The filter rejected its parameters.",
        FILTER_BAD_MODE => "The filter does not support this image mode.",
        ERR_PLUGIN_HOST_INSUFFICIENT => "The filter requires features this host does not provide.",
        ERR_PLUGIN_PROPERTY_UNDEFINED => "The filter asked for an undefined property.",
        _ => return None,
    })
}

// PICA (Sweet Pea) error codes are four-character codes.
pub const SP_NO_ERROR: i32 = 0;
pub const SP_SUITE_NOT_FOUND: i32 = fourcc(b"S!Fd") as i32;
pub const SP_UNIMPLEMENTED: i32 = fourcc(b"!IMP") as i32;
pub const SP_OUT_OF_MEMORY: i32 = 0x0000_006D;
pub const SP_BAD_PARAMETER: i32 = fourcc(b"Parm") as i32;

// Image modes.
pub const MODE_BITMAP: i16 = 0;
pub const MODE_GRAYSCALE: i16 = 1;
pub const MODE_INDEXED: i16 = 2;
pub const MODE_RGB: i16 = 3;
pub const MODE_CMYK: i16 = 4;
pub const MODE_MULTICHANNEL: i16 = 7;
pub const MODE_LAB: i16 = 9;
pub const MODE_GRAY16: i16 = 10;
pub const MODE_RGB48: i16 = 11;
pub const MODE_LAB48: i16 = 12;
pub const MODE_CMYK64: i16 = 13;

// Filter cases.
pub const FILTER_CASE_UNSUPPORTED: i16 = -1;
pub const FILTER_CASE_FLAT_NO_SELECTION: i16 = 1;
pub const FILTER_CASE_FLAT_WITH_SELECTION: i16 = 2;
pub const FILTER_CASE_FLOATING_SELECTION: i16 = 3;
pub const FILTER_CASE_EDITABLE_TRANSPARENCY_NO_SELECTION: i16 = 4;
pub const FILTER_CASE_EDITABLE_TRANSPARENCY_WITH_SELECTION: i16 = 5;
pub const FILTER_CASE_PROTECTED_TRANSPARENCY_NO_SELECTION: i16 = 6;
pub const FILTER_CASE_PROTECTED_TRANSPARENCY_WITH_SELECTION: i16 = 7;

// FilterCaseInfo data handling.
pub const HANDLING_CANT_FILTER: u8 = 0;
pub const HANDLING_NONE: u8 = 1;
pub const HANDLING_BLACK_MAT: u8 = 2;
pub const HANDLING_GRAY_MAT: u8 = 3;
pub const HANDLING_WHITE_MAT: u8 = 4;
pub const HANDLING_DEFRINGE: u8 = 5;
pub const HANDLING_BLACK_ZAP: u8 = 6;
pub const HANDLING_GRAY_ZAP: u8 = 7;
pub const HANDLING_WHITE_ZAP: u8 = 8;
pub const HANDLING_FILL_MASK: u8 = 9;
pub const HANDLING_BACKGROUND_ZAP: u8 = 10;
pub const HANDLING_FOREGROUND_ZAP: u8 = 11;

// FilterCaseInfo flags1 bits.
pub const CASE_FLAG_DONT_COPY_TO_DESTINATION: u8 = 1 << 0;
pub const CASE_FLAG_WORKS_WITH_BLANK_DATA: u8 = 1 << 1;
pub const CASE_FLAG_FILTERS_LAYER_MASK: u8 = 1 << 2;
pub const CASE_FLAG_WRITES_OUTSIDE_SELECTION: u8 = 1 << 3;

// Padding values for inputPadding, outputPadding and maskPadding.
pub const PADDING_EDGE_REPLICATION: i16 = -1;
pub const PADDING_NONE: i16 = -2;
pub const PADDING_ERROR_ON_BOUNDS: i16 = -3;

// samplingSupport.
pub const SAMPLING_NONE: i8 = 0;
pub const SAMPLING_INTEGRAL: i8 = 1;

// Descriptor play and record info.
pub const PLAY_DIALOG_DONT_DISPLAY: i16 = 0;
pub const PLAY_DIALOG_DISPLAY: i16 = 1;
pub const PLAY_DIALOG_SILENT: i16 = 2;
pub const RECORD_DIALOG_OPTIONAL: i16 = 0;
pub const RECORD_DIALOG_REQUIRED: i16 = 1;
pub const RECORD_DIALOG_NONE: i16 = 2;

// Descriptor value types.
pub const TYPE_ALIAS: u32 = fourcc(b"alis");
pub const TYPE_BOOLEAN: u32 = fourcc(b"bool");
pub const TYPE_CHAR: u32 = fourcc(b"TEXT");
pub const TYPE_CLASS: u32 = fourcc(b"type");
pub const TYPE_ENUMERATED: u32 = fourcc(b"enum");
pub const TYPE_FLOAT: u32 = fourcc(b"doub");
pub const TYPE_INTEGER: u32 = fourcc(b"long");
pub const TYPE_NULL: u32 = fourcc(b"null");
pub const TYPE_OBJECT: u32 = fourcc(b"Objc");
pub const TYPE_UNIT_FLOAT: u32 = fourcc(b"UntF");
pub const TYPE_GLOBAL_CLASS: u32 = fourcc(b"GlbC");
pub const TYPE_GLOBAL_OBJECT: u32 = fourcc(b"GlbO");
pub const TYPE_PATH: u32 = fourcc(b"Pat ");

// Signatures and PiPL keys.
pub const PHOTOSHOP_SIGNATURE: u32 = fourcc(b"8BIM");
pub const ANY_HOST_SIGNATURE: u32 = fourcc(b"    ");
pub const FILTER_KIND: u32 = fourcc(b"8BFM");
pub const PIPL_KIND: u32 = fourcc(b"kind");
pub const PIPL_VERSION: u32 = fourcc(b"vers");
pub const PIPL_PRIORITY: u32 = fourcc(b"prty");
pub const PIPL_IMAGE_MODES: u32 = fourcc(b"mode");
pub const PIPL_ENABLE_INFO: u32 = fourcc(b"enbl");
pub const PIPL_CATEGORY: u32 = fourcc(b"catg");
pub const PIPL_NAME: u32 = fourcc(b"name");
pub const PIPL_FILTER_CASE_INFO: u32 = fourcc(b"fici");
pub const PIPL_REQUIRED_HOST: u32 = fourcc(b"host");
pub const PIPL_HAS_TERMINOLOGY: u32 = fourcc(b"hstm");
pub const PIPL_NO_ABOUT_BOX: u32 = fourcc(b"nabo");
pub const PIPL_CODE_WIN32_X86: u32 = fourcc(b"wx86");
pub const PIPL_CODE_WIN64_X86: u32 = fourcc(b"8664");
pub const PIPL_CODE_WIN64_ARM: u32 = fourcc(b"wa64");
pub const PIPL_CODE_MAC_INTEL64: u32 = fourcc(b"mi64");
pub const PIPL_CODE_MAC_ARM64: u32 = fourcc(b"ma64");

/// The filter interface version this host implements (4.0).
pub const LATEST_FILTER_VERSION: i16 = 4;
pub const LATEST_FILTER_SUB_VERSION: i16 = 0;

// Property keys used by the property suite.
pub const PROP_NUMBER_OF_CHANNELS: u32 = fourcc(b"nuch");
pub const PROP_CHANNEL_NAME: u32 = fourcc(b"nmch");
pub const PROP_UNICODE_CHANNEL_NAME: u32 = fourcc(b"unch");
pub const PROP_IMAGE_MODE: u32 = fourcc(b"mode");
pub const PROP_NUMBER_OF_PATHS: u32 = fourcc(b"nupa");
pub const PROP_CAPTION: u32 = fourcc(b"capt");
pub const PROP_BIG_NUDGE_H: u32 = fourcc(b"bndH");
pub const PROP_BIG_NUDGE_V: u32 = fourcc(b"bndV");
pub const PROP_INTERPOLATION_METHOD: u32 = fourcc(b"intp");
pub const PROP_RULER_UNITS: u32 = fourcc(b"rulr");
pub const PROP_RULER_ORIGIN_H: u32 = fourcc(b"rorH");
pub const PROP_RULER_ORIGIN_V: u32 = fourcc(b"rorV");
pub const PROP_GRID_MAJOR: u32 = fourcc(b"grmj");
pub const PROP_GRID_MINOR: u32 = fourcc(b"grmn");
pub const PROP_SERIAL_STRING: u32 = fourcc(b"sstr");
pub const PROP_SERIAL_STRING2: u32 = fourcc(b"sstR");
pub const PROP_WATCH_SUSPENSION: u32 = fourcc(b"wtch");
pub const PROP_COPYRIGHT: u32 = fourcc(b"cpyr");
pub const PROP_COPYRIGHT2: u32 = fourcc(b"cpyR");
pub const PROP_WATERMARK: u32 = fourcc(b"watr");
pub const PROP_URL: u32 = fourcc(b"URL ");
pub const PROP_TITLE: u32 = fourcc(b"titl");
pub const PROP_DOCUMENT_WIDTH: u32 = fourcc(b"docW");
pub const PROP_DOCUMENT_HEIGHT: u32 = fourcc(b"docH");
pub const PROP_TOOL_TIPS: u32 = fourcc(b"tltp");
pub const PROP_EXIF_DATA: u32 = fourcc(b"EXIF");
pub const PROP_XMP: u32 = fourcc(b"xmpd");
pub const PROP_VERSION: u32 = fourcc(b"vers");
pub const PROP_PLAY_IN_PROGRESS: u32 = fourcc(b"plip");
pub const PROP_RECORD_IN_PROGRESS: u32 = fourcc(b"rcip");
pub const PROP_UNICODE_NAME: u32 = fourcc(b"unnm");
pub const PROP_UNICODE_NAME_WITHOUT_EXTENSION: u32 = fourcc(b"neun");
pub const PROP_NUMBER_OF_LAYERS: u32 = fourcc(b"nuly");
pub const PROP_TARGET_LAYER_INDEX: u32 = fourcc(b"tgly");
pub const PROP_LAYER_NAME: u32 = fourcc(b"lynm");
pub const PROP_UNICODE_LAYER_NAME: u32 = fourcc(b"lynu");
pub const PROP_DOCUMENT_ID: u32 = fourcc(b"dcID");
pub const PROP_HARDWARE_GAMMA_TABLE: u32 = fourcc(b"hgam");
pub const PROP_INTERFACE_COLOR: u32 = fourcc(b"iclr");
pub const PROP_APP_IS_MODAL: u32 = fourcc(b"aism");
pub const PROP_PIXEL_SCALE_FACTOR: u32 = fourcc(b"pxsf");
pub const PROP_TARGET_LAYER_LOCK: u32 = fourcc(b"tllk");
pub const PROP_LOCALE: u32 = fourcc(b"locl");

// Color services.
pub const COLOR_SERVICES_CHOOSE_COLOR: i16 = 0;
pub const COLOR_SERVICES_CONVERT_COLOR: i16 = 1;
pub const COLOR_SERVICES_SAMPLE_POINT: i16 = 2;
pub const COLOR_SERVICES_GET_SPECIAL_COLOR: i16 = 3;
pub const COLOR_SPACE_CHOSEN: i16 = -1;
pub const COLOR_SPACE_RGB: i16 = 0;
pub const COLOR_SPACE_HSB: i16 = 1;
pub const COLOR_SPACE_CMYK: i16 = 2;
pub const COLOR_SPACE_LAB: i16 = 3;
pub const COLOR_SPACE_GRAY: i16 = 4;
pub const COLOR_SPACE_HSL: i16 = 5;
pub const COLOR_SPACE_XYZ: i16 = 6;
pub const SPECIAL_COLOR_FOREGROUND: i32 = 0;
pub const SPECIAL_COLOR_BACKGROUND: i32 = 1;

// PICA suite names.
pub const SUITE_HANDLE: &[u8] = b"Photoshop Handle Suite for Plug-ins";
pub const SUITE_BUFFER: &[u8] = b"Photoshop Buffer Suite for Plug-ins";
pub const SUITE_UI_HOOKS: &[u8] = b"Photoshop UIHooks Suite for Plug-ins";
pub const SUITE_PROPERTY: &[u8] = b"Photoshop Property Suite for Plug-ins";

/// One in 16.16 fixed point.
pub const FIXED_ONE: i32 = 0x10000;

/// The largest value Photoshop uses for 16-bit channels.
pub const PS_16BIT_MAX: u16 = 32768;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fourcc_round_trips() {
        assert_eq!(fourcc_str(fourcc(b"8BFM")), "8BFM");
        assert_eq!(parse_fourcc("Pat "), Some(TYPE_PATH));
    }

    #[test]
    fn parse_fourcc_rejects_wrong_lengths_and_wide_characters() {
        assert_eq!(parse_fourcc("abc"), None);
        assert_eq!(parse_fourcc("abcde"), None);
        assert_eq!(parse_fourcc("ab\u{2603}c"), None);
    }

    #[test]
    fn known_error_codes_have_messages() {
        assert!(error_message(FILTER_BAD_MODE).is_some());
        assert!(error_message(1234).is_none());
    }
}
