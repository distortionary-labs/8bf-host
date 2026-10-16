//! The descriptor (scripting parameter) suite.
//!
//! Scripting-aware filters write their settings into a descriptor when
//! they finish and read them back when an action is replayed. The host
//! keeps descriptors as [`Descriptor`] values, hands filters opaque handles
//! for them, and serialises them to JSON so "repeat last filter" works
//! across runs and across processes.
//!
//! Filters reach the suite through [`WRITE_PROCS`] and [`READ_PROCS`]. The
//! callbacks carry no user data, so they find the [`DescriptorSuite`] in
//! this thread's [`Managers`](super::context::Managers).

use std::collections::HashMap;
use std::ffi::c_void;

use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine;
use serde_json::{json, Map, Value};

use super::constants as c;
use super::context;
use super::memory::HandleManager;
use super::structs::{
    Boolean, DescriptorToken, Handle, OSErr, ReadDescriptorProcs, WriteDescriptorProcs,
};

/// Marks a handle's contents as a descriptor serialised by this host.
pub const DESCRIPTOR_MAGIC: &[u8] = b"8BF-HOST-DESCRIPTOR\n";

/// The number of procs in [`WRITE_PROCS`].
const WRITE_PROC_COUNT: i16 = 16;

/// The number of procs in [`READ_PROCS`].
const READ_PROC_COUNT: i16 = 18;

/// The longest key list the host scans when marking keys as found.
const MAX_EXPECTED_KEYS: usize = 4096;

/// The longest text a Pascal `Str255` holds.
const STR255_MAX: usize = 255;

/// One value stored in a [`Descriptor`].
///
/// Composite descriptor types are tuples, as in the original host:
/// unit floats are `(unit, number)`, enumerations `(type, value)` and
/// objects `(class, descriptor)`. Text and aliases are raw bytes.
#[derive(Clone, Debug, PartialEq)]
pub enum DescValue {
    /// A whole number (also used for counts, classes and four-char codes).
    Integer(i64),
    /// A double-precision number.
    Float(f64),
    /// A boolean flag.
    Bool(bool),
    /// Raw bytes: text and alias data.
    Bytes(Vec<u8>),
    /// A fixed group of values, such as a unit float or an enumeration.
    Tuple(Vec<DescValue>),
    /// A nested descriptor.
    Descriptor(Descriptor),
}

impl DescValue {
    /// The depth of the deepest descriptor inside this value.
    fn descriptor_depth(&self) -> usize {
        match self {
            Self::Descriptor(nested) => nested.depth(),
            Self::Tuple(items) => items.iter().map(Self::descriptor_depth).max().unwrap_or(0),
            _ => 0,
        }
    }

    /// The value as a whole number, truncating floats like Python's `int()`.
    fn as_i64(&self) -> Option<i64> {
        match self {
            Self::Integer(value) => Some(*value),
            // `as` saturates out-of-range floats and maps NaN to 0.
            Self::Float(value) => Some(*value as i64),
            Self::Bool(value) => Some(i64::from(*value)),
            _ => None,
        }
    }

    /// The value as a double.
    fn as_f64(&self) -> Option<f64> {
        match self {
            Self::Integer(value) => Some(*value as f64),
            Self::Float(value) => Some(*value),
            Self::Bool(value) => Some(if *value { 1.0 } else { 0.0 }),
            _ => None,
        }
    }

    /// Python truthiness, used by the boolean getter.
    fn is_truthy(&self) -> bool {
        match self {
            Self::Integer(value) => *value != 0,
            Self::Float(value) => *value != 0.0,
            Self::Bool(value) => *value,
            Self::Bytes(value) => !value.is_empty(),
            Self::Tuple(value) => !value.is_empty(),
            Self::Descriptor(value) => !value.items.is_empty(),
        }
    }

    /// The two members of a pair tuple.
    fn as_pair(&self) -> Option<(&DescValue, &DescValue)> {
        match self {
            Self::Tuple(items) if items.len() == 2 => Some((&items[0], &items[1])),
            _ => None,
        }
    }

    fn to_json(&self) -> Value {
        match self {
            Self::Integer(value) => json!(value),
            // Non-finite floats have no JSON form and become null.
            Self::Float(value) => json!(value),
            Self::Bool(value) => json!(value),
            Self::Bytes(value) => json!({ "bytes": BASE64.encode(value) }),
            Self::Tuple(items) => {
                json!({ "tuple": items.iter().map(Self::to_json).collect::<Vec<_>>() })
            }
            Self::Descriptor(descriptor) => json!({ "descriptor": descriptor.to_json() }),
        }
    }

    fn from_json(value: &Value) -> Result<Self, String> {
        match value {
            Value::Bool(flag) => Ok(Self::Bool(*flag)),
            Value::Number(number) => decode_number(number),
            Value::Object(map) => decode_object(map),
            other => Err(format!("unsupported descriptor value: {other}")),
        }
    }
}

/// JSON integers become [`DescValue::Integer`], everything else a float.
fn decode_number(number: &serde_json::Number) -> Result<DescValue, String> {
    if !number.is_f64() {
        if let Some(value) = number.as_i64() {
            return Ok(DescValue::Integer(value));
        }
    }
    number
        .as_f64()
        .map(DescValue::Float)
        .ok_or_else(|| format!("unsupported number: {number}"))
}

fn decode_object(map: &Map<String, Value>) -> Result<DescValue, String> {
    if let Some(nested) = map.get("descriptor") {
        return Descriptor::from_json(nested).map(DescValue::Descriptor);
    }
    if let Some(encoded) = map.get("bytes") {
        let text = encoded
            .as_str()
            .ok_or_else(|| "bytes value is not a string".to_string())?;
        return BASE64
            .decode(text)
            .map(DescValue::Bytes)
            .map_err(|error| format!("bad base64 in bytes value: {error}"));
    }
    if let Some(items) = map.get("tuple") {
        let items = items
            .as_array()
            .ok_or_else(|| "tuple value is not a list".to_string())?;
        return items
            .iter()
            .map(DescValue::from_json)
            .collect::<Result<Vec<_>, _>>()
            .map(DescValue::Tuple);
    }
    Err("unknown descriptor value object".to_string())
}

fn json_fourcc(item: &Value, field: &str) -> Result<u32, String> {
    let text = item
        .get(field)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("descriptor item has no {field:?} text"))?;
    c::parse_fourcc(text).ok_or_else(|| format!("{field} {text:?} is not a four-character code"))
}

/// An ordered list of `(key, type, value)` items.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Descriptor {
    /// The items in the order the filter wrote them.
    pub items: Vec<(u32, u32, DescValue)>,
}

/// How deeply a filter may nest descriptors inside each other.
///
/// Cloning, dropping and serializing a descriptor recurse once per level,
/// and a stack overflow aborts the process where no panic guard can catch
/// it. Real filters nest a handful of levels; JSON presets cannot be read
/// back past serde_json's recursion limit anyway.
pub const MAX_DESCRIPTOR_DEPTH: usize = 16;

impl Descriptor {
    /// The number of descriptor levels: 1 for a flat descriptor.
    pub fn depth(&self) -> usize {
        1 + self
            .items
            .iter()
            .map(|(_, _, value)| value.descriptor_depth())
            .max()
            .unwrap_or(0)
    }

    /// Sets `key`, replacing an existing item in place or appending a new one.
    pub fn put(&mut self, key: u32, value_type: u32, value: DescValue) {
        if let Some(item) = self.items.iter_mut().find(|item| item.0 == key) {
            *item = (key, value_type, value);
            return;
        }
        self.items.push((key, value_type, value));
    }

    /// Returns the type and value stored under `key`.
    pub fn get(&self, key: u32) -> Option<(u32, &DescValue)> {
        self.items
            .iter()
            .find(|item| item.0 == key)
            .map(|(_, value_type, value)| (*value_type, value))
    }

    /// Encodes the descriptor in the JSON format presets use.
    ///
    /// The result is a list of `{"key", "type", "value"}` objects. Values
    /// are plain numbers and booleans, or `{"descriptor": [...]}`,
    /// `{"bytes": base64}` and `{"tuple": [...]}`.
    pub fn to_json(&self) -> Value {
        Value::Array(
            self.items
                .iter()
                .map(|(key, value_type, value)| {
                    json!({
                        "key": c::fourcc_str(*key),
                        "type": c::fourcc_str(*value_type),
                        "value": value.to_json(),
                    })
                })
                .collect(),
        )
    }

    /// Decodes the JSON produced by [`Descriptor::to_json`].
    ///
    /// # Errors
    ///
    /// Returns a description of the problem when the JSON is not a list of
    /// items, a key or type is not a four-character code, a value has an
    /// unknown shape, or descriptors nest more than [`MAX_DESCRIPTOR_DEPTH`]
    /// levels deep.
    pub fn from_json(value: &Value) -> Result<Self, String> {
        let list = value
            .as_array()
            .ok_or_else(|| "descriptor is not a list".to_string())?;
        let mut items = Vec::with_capacity(list.len());
        for item in list {
            let key = json_fourcc(item, "key")?;
            let value_type = json_fourcc(item, "type")?;
            let value = item
                .get("value")
                .ok_or_else(|| "descriptor item has no value".to_string())?;
            items.push((key, value_type, DescValue::from_json(value)?));
        }
        let descriptor = Self { items };
        if descriptor.depth() > MAX_DESCRIPTOR_DEPTH {
            return Err(format!(
                "descriptors nest more than {MAX_DESCRIPTOR_DEPTH} levels deep"
            ));
        }
        Ok(descriptor)
    }

    /// Returns the bytes stored in a descriptor handle: magic prefix plus JSON.
    pub fn serialize(&self) -> Vec<u8> {
        let mut data = DESCRIPTOR_MAGIC.to_vec();
        data.extend_from_slice(self.to_json().to_string().as_bytes());
        data
    }

    /// Parses bytes written by [`Descriptor::serialize`].
    ///
    /// Returns `None` for data without the magic prefix or with bad JSON.
    pub fn deserialize(data: &[u8]) -> Option<Self> {
        let body = data.strip_prefix(DESCRIPTOR_MAGIC)?;
        let value: Value = serde_json::from_slice(body).ok()?;
        Self::from_json(&value).ok()
    }
}

/// A read descriptor opened by a filter.
struct ReadState {
    descriptor: Descriptor,
    /// The filter's zero-terminated key list (exposed address), or 0.
    expected_keys: usize,
    index: usize,
    current: Option<(u32, u32, DescValue)>,
    error: OSErr,
}

/// Host state behind [`WRITE_PROCS`] and [`READ_PROCS`].
#[derive(Default)]
pub struct DescriptorSuite {
    writers: HashMap<usize, Descriptor>,
    readers: HashMap<usize, ReadState>,
    /// Descriptors handed out in handles, with the bytes written there.
    registry: HashMap<usize, (Vec<u8>, Descriptor)>,
    next_token: usize,
}

impl DescriptorSuite {
    /// Returns a new handle holding `descriptor`, or null when memory is short.
    pub fn handle_for(&mut self, handles: &mut HandleManager, descriptor: &Descriptor) -> Handle {
        let data = descriptor.serialize();
        let handle = handles.allocate_from(&data);
        if handle.is_null() {
            return handle;
        }
        self.registry
            .retain(|&address, _| handles.contains(std::ptr::without_provenance_mut(address)));
        self.registry
            .insert(handle as usize, (data, descriptor.clone()));
        handle
    }

    /// Returns the descriptor stored in `handle`, if it holds one.
    pub fn descriptor_for(&self, handles: &HandleManager, handle: Handle) -> Option<Descriptor> {
        if handle.is_null() {
            return None;
        }
        let data = handles.read(handle);
        if let Some((written, descriptor)) = self.registry.get(&(handle as usize)) {
            // Only trust the registry while the handle still holds what we
            // wrote: the filter may have rewritten it, or a disposed
            // handle's address may have been reused.
            if *written == data {
                return Some(descriptor.clone());
            }
        }
        Descriptor::deserialize(&data)
    }

    /// Returns a fresh non-zero token.
    fn token(&mut self) -> usize {
        self.next_token = self.next_token.wrapping_add(1).max(1);
        self.next_token
    }

    fn open_write(&mut self) -> usize {
        let token = self.token();
        self.writers.insert(token, Descriptor::default());
        token
    }

    fn put(&mut self, token: usize, key: u32, value_type: u32, value: DescValue) -> OSErr {
        let Some(descriptor) = self.writers.get_mut(&token) else {
            return c::PARAM_ERR;
        };
        descriptor.put(key, value_type, value);
        c::NO_ERR
    }

    fn open_read(&mut self, handles: &HandleManager, handle: Handle, keys: usize) -> usize {
        let Some(descriptor) = self.descriptor_for(handles, handle) else {
            return 0;
        };
        let token = self.token();
        self.readers.insert(
            token,
            ReadState {
                descriptor,
                expected_keys: keys,
                index: 0,
                current: None,
                error: c::NO_ERR,
            },
        );
        token
    }

    /// Returns the current item's value if its type is one of `types`.
    ///
    /// A type mismatch is remembered and reported by `closeReadDescriptor`.
    fn current(&mut self, token: usize, types: &[u32]) -> Option<DescValue> {
        let state = self.readers.get_mut(&token)?;
        let (_, value_type, value) = state.current.as_ref()?;
        if !types.contains(value_type) {
            state.error = c::PARAM_ERR;
            return None;
        }
        Some(value.clone())
    }
}

/// Runs `f` with this thread's descriptor suite and handles.
///
/// Returns `default` when the managers are already borrowed.
fn with_suite<R>(default: R, f: impl FnOnce(&mut DescriptorSuite, &mut HandleManager) -> R) -> R {
    context::with_managers(|m| f(&mut m.descriptors, &mut m.handles)).unwrap_or(default)
}

fn token_ptr(token: usize) -> DescriptorToken {
    std::ptr::without_provenance_mut(token)
}

/// Stores `value` through an output pointer, refusing null.
///
/// # Safety
///
/// `out` must be null or valid for writing a `T`.
unsafe fn store<T>(out: *mut T, value: T) -> bool {
    if out.is_null() {
        return false;
    }
    // SAFETY: the caller guarantees a non-null `out` is writable.
    unsafe { out.write_unaligned(value) };
    true
}

/// Reads a value through an optional input pointer.
///
/// # Safety
///
/// `pointer` must be null or valid for reading a `T`.
unsafe fn load<T: Copy>(pointer: *const T) -> Option<T> {
    if pointer.is_null() {
        return None;
    }
    // SAFETY: the caller guarantees a non-null `pointer` is readable.
    Some(unsafe { pointer.read_unaligned() })
}

/// Clamps like Python's `min(max(number, minimum), maximum)`, NaN included.
fn pin_float(mut number: f64, minimum: Option<f64>, maximum: Option<f64>) -> f64 {
    if let Some(minimum) = minimum {
        if minimum > number {
            number = minimum;
        }
    }
    if let Some(maximum) = maximum {
        if maximum < number {
            number = maximum;
        }
    }
    number
}

// Write side -------------------------------------------------------------

unsafe extern "C" fn open_write_descriptor() -> DescriptorToken {
    context::guard("openWriteDescriptorProc", std::ptr::null_mut(), || {
        token_ptr(with_suite(0, |suite, _| suite.open_write()))
    })
}

unsafe extern "C" fn close_write_descriptor(
    token: DescriptorToken,
    handle_out: *mut Handle,
) -> OSErr {
    context::guard("closeWriteDescriptorProc", c::PARAM_ERR, || {
        with_suite(c::PARAM_ERR, |suite, handles| {
            let Some(descriptor) = suite.writers.remove(&token.addr()) else {
                return c::PARAM_ERR;
            };
            if handle_out.is_null() {
                return c::NO_ERR;
            }
            let handle = suite.handle_for(handles, &descriptor);
            if handle.is_null() {
                return c::MEM_FULL_ERR;
            }
            // SAFETY: the filter passes a writable `Handle*` (checked non-null).
            unsafe { store(handle_out, handle) };
            c::NO_ERR
        })
    })
}

/// Stores one value in an open write descriptor.
fn put_value(token: DescriptorToken, key: u32, value_type: u32, value: DescValue) -> OSErr {
    with_suite(c::PARAM_ERR, |suite, _| {
        suite.put(token.addr(), key, value_type, value)
    })
}

unsafe extern "C" fn put_integer(token: DescriptorToken, key: u32, value: i32) -> OSErr {
    context::guard("putIntegerProc", c::PARAM_ERR, || {
        put_value(
            token,
            key,
            c::TYPE_INTEGER,
            DescValue::Integer(value.into()),
        )
    })
}

unsafe extern "C" fn put_float(token: DescriptorToken, key: u32, value: *const f64) -> OSErr {
    context::guard("putFloatProc", c::PARAM_ERR, || {
        // SAFETY: the filter passes null or a readable double.
        let Some(value) = (unsafe { load(value) }) else {
            return c::PARAM_ERR;
        };
        put_value(token, key, c::TYPE_FLOAT, DescValue::Float(value))
    })
}

unsafe extern "C" fn put_unit_float(
    token: DescriptorToken,
    key: u32,
    unit: u32,
    value: *const f64,
) -> OSErr {
    context::guard("putUnitFloatProc", c::PARAM_ERR, || {
        // SAFETY: the filter passes null or a readable double.
        let Some(value) = (unsafe { load(value) }) else {
            return c::PARAM_ERR;
        };
        let pair = vec![DescValue::Integer(unit.into()), DescValue::Float(value)];
        put_value(token, key, c::TYPE_UNIT_FLOAT, DescValue::Tuple(pair))
    })
}

unsafe extern "C" fn put_boolean(token: DescriptorToken, key: u32, value: Boolean) -> OSErr {
    context::guard("putBooleanProc", c::PARAM_ERR, || {
        put_value(token, key, c::TYPE_BOOLEAN, DescValue::Bool(value != 0))
    })
}

/// Stores a copy of a handle's contents (empty for null or unknown handles).
fn put_handle_bytes(token: DescriptorToken, key: u32, value_type: u32, handle: Handle) -> OSErr {
    with_suite(c::PARAM_ERR, |suite, handles| {
        let data = handles.read(handle);
        suite.put(token.addr(), key, value_type, DescValue::Bytes(data))
    })
}

unsafe extern "C" fn put_text(token: DescriptorToken, key: u32, handle: Handle) -> OSErr {
    context::guard("putTextProc", c::PARAM_ERR, || {
        put_handle_bytes(token, key, c::TYPE_CHAR, handle)
    })
}

unsafe extern "C" fn put_alias(token: DescriptorToken, key: u32, handle: Handle) -> OSErr {
    context::guard("putAliasProc", c::PARAM_ERR, || {
        put_handle_bytes(token, key, c::TYPE_ALIAS, handle)
    })
}

unsafe extern "C" fn put_enumerated(
    token: DescriptorToken,
    key: u32,
    enum_type: u32,
    value: u32,
) -> OSErr {
    context::guard("putEnumeratedProc", c::PARAM_ERR, || {
        let pair = vec![
            DescValue::Integer(enum_type.into()),
            DescValue::Integer(value.into()),
        ];
        put_value(token, key, c::TYPE_ENUMERATED, DescValue::Tuple(pair))
    })
}

unsafe extern "C" fn put_class(token: DescriptorToken, key: u32, value: u32) -> OSErr {
    context::guard("putClassProc", c::PARAM_ERR, || {
        put_value(token, key, c::TYPE_CLASS, DescValue::Integer(value.into()))
    })
}

unsafe extern "C" fn put_scoped_class(token: DescriptorToken, key: u32, value: u32) -> OSErr {
    context::guard("putScopedClassProc", c::PARAM_ERR, || {
        put_value(
            token,
            key,
            c::TYPE_GLOBAL_CLASS,
            DescValue::Integer(value.into()),
        )
    })
}

/// Stores the descriptor held in `handle` as a nested object.
fn put_nested(
    token: DescriptorToken,
    key: u32,
    value_type: u32,
    class_type: u32,
    handle: Handle,
) -> OSErr {
    with_suite(c::PARAM_ERR, |suite, handles| {
        let Some(nested) = suite.descriptor_for(handles, handle) else {
            return c::PARAM_ERR;
        };
        if nested.depth() >= MAX_DESCRIPTOR_DEPTH {
            context::log(&format!(
                "refused to nest descriptors more than {MAX_DESCRIPTOR_DEPTH} levels deep"
            ));
            return c::PARAM_ERR;
        }
        let pair = vec![
            DescValue::Integer(class_type.into()),
            DescValue::Descriptor(nested),
        ];
        suite.put(token.addr(), key, value_type, DescValue::Tuple(pair))
    })
}

unsafe extern "C" fn put_object(
    token: DescriptorToken,
    key: u32,
    class_type: u32,
    handle: Handle,
) -> OSErr {
    context::guard("putObjectProc", c::PARAM_ERR, || {
        put_nested(token, key, c::TYPE_OBJECT, class_type, handle)
    })
}

unsafe extern "C" fn put_scoped_object(
    token: DescriptorToken,
    key: u32,
    class_type: u32,
    handle: Handle,
) -> OSErr {
    context::guard("putScopedObjectProc", c::PARAM_ERR, || {
        put_nested(token, key, c::TYPE_GLOBAL_OBJECT, class_type, handle)
    })
}

unsafe extern "C" fn put_count(token: DescriptorToken, key: u32, value: u32) -> OSErr {
    context::guard("putCountProc", c::PARAM_ERR, || {
        put_value(
            token,
            key,
            c::TYPE_INTEGER,
            DescValue::Integer(value.into()),
        )
    })
}

unsafe extern "C" fn put_string(token: DescriptorToken, key: u32, pascal: *const u8) -> OSErr {
    context::guard("putStringProc", c::PARAM_ERR, || {
        // SAFETY: the filter passes null or a Pascal string.
        let Some(length) = (unsafe { load(pascal) }) else {
            return c::PARAM_ERR;
        };
        // SAFETY: a Pascal string has `length` (at most 255) bytes after
        // its length byte.
        let text = unsafe { std::slice::from_raw_parts(pascal.add(1), usize::from(length)) };
        put_value(token, key, c::TYPE_CHAR, DescValue::Bytes(text.to_vec()))
    })
}

unsafe extern "C" fn put_simple_reference(
    _token: DescriptorToken,
    _key: u32,
    _reference: *const c_void,
) -> OSErr {
    c::ERR_PLUGIN_HOST_INSUFFICIENT
}

// Read side --------------------------------------------------------------

unsafe extern "C" fn open_read_descriptor(handle: Handle, keys: *const u32) -> DescriptorToken {
    context::guard("openReadDescriptorProc", std::ptr::null_mut(), || {
        let keys = keys.expose_provenance();
        token_ptr(with_suite(0, |suite, handles| {
            suite.open_read(handles, handle, keys)
        }))
    })
}

unsafe extern "C" fn close_read_descriptor(token: DescriptorToken) -> OSErr {
    context::guard("closeReadDescriptorProc", c::PARAM_ERR, || {
        with_suite(c::PARAM_ERR, |suite, _| {
            suite
                .readers
                .remove(&token.addr())
                .map_or(c::PARAM_ERR, |state| state.error)
        })
    })
}

/// Replaces `key` with `typeNull` in a filter's zero-terminated key list.
///
/// # Safety
///
/// `keys` must be the exposed address of a writable, zero-terminated
/// `uint32` array, or 0.
unsafe fn mark_key_found(keys: usize, key: u32) {
    let keys = std::ptr::with_exposed_provenance_mut::<u32>(keys);
    if keys.is_null() {
        return;
    }
    for index in 0..MAX_EXPECTED_KEYS {
        // SAFETY: the list is zero-terminated and we stop at the terminator.
        let slot = unsafe { keys.add(index) };
        // SAFETY: `slot` is inside the list (see above).
        let value = unsafe { slot.read_unaligned() };
        if value == 0 {
            return;
        }
        if value == key {
            // SAFETY: `slot` is a writable entry of the filter's list.
            unsafe { slot.write_unaligned(c::TYPE_NULL) };
        }
    }
}

unsafe extern "C" fn get_key(
    token: DescriptorToken,
    key_out: *mut u32,
    type_out: *mut u32,
    flags_out: *mut i32,
) -> Boolean {
    context::guard("getKeyProc", 0, || {
        with_suite(0, |suite, _| {
            let Some(state) = suite.readers.get_mut(&token.addr()) else {
                return 0;
            };
            if key_out.is_null() {
                return 0;
            }
            let Some(item) = state.descriptor.items.get(state.index).cloned() else {
                return 0;
            };
            state.index += 1;
            let (key, value_type, _) = item;
            state.current = Some(item);
            // SAFETY: the filter passes writable outputs or null.
            unsafe {
                store(key_out, key);
                store(type_out, value_type);
                store(flags_out, 0);
                mark_key_found(state.expected_keys, key);
            }
            1
        })
    })
}

/// Runs `f` on the current value when its type is one of `types`.
fn with_current(
    token: DescriptorToken,
    types: &[u32],
    f: impl FnOnce(DescValue, &mut DescriptorSuite, &mut HandleManager) -> OSErr,
) -> OSErr {
    with_suite(c::PARAM_ERR, |suite, handles| {
        match suite.current(token.addr(), types) {
            Some(value) => f(value, suite, handles),
            None => c::PARAM_ERR,
        }
    })
}

/// Stores a converted value through `out`, or fails when either is missing.
///
/// # Safety
///
/// `out` must be null or writable.
unsafe fn store_result<T>(out: *mut T, value: Option<T>) -> OSErr {
    let Some(value) = value else {
        return c::PARAM_ERR;
    };
    // SAFETY: forwarded from the caller.
    if unsafe { store(out, value) } {
        c::NO_ERR
    } else {
        c::PARAM_ERR
    }
}

unsafe extern "C" fn get_integer(token: DescriptorToken, out: *mut i32) -> OSErr {
    context::guard("getIntegerProc", c::PARAM_ERR, || {
        with_current(token, &[c::TYPE_INTEGER], |value, _, _| {
            // Wraps like a ctypes int32 store.
            let number = value.as_i64().map(|number| number as i32);
            // SAFETY: the filter passes a writable int32 or null.
            unsafe { store_result(out, number) }
        })
    })
}

unsafe extern "C" fn get_count(token: DescriptorToken, out: *mut u32) -> OSErr {
    context::guard("getCountProc", c::PARAM_ERR, || {
        with_current(token, &[c::TYPE_INTEGER], |value, _, _| {
            let number = value.as_i64().map(|number| number as u32);
            // SAFETY: the filter passes a writable uint32 or null.
            unsafe { store_result(out, number) }
        })
    })
}

unsafe extern "C" fn get_float(token: DescriptorToken, out: *mut f64) -> OSErr {
    context::guard("getFloatProc", c::PARAM_ERR, || {
        with_current(token, &[c::TYPE_FLOAT, c::TYPE_INTEGER], |value, _, _| {
            // SAFETY: the filter passes a writable double or null.
            unsafe { store_result(out, value.as_f64()) }
        })
    })
}

/// Splits a unit float into its unit and number.
fn unit_float(value: &DescValue) -> Option<(u32, f64)> {
    let (unit, number) = value.as_pair()?;
    Some((unit.as_i64()? as u32, number.as_f64()?))
}

unsafe extern "C" fn get_unit_float(
    token: DescriptorToken,
    unit_out: *mut u32,
    out: *mut f64,
) -> OSErr {
    context::guard("getUnitFloatProc", c::PARAM_ERR, || {
        with_current(token, &[c::TYPE_UNIT_FLOAT], |value, _, _| {
            let Some((unit, number)) = unit_float(&value) else {
                return c::PARAM_ERR;
            };
            // SAFETY: the filter passes writable outputs or null.
            unsafe {
                if out.is_null() {
                    return c::PARAM_ERR;
                }
                store(unit_out, unit);
                store_result(out, Some(number))
            }
        })
    })
}

unsafe extern "C" fn get_boolean(token: DescriptorToken, out: *mut Boolean) -> OSErr {
    context::guard("getBooleanProc", c::PARAM_ERR, || {
        with_current(token, &[c::TYPE_BOOLEAN], |value, _, _| {
            let flag = Boolean::from(value.is_truthy());
            // SAFETY: the filter passes a writable Boolean or null.
            unsafe { store_result(out, Some(flag)) }
        })
    })
}

/// Returns the current text or alias bytes in a new handle.
fn get_handle_bytes(token: DescriptorToken, value_type: u32, handle_out: *mut Handle) -> OSErr {
    with_current(token, &[value_type], |value, _, handles| {
        let DescValue::Bytes(data) = value else {
            return c::PARAM_ERR;
        };
        if handle_out.is_null() {
            return c::PARAM_ERR;
        }
        let handle = handles.allocate_from(&data);
        if handle.is_null() {
            return c::MEM_FULL_ERR;
        }
        // SAFETY: the filter passes a writable `Handle*` (checked non-null).
        unsafe { store_result(handle_out, Some(handle)) }
    })
}

unsafe extern "C" fn get_text(token: DescriptorToken, handle_out: *mut Handle) -> OSErr {
    context::guard("getTextProc", c::PARAM_ERR, || {
        get_handle_bytes(token, c::TYPE_CHAR, handle_out)
    })
}

unsafe extern "C" fn get_alias(token: DescriptorToken, handle_out: *mut Handle) -> OSErr {
    context::guard("getAliasProc", c::PARAM_ERR, || {
        get_handle_bytes(token, c::TYPE_ALIAS, handle_out)
    })
}

unsafe extern "C" fn get_enumerated(token: DescriptorToken, out: *mut u32) -> OSErr {
    context::guard("getEnumeratedProc", c::PARAM_ERR, || {
        with_current(token, &[c::TYPE_ENUMERATED], |value, _, _| {
            let number = value
                .as_pair()
                .and_then(|(_, number)| number.as_i64())
                .map(|number| number as u32);
            // SAFETY: the filter passes a writable uint32 or null.
            unsafe { store_result(out, number) }
        })
    })
}

unsafe extern "C" fn get_class(token: DescriptorToken, out: *mut u32) -> OSErr {
    context::guard("getClassProc", c::PARAM_ERR, || {
        let types = [c::TYPE_CLASS, c::TYPE_GLOBAL_CLASS];
        with_current(token, &types, |value, _, _| {
            let number = value.as_i64().map(|number| number as u32);
            // SAFETY: the filter passes a writable uint32 or null.
            unsafe { store_result(out, number) }
        })
    })
}

unsafe extern "C" fn get_object(
    token: DescriptorToken,
    type_out: *mut u32,
    handle_out: *mut Handle,
) -> OSErr {
    context::guard("getObjectProc", c::PARAM_ERR, || {
        let types = [c::TYPE_OBJECT, c::TYPE_GLOBAL_OBJECT];
        with_current(token, &types, |value, suite, handles| {
            let Some((class_type, DescValue::Descriptor(nested))) = value.as_pair() else {
                return c::PARAM_ERR;
            };
            let Some(class_type) = class_type.as_i64() else {
                return c::PARAM_ERR;
            };
            if handle_out.is_null() {
                return c::PARAM_ERR;
            }
            let handle = suite.handle_for(handles, nested);
            if handle.is_null() {
                return c::MEM_FULL_ERR;
            }
            // SAFETY: the filter passes writable outputs or null.
            unsafe {
                store(type_out, class_type as u32);
                store_result(handle_out, Some(handle))
            }
        })
    })
}

unsafe extern "C" fn get_string(token: DescriptorToken, str255: *mut u8) -> OSErr {
    context::guard("getStringProc", c::PARAM_ERR, || {
        with_current(token, &[c::TYPE_CHAR], |value, _, _| {
            let DescValue::Bytes(data) = value else {
                return c::PARAM_ERR;
            };
            if str255.is_null() {
                return c::PARAM_ERR;
            }
            let text = &data[..data.len().min(STR255_MAX)];
            // SAFETY: a Str255 has room for a length byte and 255 bytes,
            // and `text` is at most 255 bytes.
            unsafe {
                str255.write(text.len() as u8);
                std::ptr::copy_nonoverlapping(text.as_ptr(), str255.add(1), text.len());
            }
            c::NO_ERR
        })
    })
}

unsafe extern "C" fn get_pinned_integer(
    token: DescriptorToken,
    minimum: i32,
    maximum: i32,
    out: *mut i32,
) -> OSErr {
    context::guard("getPinnedIntegerProc", c::PARAM_ERR, || {
        with_current(token, &[c::TYPE_INTEGER], |value, _, _| {
            let number = value.as_i64().map(|number| {
                let pinned = number.max(minimum.into()).min(maximum.into());
                i32::try_from(pinned).unwrap_or(maximum)
            });
            // SAFETY: the filter passes a writable int32 or null.
            unsafe { store_result(out, number) }
        })
    })
}

unsafe extern "C" fn get_pinned_float(
    token: DescriptorToken,
    minimum: *const f64,
    maximum: *const f64,
    out: *mut f64,
) -> OSErr {
    context::guard("getPinnedFloatProc", c::PARAM_ERR, || {
        with_current(token, &[c::TYPE_FLOAT, c::TYPE_INTEGER], |value, _, _| {
            // SAFETY: the filter passes readable bounds or null.
            let (minimum, maximum) = unsafe { (load(minimum), load(maximum)) };
            let number = value
                .as_f64()
                .map(|number| pin_float(number, minimum, maximum));
            // SAFETY: the filter passes a writable double or null.
            unsafe { store_result(out, number) }
        })
    })
}

unsafe extern "C" fn get_pinned_unit_float(
    token: DescriptorToken,
    minimum: *const f64,
    maximum: *const f64,
    unit_out: *mut u32,
    out: *mut f64,
) -> OSErr {
    context::guard("getPinnedUnitFloatProc", c::PARAM_ERR, || {
        with_current(token, &[c::TYPE_UNIT_FLOAT], |value, _, _| {
            let Some((unit, number)) = unit_float(&value) else {
                return c::PARAM_ERR;
            };
            if out.is_null() {
                return c::PARAM_ERR;
            }
            // SAFETY: the filter passes readable bounds and writable
            // outputs, or null.
            unsafe {
                let number = pin_float(number, load(minimum), load(maximum));
                store(unit_out, unit);
                store_result(out, Some(number))
            }
        })
    })
}

unsafe extern "C" fn get_simple_reference(_token: DescriptorToken, _out: *mut c_void) -> OSErr {
    c::ERR_PLUGIN_HOST_INSUFFICIENT
}

/// The write-descriptor procs handed to filters.
pub static WRITE_PROCS: WriteDescriptorProcs = WriteDescriptorProcs {
    writeDescriptorProcsVersion: 0,
    numWriteDescriptorProcs: WRITE_PROC_COUNT,
    openWriteDescriptorProc: Some(open_write_descriptor),
    closeWriteDescriptorProc: Some(close_write_descriptor),
    putIntegerProc: Some(put_integer),
    putFloatProc: Some(put_float),
    putUnitFloatProc: Some(put_unit_float),
    putBooleanProc: Some(put_boolean),
    putTextProc: Some(put_text),
    putAliasProc: Some(put_alias),
    putEnumeratedProc: Some(put_enumerated),
    putClassProc: Some(put_class),
    putSimpleReferenceProc: Some(put_simple_reference),
    putObjectProc: Some(put_object),
    putCountProc: Some(put_count),
    putStringProc: Some(put_string),
    putScopedClassProc: Some(put_scoped_class),
    putScopedObjectProc: Some(put_scoped_object),
};

/// The read-descriptor procs handed to filters.
pub static READ_PROCS: ReadDescriptorProcs = ReadDescriptorProcs {
    readDescriptorProcsVersion: 0,
    numReadDescriptorProcs: READ_PROC_COUNT,
    openReadDescriptorProc: Some(open_read_descriptor),
    closeReadDescriptorProc: Some(close_read_descriptor),
    getKeyProc: Some(get_key),
    getIntegerProc: Some(get_integer),
    getFloatProc: Some(get_float),
    getUnitFloatProc: Some(get_unit_float),
    getBooleanProc: Some(get_boolean),
    getTextProc: Some(get_text),
    getAliasProc: Some(get_alias),
    getEnumeratedProc: Some(get_enumerated),
    getClassProc: Some(get_class),
    getSimpleReferenceProc: Some(get_simple_reference),
    getObjectProc: Some(get_object),
    getCountProc: Some(get_count),
    getStringProc: Some(get_string),
    getPinnedIntegerProc: Some(get_pinned_integer),
    getPinnedFloatProc: Some(get_pinned_float),
    getPinnedUnitFloatProc: Some(get_pinned_unit_float),
};

#[cfg(test)]
mod tests {
    use super::*;
    use std::ptr::{null, null_mut};

    const AMNT: u32 = c::fourcc(b"Amnt");
    const RUNS: u32 = c::fourcc(b"Runs");
    const FLT: u32 = c::fourcc(b"Flt ");
    const RAD: u32 = c::fourcc(b"Rad ");
    const PXL: u32 = c::fourcc(b"#Pxl");
    const FLAG: u32 = c::fourcc(b"Flag");
    const TEXT: u32 = c::fourcc(b"Text");
    const MODE: u32 = c::fourcc(b"Mode");
    const HARD: u32 = c::fourcc(b"Hard");
    const CLSS: u32 = c::fourcc(b"Clss");
    const OBJC: u32 = c::fourcc(b"Objc");
    const CNT: u32 = c::fourcc(b"Cnt ");
    const STR: u32 = c::fourcc(b"Str ");

    fn sample() -> Descriptor {
        let nested = Descriptor {
            items: vec![(
                RAD,
                c::TYPE_UNIT_FLOAT,
                DescValue::Tuple(vec![DescValue::Integer(PXL.into()), DescValue::Float(2.5)]),
            )],
        };
        Descriptor {
            items: vec![
                (AMNT, c::TYPE_INTEGER, DescValue::Integer(5)),
                (
                    TEXT,
                    c::TYPE_CHAR,
                    DescValue::Bytes(b"hello\0\xff".to_vec()),
                ),
                (
                    MODE,
                    c::TYPE_ENUMERATED,
                    DescValue::Tuple(vec![
                        DescValue::Integer(MODE.into()),
                        DescValue::Integer(HARD.into()),
                    ]),
                ),
                (
                    OBJC,
                    c::TYPE_OBJECT,
                    DescValue::Tuple(vec![
                        DescValue::Integer(CLSS.into()),
                        DescValue::Descriptor(nested),
                    ]),
                ),
                (FLAG, c::TYPE_BOOLEAN, DescValue::Bool(true)),
                (FLT, c::TYPE_FLOAT, DescValue::Float(3.0)),
            ],
        }
    }

    #[test]
    fn put_replaces_in_place_and_get_finds_items() {
        let mut descriptor = Descriptor::default();
        descriptor.put(AMNT, c::TYPE_INTEGER, DescValue::Integer(1));
        descriptor.put(RUNS, c::TYPE_INTEGER, DescValue::Integer(2));
        descriptor.put(AMNT, c::TYPE_FLOAT, DescValue::Float(1.5));
        assert_eq!(descriptor.items[0].0, AMNT);
        assert_eq!(
            descriptor.get(AMNT),
            Some((c::TYPE_FLOAT, &DescValue::Float(1.5)))
        );
        assert_eq!(descriptor.get(FLAG), None);
    }

    #[test]
    fn depth_counts_nested_descriptors_inside_tuples() {
        let mut descriptor = Descriptor::default();
        assert_eq!(descriptor.depth(), 1);
        for level in 2..=4 {
            let mut outer = Descriptor::default();
            outer.put(
                AMNT,
                c::TYPE_OBJECT,
                DescValue::Tuple(vec![
                    DescValue::Integer(0),
                    DescValue::Descriptor(descriptor),
                ]),
            );
            descriptor = outer;
            assert_eq!(descriptor.depth(), level);
        }
        assert_eq!(sample().depth(), 2);
    }

    #[test]
    fn json_deeper_than_the_nesting_limit_is_refused() {
        let mut descriptor = Descriptor::default();
        for _ in 1..MAX_DESCRIPTOR_DEPTH {
            let mut outer = Descriptor::default();
            outer.put(
                AMNT,
                c::TYPE_OBJECT,
                DescValue::Tuple(vec![
                    DescValue::Integer(0),
                    DescValue::Descriptor(descriptor),
                ]),
            );
            descriptor = outer;
        }
        assert_eq!(descriptor.depth(), MAX_DESCRIPTOR_DEPTH);
        assert!(Descriptor::from_json(&descriptor.to_json()).is_ok());

        let mut outer = Descriptor::default();
        outer.put(
            AMNT,
            c::TYPE_OBJECT,
            DescValue::Tuple(vec![
                DescValue::Integer(0),
                DescValue::Descriptor(descriptor),
            ]),
        );
        assert!(Descriptor::from_json(&outer.to_json()).is_err());
    }

    #[test]
    fn json_round_trips_every_value_kind() {
        let descriptor = sample();
        let json = descriptor.to_json();
        assert_eq!(Descriptor::from_json(&json), Ok(descriptor.clone()));
        assert_eq!(
            Descriptor::deserialize(&descriptor.serialize()),
            Some(descriptor)
        );
        // Floats with integral values stay floats.
        assert_eq!(json[5]["value"], serde_json::json!(3.0));
        assert!(json[5]["value"].is_f64());
    }

    #[test]
    fn json_round_trips_the_preset_format() {
        let text = r#"[
            {"key": "Amnt", "type": "long", "value": 3},
            {"key": "Rad ", "type": "UntF", "value": {"tuple": [592476268, 2.5]}},
            {"key": "Text", "type": "TEXT", "value": {"bytes": "aGk="}},
            {"key": "Flag", "type": "bool", "value": false},
            {"key": "Objc", "type": "Objc", "value": {"tuple": [1131180915,
                {"descriptor": [{"key": "Amnt", "type": "doub", "value": 1.0}]}]}}
        ]"#;
        let value: Value = serde_json::from_str(text).unwrap();
        let descriptor = Descriptor::from_json(&value).unwrap();
        assert_eq!(
            descriptor.get(AMNT),
            Some((c::TYPE_INTEGER, &DescValue::Integer(3)))
        );
        assert_eq!(
            descriptor.get(TEXT),
            Some((c::TYPE_CHAR, &DescValue::Bytes(b"hi".to_vec())))
        );
        assert_eq!(
            descriptor.get(FLAG),
            Some((c::TYPE_BOOLEAN, &DescValue::Bool(false)))
        );
        let Some((_, DescValue::Tuple(object))) = descriptor.get(OBJC) else {
            panic!("object missing");
        };
        let DescValue::Descriptor(nested) = &object[1] else {
            panic!("nested descriptor missing");
        };
        assert_eq!(
            nested.get(AMNT),
            Some((c::TYPE_FLOAT, &DescValue::Float(1.0)))
        );
        assert_eq!(descriptor.to_json(), value);
        assert_eq!(value[0]["key"], "Amnt");
    }

    #[test]
    fn from_json_rejects_malformed_input() {
        let bad = [
            r#"{"key": "Amnt"}"#,
            r#"[{"key": "Amnt", "type": "long"}]"#,
            r#"[{"key": "Am", "type": "long", "value": 1}]"#,
            r#"[{"key": "Amnt", "type": "long", "value": "text"}]"#,
            r#"[{"key": "Amnt", "type": "long", "value": {"bytes": "!!"}}]"#,
            r#"[{"key": "Amnt", "type": "long", "value": {"other": 1}}]"#,
            r#"[{"key": "Amnt", "type": "long", "value": {"tuple": 1}}]"#,
        ];
        for text in bad {
            let value: Value = serde_json::from_str(text).unwrap();
            assert!(Descriptor::from_json(&value).is_err(), "{text}");
        }
    }

    #[test]
    fn deserialize_rejects_bad_magic_and_garbage() {
        assert_eq!(Descriptor::deserialize(b"random"), None);
        assert_eq!(Descriptor::deserialize(b""), None);
        assert_eq!(Descriptor::deserialize(b"OTHER-DESCRIPTOR\n[]"), None);
        let mut garbage = DESCRIPTOR_MAGIC.to_vec();
        garbage.extend_from_slice(b"{not json");
        assert_eq!(Descriptor::deserialize(&garbage), None);
        let mut empty = DESCRIPTOR_MAGIC.to_vec();
        empty.extend_from_slice(b"[]");
        assert_eq!(Descriptor::deserialize(&empty), Some(Descriptor::default()));
    }

    #[test]
    fn suite_registry_follows_handle_contents() {
        let mut suite = DescriptorSuite::default();
        let mut handles = HandleManager::new();
        let descriptor = sample();
        let handle = suite.handle_for(&mut handles, &descriptor);
        assert_eq!(suite.descriptor_for(&handles, handle), Some(descriptor));
        assert_eq!(suite.descriptor_for(&handles, null_mut()), None);
        // A filter rewriting the handle wins over the registry.
        let other = Descriptor {
            items: vec![(RUNS, c::TYPE_INTEGER, DescValue::Integer(9))],
        };
        handles.write(handle, &other.serialize());
        assert_eq!(suite.descriptor_for(&handles, handle), Some(other));
        handles.write(handle, b"junk");
        assert_eq!(suite.descriptor_for(&handles, handle), None);
    }

    /// Runs `f` with the managers of the test thread.
    fn managers<R>(f: impl FnOnce(&mut context::Managers) -> R) -> R {
        context::with_managers(f).unwrap()
    }

    fn new_text_handle(data: &[u8]) -> Handle {
        managers(|m| m.handles.allocate_from(data))
    }

    fn read_handle(handle: Handle) -> Vec<u8> {
        managers(|m| m.handles.read(handle))
    }

    /// Writes a descriptor through the procs the way a filter does.
    unsafe fn write_everything() -> Handle {
        let w = &WRITE_PROCS;
        let nested_token = w.openWriteDescriptorProc.unwrap()();
        assert!(!nested_token.is_null());
        let radius = 4.5;
        assert_eq!(
            w.putFloatProc.unwrap()(nested_token, RAD, &radius),
            c::NO_ERR
        );
        let mut nested = null_mut();
        assert_eq!(
            w.closeWriteDescriptorProc.unwrap()(nested_token, &mut nested),
            c::NO_ERR
        );
        assert!(!nested.is_null());

        let token = w.openWriteDescriptorProc.unwrap()();
        assert!(!token.is_null());
        assert_ne!(token, nested_token);
        let float = 1.25;
        let unit = 150.0;
        let text = new_text_handle(b"some text");
        let mut pascal = [0u8; 256];
        pascal[0] = 3;
        pascal[1..4].copy_from_slice(b"abc");
        assert_eq!(w.putIntegerProc.unwrap()(token, AMNT, -7), c::NO_ERR);
        assert_eq!(w.putFloatProc.unwrap()(token, FLT, &float), c::NO_ERR);
        assert_eq!(
            w.putUnitFloatProc.unwrap()(token, RAD, PXL, &unit),
            c::NO_ERR
        );
        assert_eq!(w.putBooleanProc.unwrap()(token, FLAG, 1), c::NO_ERR);
        assert_eq!(w.putTextProc.unwrap()(token, TEXT, text), c::NO_ERR);
        assert_eq!(
            w.putEnumeratedProc.unwrap()(token, MODE, MODE, HARD),
            c::NO_ERR
        );
        assert_eq!(w.putClassProc.unwrap()(token, CLSS, HARD), c::NO_ERR);
        assert_eq!(
            w.putObjectProc.unwrap()(token, OBJC, CLSS, nested),
            c::NO_ERR
        );
        assert_eq!(w.putCountProc.unwrap()(token, CNT, 0xFFFF_FFFF), c::NO_ERR);
        assert_eq!(
            w.putStringProc.unwrap()(token, STR, pascal.as_ptr()),
            c::NO_ERR
        );
        assert_eq!(
            w.putSimpleReferenceProc.unwrap()(token, RUNS, null()),
            c::ERR_PLUGIN_HOST_INSUFFICIENT
        );
        let mut handle = null_mut();
        assert_eq!(
            w.closeWriteDescriptorProc.unwrap()(token, &mut handle),
            c::NO_ERR
        );
        assert!(!handle.is_null());
        // The token is gone once closed.
        assert_eq!(w.putIntegerProc.unwrap()(token, AMNT, 1), c::PARAM_ERR);
        handle
    }

    #[test]
    fn proc_tables_are_complete() {
        assert_eq!(WRITE_PROCS.numWriteDescriptorProcs, 16);
        assert_eq!(READ_PROCS.numReadDescriptorProcs, 18);
        assert!(WRITE_PROCS.putScopedObjectProc.is_some());
        assert!(READ_PROCS.getPinnedUnitFloatProc.is_some());
    }

    #[test]
    fn filter_written_descriptor_is_stored_in_a_handle() {
        // SAFETY: every pointer passed to the procs is valid.
        let handle = unsafe { write_everything() };
        let descriptor = managers(|m| m.descriptors.descriptor_for(&m.handles, handle)).unwrap();
        let keys: Vec<u32> = descriptor.items.iter().map(|item| item.0).collect();
        assert_eq!(
            keys,
            [AMNT, FLT, RAD, FLAG, TEXT, MODE, CLSS, OBJC, CNT, STR]
        );
        assert_eq!(
            descriptor.get(AMNT),
            Some((c::TYPE_INTEGER, &DescValue::Integer(-7)))
        );
        assert_eq!(
            descriptor.get(CNT),
            Some((c::TYPE_INTEGER, &DescValue::Integer(0xFFFF_FFFF)))
        );
        assert_eq!(
            descriptor.get(STR),
            Some((c::TYPE_CHAR, &DescValue::Bytes(b"abc".to_vec())))
        );
        assert_eq!(
            Descriptor::deserialize(&read_handle(handle)),
            Some(descriptor)
        );
    }

    #[test]
    fn filter_reads_back_every_value() {
        // SAFETY: every pointer passed to the procs is valid and the key
        // list is zero-terminated.
        unsafe {
            let handle = write_everything();
            let r = &READ_PROCS;
            let mut expected = [AMNT, TEXT, c::fourcc(b"Miss"), 0];
            let token = r.openReadDescriptorProc.unwrap()(handle, expected.as_mut_ptr());
            assert!(!token.is_null());
            let (mut key, mut value_type, mut flags) = (0u32, 0u32, -1i32);
            let mut seen = Vec::new();
            while r.getKeyProc.unwrap()(token, &mut key, &mut value_type, &mut flags) != 0 {
                assert_eq!(flags, 0);
                seen.push(key);
                match key {
                    AMNT => {
                        assert_eq!(value_type, c::TYPE_INTEGER);
                        let mut out = 0i32;
                        assert_eq!(r.getIntegerProc.unwrap()(token, &mut out), c::NO_ERR);
                        assert_eq!(out, -7);
                        assert_eq!(r.getPinnedIntegerProc.unwrap()(token, 0, 10, &mut out), 0);
                        assert_eq!(out, 0);
                        assert_eq!(
                            r.getPinnedIntegerProc.unwrap()(token, -20, -10, &mut out),
                            0
                        );
                        assert_eq!(out, -10);
                        let mut float = 0.0;
                        assert_eq!(r.getFloatProc.unwrap()(token, &mut float), c::NO_ERR);
                        assert_eq!(float, -7.0);
                    }
                    FLT => {
                        let mut out = 0.0;
                        assert_eq!(r.getFloatProc.unwrap()(token, &mut out), c::NO_ERR);
                        assert_eq!(out, 1.25);
                        let (low, high) = (2.0, 3.0);
                        let pinned = r.getPinnedFloatProc.unwrap();
                        assert_eq!(pinned(token, &low, &high, &mut out), c::NO_ERR);
                        assert_eq!(out, 2.0);
                        assert_eq!(pinned(token, null(), null(), &mut out), c::NO_ERR);
                        assert_eq!(out, 1.25);
                        let low = 0.0;
                        let high = 1.0;
                        assert_eq!(pinned(token, &low, &high, &mut out), c::NO_ERR);
                        assert_eq!(out, 1.0);
                    }
                    RAD => {
                        assert_eq!(value_type, c::TYPE_UNIT_FLOAT);
                        let (mut unit, mut out) = (0u32, 0.0);
                        assert_eq!(
                            r.getUnitFloatProc.unwrap()(token, &mut unit, &mut out),
                            c::NO_ERR
                        );
                        assert_eq!((unit, out), (PXL, 150.0));
                        let high = 100.0;
                        let pinned = r.getPinnedUnitFloatProc.unwrap();
                        assert_eq!(pinned(token, null(), &high, &mut unit, &mut out), 0);
                        assert_eq!(out, 100.0);
                        let low = 200.0;
                        assert_eq!(pinned(token, &low, null(), null_mut(), &mut out), 0);
                        assert_eq!(out, 200.0);
                    }
                    FLAG => {
                        let mut out = 0u8;
                        assert_eq!(r.getBooleanProc.unwrap()(token, &mut out), c::NO_ERR);
                        assert_eq!(out, 1);
                    }
                    TEXT => {
                        let mut out = null_mut();
                        assert_eq!(r.getTextProc.unwrap()(token, &mut out), c::NO_ERR);
                        assert_eq!(read_handle(out), b"some text");
                        let mut pascal = [0xAAu8; 256];
                        assert_eq!(
                            r.getStringProc.unwrap()(token, pascal.as_mut_ptr()),
                            c::NO_ERR
                        );
                        assert_eq!(&pascal[..10], b"\x09some text");
                        assert_eq!(pascal[10], 0xAA);
                    }
                    MODE => {
                        let mut out = 0u32;
                        assert_eq!(r.getEnumeratedProc.unwrap()(token, &mut out), c::NO_ERR);
                        assert_eq!(out, HARD);
                    }
                    CLSS => {
                        let mut out = 0u32;
                        assert_eq!(r.getClassProc.unwrap()(token, &mut out), c::NO_ERR);
                        assert_eq!(out, HARD);
                    }
                    OBJC => {
                        let (mut class, mut nested) = (0u32, null_mut());
                        assert_eq!(
                            r.getObjectProc.unwrap()(token, &mut class, &mut nested),
                            c::NO_ERR
                        );
                        assert_eq!(class, CLSS);
                        let inner = r.openReadDescriptorProc.unwrap()(nested, null());
                        assert!(!inner.is_null());
                        assert_eq!(
                            r.getKeyProc.unwrap()(inner, &mut key, null_mut(), null_mut()),
                            1
                        );
                        assert_eq!(key, RAD);
                        let mut out = 0.0;
                        assert_eq!(r.getFloatProc.unwrap()(inner, &mut out), c::NO_ERR);
                        assert_eq!(out, 4.5);
                        assert_eq!(
                            r.getKeyProc.unwrap()(inner, &mut key, null_mut(), null_mut()),
                            0
                        );
                        assert_eq!(r.closeReadDescriptorProc.unwrap()(inner), c::NO_ERR);
                    }
                    CNT => {
                        let mut out = 0u32;
                        assert_eq!(r.getCountProc.unwrap()(token, &mut out), c::NO_ERR);
                        assert_eq!(out, 0xFFFF_FFFF);
                    }
                    STR => {
                        let mut pascal = [0u8; 256];
                        assert_eq!(
                            r.getStringProc.unwrap()(token, pascal.as_mut_ptr()),
                            c::NO_ERR
                        );
                        assert_eq!(&pascal[..4], b"\x03abc");
                    }
                    other => panic!("unexpected key {}", c::fourcc_str(other)),
                }
            }
            assert_eq!(seen.len(), 10);
            assert_eq!(
                expected,
                [c::TYPE_NULL, c::TYPE_NULL, c::fourcc(b"Miss"), 0]
            );
            assert_eq!(r.closeReadDescriptorProc.unwrap()(token), c::NO_ERR);
            assert_eq!(r.closeReadDescriptorProc.unwrap()(token), c::PARAM_ERR);
        }
    }

    #[test]
    fn type_mismatches_fail_and_are_reported_on_close() {
        let handle = managers(|m| m.descriptors.handle_for(&mut m.handles, &sample()));
        // SAFETY: every pointer passed to the procs is valid.
        unsafe {
            let r = &READ_PROCS;
            let token = r.openReadDescriptorProc.unwrap()(handle, null());
            let mut key = 0u32;
            assert_eq!(
                r.getKeyProc.unwrap()(token, &mut key, null_mut(), null_mut()),
                1
            );
            assert_eq!(key, AMNT);
            let mut flag = 0u8;
            assert_eq!(r.getBooleanProc.unwrap()(token, &mut flag), c::PARAM_ERR);
            let mut unit = 0.0;
            assert_eq!(
                r.getUnitFloatProc.unwrap()(token, null_mut(), &mut unit),
                c::PARAM_ERR
            );
            assert_eq!(
                r.getSimpleReferenceProc.unwrap()(token, null_mut()),
                c::ERR_PLUGIN_HOST_INSUFFICIENT
            );
            assert_eq!(r.closeReadDescriptorProc.unwrap()(token), c::PARAM_ERR);
        }
    }

    #[test]
    fn getters_before_get_key_fail_without_poisoning_close() {
        let handle = managers(|m| m.descriptors.handle_for(&mut m.handles, &sample()));
        // SAFETY: every pointer passed to the procs is valid.
        unsafe {
            let r = &READ_PROCS;
            let token = r.openReadDescriptorProc.unwrap()(handle, null());
            let mut out = 0i32;
            assert_eq!(r.getIntegerProc.unwrap()(token, &mut out), c::PARAM_ERR);
            assert_eq!(r.closeReadDescriptorProc.unwrap()(token), c::NO_ERR);
        }
    }

    #[test]
    fn long_text_is_truncated_to_a_str255() {
        let long = Descriptor {
            items: vec![(TEXT, c::TYPE_CHAR, DescValue::Bytes(vec![b'x'; 400]))],
        };
        let handle = managers(|m| m.descriptors.handle_for(&mut m.handles, &long));
        // SAFETY: the Str255 buffer is followed by a guard byte we check.
        unsafe {
            let r = &READ_PROCS;
            let token = r.openReadDescriptorProc.unwrap()(handle, null());
            let mut key = 0u32;
            assert_eq!(
                r.getKeyProc.unwrap()(token, &mut key, null_mut(), null_mut()),
                1
            );
            let mut buffer = [0u8; 257];
            assert_eq!(
                r.getStringProc.unwrap()(token, buffer.as_mut_ptr()),
                c::NO_ERR
            );
            assert_eq!(buffer[0], 255);
            assert!(buffer[1..256].iter().all(|&b| b == b'x'));
            assert_eq!(buffer[256], 0);
            assert_eq!(r.closeReadDescriptorProc.unwrap()(token), c::NO_ERR);
        }
    }

    #[test]
    fn null_and_unknown_arguments_return_errors() {
        // SAFETY: the procs must cope with null and bogus arguments.
        unsafe {
            let w = &WRITE_PROCS;
            let r = &READ_PROCS;
            let bogus = std::ptr::without_provenance_mut::<c_void>(0xDEAD);
            assert_eq!(
                w.closeWriteDescriptorProc.unwrap()(null_mut(), null_mut()),
                c::PARAM_ERR
            );
            assert_eq!(w.putIntegerProc.unwrap()(bogus, AMNT, 1), c::PARAM_ERR);

            let token = w.openWriteDescriptorProc.unwrap()();
            assert_eq!(w.putFloatProc.unwrap()(token, FLT, null()), c::PARAM_ERR);
            assert_eq!(
                w.putUnitFloatProc.unwrap()(token, RAD, PXL, null()),
                c::PARAM_ERR
            );
            assert_eq!(w.putStringProc.unwrap()(token, STR, null()), c::PARAM_ERR);
            assert_eq!(
                w.putObjectProc.unwrap()(token, OBJC, CLSS, null_mut()),
                c::PARAM_ERR
            );
            assert_eq!(
                w.putScopedObjectProc.unwrap()(token, OBJC, CLSS, bogus),
                c::PARAM_ERR
            );
            // Null text handles store empty text, as the original host did.
            assert_eq!(w.putTextProc.unwrap()(token, TEXT, null_mut()), c::NO_ERR);
            // Closing without an output handle discards the descriptor.
            assert_eq!(
                w.closeWriteDescriptorProc.unwrap()(token, null_mut()),
                c::NO_ERR
            );

            assert!(r.openReadDescriptorProc.unwrap()(null_mut(), null()).is_null());
            let junk = new_text_handle(b"not a descriptor");
            assert!(r.openReadDescriptorProc.unwrap()(junk, null()).is_null());
            let mut key = 0u32;
            assert_eq!(
                r.getKeyProc.unwrap()(null_mut(), &mut key, null_mut(), null_mut()),
                0
            );
            assert_eq!(r.closeReadDescriptorProc.unwrap()(null_mut()), c::PARAM_ERR);

            let handle = managers(|m| m.descriptors.handle_for(&mut m.handles, &sample()));
            let token = r.openReadDescriptorProc.unwrap()(handle, null());
            assert_eq!(
                r.getKeyProc.unwrap()(token, null_mut(), null_mut(), null_mut()),
                0
            );
            assert_eq!(
                r.getKeyProc.unwrap()(token, &mut key, null_mut(), null_mut()),
                1
            );
            assert_eq!(r.getIntegerProc.unwrap()(token, null_mut()), c::PARAM_ERR);
            assert_eq!(
                r.getPinnedIntegerProc.unwrap()(token, 0, 1, null_mut()),
                c::PARAM_ERR
            );
            assert_eq!(
                r.getKeyProc.unwrap()(token, &mut key, null_mut(), null_mut()),
                1
            );
            assert_eq!(key, TEXT);
            assert_eq!(r.getTextProc.unwrap()(token, null_mut()), c::PARAM_ERR);
            assert_eq!(r.getStringProc.unwrap()(token, null_mut()), c::PARAM_ERR);
            r.getKeyProc.unwrap()(token, &mut key, null_mut(), null_mut());
            assert_eq!(key, MODE);
            assert_eq!(
                r.getEnumeratedProc.unwrap()(token, null_mut()),
                c::PARAM_ERR
            );
            r.getKeyProc.unwrap()(token, &mut key, null_mut(), null_mut());
            assert_eq!(key, OBJC);
            assert_eq!(
                r.getObjectProc.unwrap()(token, null_mut(), null_mut()),
                c::PARAM_ERR
            );
            assert_eq!(r.closeReadDescriptorProc.unwrap()(token), c::NO_ERR);
        }
    }

    #[test]
    fn re_entrant_calls_fail_instead_of_panicking() {
        let token = context::with_managers(|_| {
            // SAFETY: the proc takes no arguments.
            unsafe { WRITE_PROCS.openWriteDescriptorProc.unwrap()() }
        });
        assert_eq!(token, Some(null_mut()));
    }
}
