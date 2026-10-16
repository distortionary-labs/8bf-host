//! Find Photoshop filter plug-ins on disk and read their PiPL resources.
//!
//! Nothing here loads plug-in code; it only parses files. Three formats are
//! recognized:
//!
//! - `pe`: a Windows DLL (`.8bf`) with PiPL resources. Runs natively on
//!   Windows and under Wine elsewhere.
//! - `mac-bundle`: a macOS bundle (`.plugin` or `.8bf` directory) with
//!   PiPLs in a `.rsrc` file or the executable's resource fork.
//! - `native`: a shared library for the current OS with a sidecar `.pipl`
//!   file holding a little-endian PiPL. Used by the test suite and by anyone
//!   building filters for Linux.

use std::collections::{BTreeMap, HashSet};
use std::ffi::OsString;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use serde::{Deserialize, Serialize};

use super::pe::{self, PeError, PeFile, ResourceKey};
use super::pipl::{self, parse_pipl, ByteOrder, PiPL, PiPLError};
use super::rsrc::{self, ResourceForkError};

/// File extensions of Windows plug-ins.
pub const PE_EXTENSIONS: [&str; 2] = [".8bf", ".eff"];
/// Directory extensions of macOS bundles.
pub const BUNDLE_EXTENSIONS: [&str; 2] = [".plugin", ".8bf"];
/// File extensions of native libraries that may have a `.pipl` sidecar.
pub const NATIVE_EXTENSIONS: [&str; 3] = [".so", ".dylib", ".dll"];
/// The extension appended to a native library's name for its sidecar.
pub const SIDECAR_EXTENSION: &str = ".pipl";
/// The resource id given to a sidecar PiPL.
pub const NATIVE_RESOURCE_ID: i32 = 16000;
/// The architecture name of native libraries.
pub const NATIVE_ARCH: &str = "native";

/// Mach-O CPU type for x86-64.
pub const MACHO_CPU_X86_64: u32 = 0x0100_0007;
/// Mach-O CPU type for arm64.
pub const MACHO_CPU_ARM64: u32 = 0x0100_000C;

const MACHO_FAT_MAGIC: u32 = 0xCAFE_BABE;
const MACHO_FAT_MAGIC_64: u32 = 0xCAFE_BABF;
const MACHO_MAGIC_64: u32 = 0xFEED_FACF;
const MACHO_FAT_ARCH_SIZE: usize = 20;
const MACHO_FAT_ARCH_64_SIZE: usize = 32;
const MACHO_FAT_HEADER_SIZE: usize = 8;
/// Fat headers listing more slices than this are cut off.
const MACHO_MAX_ARCHS: usize = 32;
/// How much of a Mach-O file is read to find its architectures.
const MACHO_HEADER_READ: u64 = 4096;

/// The resource type holding PiPLs in Windows DLLs.
const PE_PIPL_TYPE: &str = "PIPL";
/// The resource type holding PiPLs in Mac resource maps.
const MAC_PIPL_TYPE: &[u8; 4] = b"PiPL";
const RSRC_EXTENSION: &str = ".rsrc";
/// Resource maps store data offsets in 24 bits, so a valid one is at most
/// 16 MiB of data plus its map.
const MAX_RESOURCE_FILE: u64 = 32 * 1024 * 1024;
/// `Info.plist` files and `.pipl` sidecars are a few kilobytes.
const MAX_METADATA_FILE: u64 = 1024 * 1024;

/// Reads a whole file, refusing one larger than `limit` bytes.
///
/// Plug-in folders are untrusted: a huge file, or a link to a device such
/// as `/dev/zero`, must fail the scan of that plug-in, not exhaust memory.
pub(crate) fn read_capped(path: &Path, limit: u64) -> std::io::Result<Vec<u8>> {
    // Check before opening too: opening a FIFO blocks until a writer shows
    // up, which would hang the scan before the open file can be checked.
    require_regular_file(path, &fs::metadata(path)?)?;
    let file = fs::File::open(path)?;
    require_regular_file(path, &file.metadata()?)?;
    let mut data = Vec::new();
    file.take(limit.saturating_add(1)).read_to_end(&mut data)?;
    if data.len() as u64 > limit {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("{} is larger than {limit} bytes", path.display()),
        ));
    }
    Ok(data)
}

fn require_regular_file(path: &Path, metadata: &fs::Metadata) -> std::io::Result<()> {
    if metadata.is_file() {
        return Ok(());
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::InvalidInput,
        format!("{} is not a regular file", path.display()),
    ))
}

const INFO_PLIST_KEY: &str = "<key>CFBundleExecutable</key>";
const PLIST_STRING_OPEN: &str = "<string>";
const PLIST_STRING_CLOSE: &str = "</string>";
const BINARY_PLIST_MAGIC: &[u8] = b"bplist";
const PYTHON_NONE: &str = "None";
const DEFAULT_CATEGORY: &str = "Other";

/// The kind of file a plug-in was read from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum PluginFormat {
    /// A Windows DLL.
    #[serde(rename = "pe")]
    Pe,
    /// A macOS bundle directory.
    #[serde(rename = "mac-bundle")]
    MacBundle,
    /// A native library with a `.pipl` sidecar.
    #[serde(rename = "native")]
    Native,
}

/// Raised when a candidate plug-in can't be read.
#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct DiscoveryError(pub String);

impl From<std::io::Error> for DiscoveryError {
    fn from(error: std::io::Error) -> Self {
        Self(error.to_string())
    }
}

impl From<PeError> for DiscoveryError {
    fn from(error: PeError) -> Self {
        Self(error.0)
    }
}

impl From<PiPLError> for DiscoveryError {
    fn from(error: PiPLError) -> Self {
        Self(error.0)
    }
}

impl From<ResourceForkError> for DiscoveryError {
    fn from(error: ResourceForkError) -> Self {
        Self(error.0)
    }
}

/// One filter: a single PiPL inside a plug-in file.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PluginInfo {
    /// The plug-in file or bundle directory.
    pub path: String,
    /// The file holding the code (the bundle executable for Mac bundles).
    pub binary: String,
    pub format: PluginFormat,
    /// Architectures present in the binary.
    pub archs: Vec<String>,
    pub pipl: PiPL,
    #[serde(default)]
    pub file_size: u64,
    /// Modification time in seconds since the Unix epoch.
    #[serde(default)]
    pub file_mtime: f64,
    /// Why the plug-in can't run, if known at discovery time.
    #[serde(default)]
    pub error: Option<String>,
    /// Free-form data attached by callers.
    #[serde(default)]
    pub extra: BTreeMap<String, serde_json::Value>,
}

impl PluginInfo {
    /// The menu name, or the file name without its extension.
    pub fn name(&self) -> String {
        if !self.pipl.name.is_empty() {
            return self.pipl.name.clone();
        }
        Path::new(&self.path)
            .file_stem()
            .map(|stem| stem.to_string_lossy().into_owned())
            .unwrap_or_default()
    }

    /// The menu category, or `"Other"`.
    pub fn category(&self) -> String {
        if self.pipl.category.is_empty() {
            return DEFAULT_CATEGORY.to_owned();
        }
        self.pipl.category.clone()
    }

    /// A stable identifier for menus, settings and parameter storage.
    ///
    /// The format is `path|resource_id|name`, with a missing resource id
    /// written as `None` to match the Python host.
    pub fn key(&self) -> String {
        let resource_id = self
            .pipl
            .resource_id
            .map_or_else(|| PYTHON_NONE.to_owned(), |id| id.to_string());
        format!("{}|{}|{}", self.path, resource_id, self.name())
    }

    /// The entry point name for an architecture.
    pub fn entry_point(&self, arch: &str) -> Option<&str> {
        self.pipl.entry_points.get(arch).map(String::as_str)
    }

    /// Architectures that have both code in the binary and an entry point.
    pub fn runnable_archs(&self) -> Vec<String> {
        self.archs
            .iter()
            .filter(|arch| self.pipl.entry_points.contains_key(arch.as_str()))
            .cloned()
            .collect()
    }
}

/// Returns the plug-in architecture name for this build of the host.
pub fn host_arch() -> &'static str {
    if cfg!(all(windows, target_arch = "x86_64")) {
        "win-x64"
    } else if cfg!(all(windows, target_arch = "x86")) {
        "win-x86"
    } else if cfg!(all(windows, target_arch = "aarch64")) {
        "win-arm64"
    } else if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        "mac-arm64"
    } else if cfg!(all(target_os = "macos", target_arch = "x86_64")) {
        "mac-x64"
    } else {
        NATIVE_ARCH
    }
}

fn macho_cpu_arch(cpu: u32) -> Option<&'static str> {
    match cpu {
        MACHO_CPU_X86_64 => Some("mac-x64"),
        MACHO_CPU_ARM64 => Some("mac-arm64"),
        _ => None,
    }
}

fn read_word(header: &[u8], pos: usize, big_endian: bool) -> Option<u32> {
    let bytes: [u8; 4] = header.get(pos..pos.checked_add(4)?)?.try_into().ok()?;
    Some(if big_endian {
        u32::from_be_bytes(bytes)
    } else {
        u32::from_le_bytes(bytes)
    })
}

/// Returns the plug-in architectures present in a Mach-O file.
///
/// Understands fat (universal) files and thin 64-bit files. Anything else,
/// including unreadable files, yields an empty list.
pub fn macho_archs(path: &Path) -> Vec<String> {
    let mut header = Vec::new();
    let read =
        fs::File::open(path).and_then(|file| file.take(MACHO_HEADER_READ).read_to_end(&mut header));
    if read.is_err() || header.len() < MACHO_FAT_HEADER_SIZE {
        return Vec::new();
    }
    let magic_be = read_word(&header, 0, true).unwrap_or_default();
    if magic_be == MACHO_FAT_MAGIC || magic_be == MACHO_FAT_MAGIC_64 {
        let count = read_word(&header, 4, true).unwrap_or_default() as usize;
        let entry_size = if magic_be == MACHO_FAT_MAGIC {
            MACHO_FAT_ARCH_SIZE
        } else {
            MACHO_FAT_ARCH_64_SIZE
        };
        return (0..count.min(MACHO_MAX_ARCHS))
            .map_while(|index| read_word(&header, MACHO_FAT_HEADER_SIZE + index * entry_size, true))
            .filter_map(macho_cpu_arch)
            .map(str::to_owned)
            .collect();
    }
    if read_word(&header, 0, false) != Some(MACHO_MAGIC_64) {
        return Vec::new();
    }
    read_word(&header, 4, false)
        .and_then(macho_cpu_arch)
        .map(|arch| vec![arch.to_owned()])
        .unwrap_or_default()
}

/// Returns the size and modification time of a file, or zeros.
fn stat(path: &Path) -> (u64, f64) {
    let Ok(metadata) = fs::metadata(path) else {
        return (0, 0.0);
    };
    let mtime = metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map_or(0.0, |elapsed| elapsed.as_secs_f64());
    (metadata.len(), mtime)
}

/// PiPLs read from a plug-in, plus the first failure met along the way.
#[derive(Default)]
struct Parsed {
    pipls: Vec<PiPL>,
    first_error: Option<DiscoveryError>,
}

impl Parsed {
    fn note(&mut self, error: impl Into<DiscoveryError>) {
        self.first_error.get_or_insert(error.into());
    }

    /// The PiPLs, or the failure when nothing could be read. A plug-in with
    /// one broken PiPL among good ones still lists the good ones; one that
    /// yields nothing at all reports why instead of vanishing from a scan.
    fn into_result(self) -> Result<Vec<PiPL>, DiscoveryError> {
        match self.first_error {
            Some(error) if self.pipls.is_empty() => Err(error),
            _ => Ok(self.pipls),
        }
    }
}

/// Parses every resource, keeping the good PiPLs and noting broken ones.
fn parse_all(
    parsed: &mut Parsed,
    resources: impl IntoIterator<Item = (Option<i32>, Vec<u8>)>,
    order: ByteOrder,
) {
    for (resource_id, payload) in resources {
        match parse_pipl(&payload, order, resource_id) {
            Ok(pipl) => parsed.pipls.push(pipl),
            Err(error) => parsed.note(error),
        }
    }
}

fn path_string(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

/// Returns entries for every PiPL in a Windows DLL.
///
/// PiPLs that fail to parse are skipped when others parse. Resources with
/// a string name instead of a number get no resource id.
///
/// # Errors
///
/// Returns [`DiscoveryError`] when the file can't be read, is not a valid
/// PE image, or has PiPLs of which none parse.
pub fn read_pe_plugin(path: &Path) -> Result<Vec<PluginInfo>, DiscoveryError> {
    let image = PeFile::open(path)?;
    let resources = image.resources(&ResourceKey::Name(PE_PIPL_TYPE.to_owned()))?;
    let mut parsed = Parsed::default();
    parse_all(
        &mut parsed,
        resources.into_iter().map(|(key, payload)| {
            let resource_id = match key {
                ResourceKey::Id(id) => Some(i32::from(id)),
                ResourceKey::Name(_) => None,
            };
            (resource_id, payload)
        }),
        ByteOrder::Little,
    );
    let pipls = parsed.into_result()?;
    let (file_size, file_mtime) = stat(path);
    let path_text = path_string(path);
    Ok(pipls
        .into_iter()
        .map(|pipl| PluginInfo {
            path: path_text.clone(),
            binary: path_text.clone(),
            format: PluginFormat::Pe,
            archs: vec![image.arch().to_owned()],
            pipl,
            file_size,
            file_mtime,
            error: None,
            extra: BTreeMap::new(),
        })
        .collect())
}

/// Decodes the five predefined XML entities.
fn xml_unescape(text: &str) -> String {
    text.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

/// Extracts `CFBundleExecutable` from an XML property list.
///
/// This is a minimal scan rather than a full plist parser: it finds the key
/// and takes the `<string>` element that follows it. Binary property lists
/// (`bplist00`) are not understood and yield `None`, in which case the
/// caller falls back to the first file in `Contents/MacOS`, exactly as the
/// Python host does when `plistlib` fails.
fn plist_bundle_executable(data: &[u8]) -> Option<String> {
    if data.starts_with(BINARY_PLIST_MAGIC) {
        return None;
    }
    let text = std::str::from_utf8(data).ok()?;
    let after_key = &text[text.find(INFO_PLIST_KEY)? + INFO_PLIST_KEY.len()..];
    let value = after_key.trim_start().strip_prefix(PLIST_STRING_OPEN)?;
    let name = xml_unescape(value.get(..value.find(PLIST_STRING_CLOSE)?)?.trim());
    // Refuse names that would escape Contents/MacOS.
    let mut components = Path::new(&name).components();
    match (components.next(), components.next()) {
        (Some(std::path::Component::Normal(_)), None) => Some(name),
        _ => None,
    }
}

/// Returns the path of a macOS bundle's executable, or `None`.
///
/// Uses `CFBundleExecutable` from `Contents/Info.plist` when it names an
/// existing file, and otherwise the first file in `Contents/MacOS`.
pub fn bundle_executable(bundle: &Path) -> Option<PathBuf> {
    let contents = bundle.join("Contents");
    let macos_dir = contents.join("MacOS");
    let name = read_capped(&contents.join("Info.plist"), MAX_METADATA_FILE)
        .ok()
        .and_then(|data| plist_bundle_executable(&data));
    if let Some(name) = name {
        let candidate = macos_dir.join(name);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    sorted_entries(&macos_dir)
        .into_iter()
        .map(|entry| macos_dir.join(entry))
        .find(|candidate| candidate.is_file())
}

/// The names in a directory, sorted; empty when it can't be listed.
fn sorted_entries(dir: &Path) -> Vec<OsString> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut names: Vec<OsString> = entries
        .filter_map(Result::ok)
        .map(|entry| entry.file_name())
        .collect();
    names.sort();
    names
}

fn has_extension(name: &Path, extensions: &[&str]) -> bool {
    let lower = name.to_string_lossy().to_lowercase();
    extensions.iter().any(|ext| lower.ends_with(ext))
}

/// Appends a suffix to a path without treating it as an extension.
fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut text = path.as_os_str().to_owned();
    text.push(suffix);
    PathBuf::from(text)
}

/// Reads `.rsrc` files in `Contents/Resources` and the executable's
/// resource fork (which only exists on macOS). Files that can't be read
/// are noted in `parsed`.
fn bundle_resource_blobs(
    bundle: &Path,
    executable: Option<&Path>,
    parsed: &mut Parsed,
) -> Vec<Vec<u8>> {
    let resources_dir = bundle.join("Contents").join("Resources");
    let mut blobs = Vec::new();
    for name in sorted_entries(&resources_dir) {
        if !has_extension(Path::new(&name), &[RSRC_EXTENSION]) {
            continue;
        }
        match read_capped(&resources_dir.join(&name), MAX_RESOURCE_FILE) {
            Ok(data) => blobs.push(data),
            Err(error) => parsed.note(error),
        }
    }
    // The fork is absent everywhere but macOS, so failing to read it is
    // not worth reporting.
    if let Some(executable) = executable {
        let fork = executable.join("..namedfork").join("rsrc");
        if let Ok(data) = read_capped(&fork, MAX_RESOURCE_FILE) {
            if !data.is_empty() {
                blobs.push(data);
            }
        }
    }
    blobs
}

/// Returns entries for every PiPL in a macOS bundle.
///
/// Unreadable resource maps and broken PiPLs are skipped when other PiPLs
/// parse. A bundle without an executable still lists its filters, with
/// `error` set.
///
/// # Errors
///
/// Returns [`DiscoveryError`] when the bundle has resource files or PiPLs
/// but none of them could be read.
pub fn read_mac_bundle(bundle: &Path) -> Result<Vec<PluginInfo>, DiscoveryError> {
    let executable = bundle_executable(bundle);
    let mut parsed = Parsed::default();
    for blob in bundle_resource_blobs(bundle, executable.as_deref(), &mut parsed) {
        match rsrc::read_resources(&blob, MAC_PIPL_TYPE) {
            Ok(resources) => parse_all(
                &mut parsed,
                resources
                    .into_iter()
                    .map(|(id, payload)| (Some(i32::from(id)), payload)),
                ByteOrder::Big,
            ),
            Err(error) => parsed.note(error),
        }
    }
    let pipls = parsed.into_result()?;
    let (file_size, file_mtime) = stat(executable.as_deref().unwrap_or(bundle));
    let archs = executable.as_deref().map(macho_archs).unwrap_or_default();
    let binary = executable.as_deref().map(path_string).unwrap_or_default();
    let error = executable
        .is_none()
        .then(|| "bundle has no executable".to_owned());
    let path_text = path_string(bundle);
    Ok(pipls
        .into_iter()
        .map(|pipl| PluginInfo {
            path: path_text.clone(),
            binary: binary.clone(),
            format: PluginFormat::MacBundle,
            archs: archs.clone(),
            pipl,
            file_size,
            file_mtime,
            error: error.clone(),
            extra: BTreeMap::new(),
        })
        .collect())
}

/// Returns the entry for a native library with a `.pipl` sidecar.
///
/// When the sidecar declares no `native` entry point, the first entry point
/// it declares (in resource order) is used for `native`.
///
/// # Errors
///
/// Returns [`DiscoveryError`] when the sidecar can't be read or parsed.
pub fn read_native_plugin(path: &Path) -> Result<Vec<PluginInfo>, DiscoveryError> {
    let data = read_capped(&with_suffix(path, SIDECAR_EXTENSION), MAX_METADATA_FILE)?;
    let mut pipl = parse_pipl(&data, ByteOrder::Little, Some(NATIVE_RESOURCE_ID))?;
    if !pipl.entry_points.contains_key(NATIVE_ARCH) {
        // Native fixtures reuse whichever entry point name they declare first.
        let first = pipl::parse_properties(&data, ByteOrder::Little)?
            .iter()
            .filter_map(|property| pipl::entry_point_arch(property.key))
            .find_map(|arch| pipl.entry_points.get(arch).cloned());
        if let Some(name) = first {
            pipl.entry_points.insert(NATIVE_ARCH.to_owned(), name);
        }
    }
    let (file_size, file_mtime) = stat(path);
    let path_text = path_string(path);
    Ok(vec![PluginInfo {
        path: path_text.clone(),
        binary: path_text,
        format: PluginFormat::Native,
        archs: vec![NATIVE_ARCH.to_owned()],
        pipl,
        file_size,
        file_mtime,
        error: None,
        extra: BTreeMap::new(),
    }])
}

/// Returns true for a directory with a `Contents/Info.plist`.
pub fn is_bundle(path: &Path) -> bool {
    path.is_dir() && path.join("Contents").join("Info.plist").is_file()
}

/// Reads one candidate path and returns its filters (possibly none).
///
/// Paths that are not plug-ins give an empty list.
///
/// # Errors
///
/// Returns [`DiscoveryError`] when a plug-in file is unreadable or
/// malformed.
pub fn read_plugin_path(path: &Path) -> Result<Vec<PluginInfo>, DiscoveryError> {
    if path.is_dir() {
        if has_extension(path, &BUNDLE_EXTENSIONS) || is_bundle(path) {
            return read_mac_bundle(path);
        }
        return Ok(Vec::new());
    }
    if with_suffix(path, SIDECAR_EXTENSION).is_file() && has_extension(path, &NATIVE_EXTENSIONS) {
        return read_native_plugin(path);
    }
    if has_extension(path, &PE_EXTENSIONS) && pe::is_pe_file(path) {
        return read_pe_plugin(path);
    }
    Ok(Vec::new())
}

/// Returns plug-in candidate paths below the given folders, recursively.
///
/// Symbolic links are followed, each real directory is visited once, and
/// bundles are listed but never descended into. Within a directory,
/// bundles come first, then files, then subdirectories, each sorted.
pub fn iter_candidates(folders: &[PathBuf]) -> Vec<PathBuf> {
    let mut seen = HashSet::new();
    let mut found = Vec::new();
    for folder in folders {
        if folder.as_os_str().is_empty() || !folder.is_dir() {
            continue;
        }
        walk(folder, &mut seen, &mut found);
    }
    found
}

fn walk(root: &Path, seen: &mut HashSet<PathBuf>, found: &mut Vec<PathBuf>) {
    let real_root = fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    if !seen.insert(real_root) {
        return;
    }
    let mut bundles = Vec::new();
    let mut dirs = Vec::new();
    let mut files = Vec::new();
    for name in sorted_entries(root) {
        let path = root.join(&name);
        if !path.is_dir() {
            files.push(name);
        } else if has_extension(Path::new(&name), &BUNDLE_EXTENSIONS) {
            bundles.push(path);
        } else {
            dirs.push(path);
        }
    }
    found.extend(bundles);
    for name in files {
        let path = root.join(&name);
        let name = Path::new(&name);
        let native = has_extension(name, &NATIVE_EXTENSIONS)
            && with_suffix(&path, SIDECAR_EXTENSION).is_file();
        if has_extension(name, &PE_EXTENSIONS) || native {
            found.push(path);
        }
    }
    for dir in dirs {
        walk(&dir, seen, found);
    }
}

/// Scans folders and returns every filter found.
///
/// Plug-ins that fail to read are reported through `on_error` and skipped.
/// Non-filter plug-ins (other kinds) are left out.
pub fn scan(
    folders: &[PathBuf],
    on_error: &mut dyn FnMut(&Path, &DiscoveryError),
) -> Vec<PluginInfo> {
    let mut found = Vec::new();
    for path in iter_candidates(folders) {
        match read_plugin_path(&path) {
            Ok(entries) => found.extend(entries.into_iter().filter(|entry| entry.pipl.is_filter())),
            Err(error) => on_error(&path, &error),
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host::constants::{self as c, fourcc};
    use crate::host::pe::test_support::build_pe;
    use crate::host::pipl::{build_pipl, int_property, pascal};

    fn filter_pipl(name: &str, entry_key: &[u8; 4], entry: &str, order: ByteOrder) -> Vec<u8> {
        let mut entry_bytes = entry.as_bytes().to_vec();
        entry_bytes.push(0);
        build_pipl(
            &[
                (c::PIPL_KIND, int_property(c::FILTER_KIND as i32, order)),
                (c::PIPL_NAME, pascal(name)),
                (c::PIPL_CATEGORY, pascal("Durable")),
                (fourcc(entry_key), entry_bytes),
            ],
            order,
        )
    }

    fn fat_header(archs: &[u32]) -> Vec<u8> {
        let mut header = MACHO_FAT_MAGIC.to_be_bytes().to_vec();
        header.extend_from_slice(&(archs.len() as u32).to_be_bytes());
        for cpu in archs {
            header.extend_from_slice(&cpu.to_be_bytes());
            header.extend_from_slice(&[0; 16]);
        }
        header
    }

    fn info_plist(executable: &str) -> String {
        format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<plist version=\"1.0\">\n<dict>\n\
       \t<key>CFBundleExecutable</key>\n\t<string>{executable}</string>\n</dict>\n</plist>\n"
        )
    }

    fn make_bundle(root: &Path, name: &str, archs: &[u32]) -> PathBuf {
        let bundle = root.join(format!("{name}.plugin"));
        fs::create_dir_all(bundle.join("Contents/MacOS")).unwrap();
        fs::create_dir_all(bundle.join("Contents/Resources")).unwrap();
        fs::write(bundle.join("Contents/Info.plist"), info_plist(name)).unwrap();
        fs::write(bundle.join("Contents/MacOS").join(name), fat_header(archs)).unwrap();
        let resource = rsrc::build_resource_file(&[(
            *b"PiPL",
            vec![(16000, filter_pipl(name, b"ma64", "Main", ByteOrder::Big))],
        )]);
        fs::write(
            bundle
                .join("Contents/Resources")
                .join(format!("{name}.rsrc")),
            resource,
        )
        .unwrap();
        bundle
    }

    #[test]
    fn mac_bundle_discovery() {
        let dir = tempfile::tempdir().unwrap();
        let bundle = make_bundle(dir.path(), "Glow", &[MACHO_CPU_X86_64, MACHO_CPU_ARM64]);
        let entries = read_plugin_path(&bundle).unwrap();
        assert_eq!(entries.len(), 1);
        let entry = &entries[0];
        assert_eq!(entry.format, PluginFormat::MacBundle);
        assert_eq!(entry.name(), "Glow");
        assert_eq!(entry.archs, vec!["mac-x64", "mac-arm64"]);
        assert_eq!(entry.runnable_archs(), vec!["mac-arm64"]);
        assert!(Path::new(&entry.binary).ends_with("MacOS/Glow"));
        assert_eq!(entry.pipl.resource_id, Some(16000));
        assert!(entry.error.is_none());
    }

    #[test]
    fn bundle_without_executable_reports_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let bundle = make_bundle(dir.path(), "Empty", &[]);
        fs::remove_file(bundle.join("Contents/MacOS/Empty")).unwrap();
        let entries = read_mac_bundle(&bundle).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].binary, "");
        assert!(entries[0].archs.is_empty());
        assert_eq!(
            entries[0].error.as_deref(),
            Some("bundle has no executable")
        );
    }

    #[test]
    fn bundle_with_broken_resources_reports_why() {
        let dir = tempfile::tempdir().unwrap();
        let bundle = make_bundle(dir.path(), "Broken", &[MACHO_CPU_ARM64]);
        fs::write(bundle.join("Contents/Resources/Broken.rsrc"), b"garbage").unwrap();
        assert!(read_mac_bundle(&bundle).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn a_fifo_is_refused_without_blocking() {
        let dir = tempfile::tempdir().unwrap();
        let fifo = dir.path().join("pipe.8bf");
        let status = std::process::Command::new("mkfifo")
            .arg(&fifo)
            .status()
            .unwrap();
        assert!(status.success());
        assert!(read_capped(&fifo, 16).is_err());
        assert!(!pe::is_pe_file(&fifo));
    }

    #[test]
    fn bundle_executable_fallbacks() {
        let dir = tempfile::tempdir().unwrap();
        let bundle = make_bundle(dir.path(), "Glow", &[]);
        let macos = bundle.join("Contents/MacOS");
        fs::write(macos.join("Another"), b"").unwrap();
        assert_eq!(bundle_executable(&bundle), Some(macos.join("Glow")));
        // A binary plist is not understood: the first file is used.
        fs::write(bundle.join("Contents/Info.plist"), b"bplist00\x01\x02").unwrap();
        assert_eq!(bundle_executable(&bundle), Some(macos.join("Another")));
        // A plist naming a missing or escaping file also falls back.
        fs::write(bundle.join("Contents/Info.plist"), info_plist("Missing")).unwrap();
        assert_eq!(bundle_executable(&bundle), Some(macos.join("Another")));
        fs::write(bundle.join("Contents/Info.plist"), info_plist("../../x")).unwrap();
        assert_eq!(bundle_executable(&bundle), Some(macos.join("Another")));
        fs::remove_dir_all(&macos).unwrap();
        assert_eq!(bundle_executable(&bundle), None);
    }

    #[test]
    fn plist_scanning() {
        assert_eq!(
            plist_bundle_executable(info_plist("A &amp; B").as_bytes()),
            Some("A & B".into())
        );
        assert_eq!(
            plist_bundle_executable(b"<key>CFBundleExecutable</key><string>Run"),
            None
        );
        assert_eq!(
            plist_bundle_executable(b"<key>CFBundleExecutable</key><true/>"),
            None
        );
        assert_eq!(
            plist_bundle_executable(b"<key>CFBundleExecutable</key>"),
            None
        );
        assert_eq!(plist_bundle_executable(b"\xff\xfe"), None);
        assert_eq!(plist_bundle_executable(info_plist("").as_bytes()), None);
    }

    #[test]
    fn thin_macho_arch() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("thin");
        let mut data = MACHO_MAGIC_64.to_le_bytes().to_vec();
        data.extend_from_slice(&MACHO_CPU_ARM64.to_le_bytes());
        data.extend_from_slice(&[0; 24]);
        fs::write(&path, data).unwrap();
        assert_eq!(macho_archs(&path), vec!["mac-arm64"]);
    }

    #[test]
    fn macho_edge_cases() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bin");
        assert!(macho_archs(&path).is_empty());
        fs::write(&path, b"short").unwrap();
        assert!(macho_archs(&path).is_empty());
        fs::write(&path, b"\x7fELF\x02\x01\x01\x00").unwrap();
        assert!(macho_archs(&path).is_empty());
        // A fat header claiming more slices than it holds stops at the end.
        let mut fat = fat_header(&[MACHO_CPU_ARM64]);
        fat[4..8].copy_from_slice(&u32::MAX.to_be_bytes());
        fs::write(&path, fat).unwrap();
        assert_eq!(macho_archs(&path), vec!["mac-arm64"]);
        // Unknown CPU types are skipped.
        fs::write(&path, fat_header(&[0x0000_0007, MACHO_CPU_X86_64])).unwrap();
        assert_eq!(macho_archs(&path), vec!["mac-x64"]);
    }

    #[test]
    fn native_plugin_with_sidecar() {
        let dir = tempfile::tempdir().unwrap();
        let library = dir.path().join("libinvert.so");
        fs::write(&library, b"\x7fELF").unwrap();
        fs::write(
            with_suffix(&library, SIDECAR_EXTENSION),
            filter_pipl("Native", b"8664", "InvertMain", ByteOrder::Little),
        )
        .unwrap();
        let found = scan(&[dir.path().to_path_buf()], &mut |path, error| {
            panic!("{path:?}: {error}")
        });
        let names: Vec<String> = found.iter().map(PluginInfo::name).collect();
        assert_eq!(names, vec!["Native"]);
        assert_eq!(found[0].entry_point("native"), Some("InvertMain"));
        assert_eq!(found[0].format, PluginFormat::Native);
        assert_eq!(found[0].pipl.resource_id, Some(NATIVE_RESOURCE_ID));
        assert_eq!(found[0].file_size, 4);
        assert!(found[0].file_mtime > 0.0);
    }

    #[test]
    fn native_entry_uses_first_declared_entry_point() {
        let dir = tempfile::tempdir().unwrap();
        let library = dir.path().join("lib.dylib");
        fs::write(&library, b"").unwrap();
        let pipl = build_pipl(
            &[
                (
                    c::PIPL_KIND,
                    int_property(c::FILTER_KIND as i32, ByteOrder::Little),
                ),
                (fourcc(b"wx86"), b"Second\0".to_vec()),
                (fourcc(b"8664"), b"First\0".to_vec()),
            ],
            ByteOrder::Little,
        );
        // wx86 comes first in the resource even though win-x64 sorts first.
        fs::write(with_suffix(&library, SIDECAR_EXTENSION), pipl).unwrap();
        let entries = read_plugin_path(&library).unwrap();
        assert_eq!(entries[0].entry_point(NATIVE_ARCH), Some("Second"));
    }

    #[test]
    fn capped_reads_refuse_large_files_and_non_files() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("x.rsrc");
        fs::write(&file, [0u8; 10]).unwrap();
        assert_eq!(read_capped(&file, 10).unwrap().len(), 10);
        assert!(read_capped(&file, 9).is_err());
        assert!(read_capped(dir.path(), 10).is_err());
        #[cfg(unix)]
        assert!(read_capped(Path::new("/dev/zero"), 10).is_err());
    }

    #[test]
    fn broken_sidecar_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let library = dir.path().join("lib.so");
        fs::write(&library, b"").unwrap();
        fs::write(with_suffix(&library, SIDECAR_EXTENSION), b"bad").unwrap();
        assert!(read_plugin_path(&library).is_err());
    }

    #[test]
    fn scan_skips_non_plugins_and_reports_errors() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("readme.txt"), "hello").unwrap();
        fs::write(dir.path().join("lonely.so"), "no sidecar").unwrap();
        fs::write(dir.path().join("text.8bf"), "not MZ").unwrap();
        let mut broken = b"MZ".to_vec();
        broken.extend_from_slice(&[0; 10]);
        fs::write(dir.path().join("broken.8bf"), broken).unwrap();
        let mut errors = Vec::new();
        let found = scan(&[dir.path().to_path_buf()], &mut |path, _| {
            errors.push(path.to_path_buf())
        });
        assert!(found.is_empty());
        assert_eq!(errors, vec![dir.path().join("broken.8bf")]);
    }

    #[test]
    fn pe_plugin_discovery() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("Filters.8BF");
        let mut hidden_kind = filter_pipl("Not A Filter", b"8664", "Other", ByteOrder::Little);
        hidden_kind[26..30]
            .copy_from_slice(&int_property(fourcc(b"8BEF") as i32, ByteOrder::Little));
        fs::write(
            &path,
            build_pe(&[
                (
                    16000,
                    filter_pipl("Zig Invert", b"8664", "PluginMain", ByteOrder::Little),
                ),
                (
                    16001,
                    filter_pipl("Zig Blur", b"8664", "BlurMain", ByteOrder::Little),
                ),
                (16002, b"broken".to_vec()),
                (16003, hidden_kind),
            ]),
        )
        .unwrap();
        let entries = read_plugin_path(&path).unwrap();
        assert_eq!(entries.len(), 3);
        let mut names: Vec<String> = entries.iter().map(PluginInfo::name).collect();
        names.sort();
        assert_eq!(names, vec!["Not A Filter", "Zig Blur", "Zig Invert"]);
        assert!(entries
            .iter()
            .all(|e| e.format == PluginFormat::Pe && e.archs == ["win-x64"]));
        let found = scan(&[dir.path().to_path_buf()], &mut |path, error| {
            panic!("{path:?}: {error}")
        });
        let mut entry_points: Vec<&str> = found
            .iter()
            .filter_map(|e| e.entry_point("win-x64"))
            .collect();
        entry_points.sort_unstable();
        assert_eq!(entry_points, vec!["BlurMain", "PluginMain"]);
        assert_eq!(
            found[0].key(),
            format!("{}|16000|Zig Invert", path.display())
        );
    }

    #[test]
    fn candidates_order_and_bundles() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        make_bundle(root, "Zed", &[]);
        fs::create_dir_all(root.join("sub/deeper")).unwrap();
        fs::write(root.join("b.8bf"), b"").unwrap();
        fs::write(root.join("a.EFF"), b"").unwrap();
        fs::write(root.join("sub/deeper/c.8bf"), b"").unwrap();
        fs::write(root.join("notes.txt"), b"").unwrap();
        let candidates = iter_candidates(&[
            root.to_path_buf(),
            root.to_path_buf(),
            PathBuf::new(),
            root.join("missing"),
        ]);
        assert_eq!(
            candidates,
            vec![
                root.join("Zed.plugin"),
                root.join("a.EFF"),
                root.join("b.8bf"),
                root.join("sub/deeper/c.8bf"),
            ]
        );
    }

    #[cfg(unix)]
    #[test]
    fn symlink_loops_are_visited_once() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::create_dir(root.join("sub")).unwrap();
        fs::write(root.join("sub/x.8bf"), b"").unwrap();
        std::os::unix::fs::symlink(root, root.join("sub/loop")).unwrap();
        assert_eq!(
            iter_candidates(&[root.to_path_buf()]),
            vec![root.join("sub/x.8bf")]
        );
    }

    #[test]
    fn directories_that_are_not_bundles_are_ignored() {
        let dir = tempfile::tempdir().unwrap();
        assert!(read_plugin_path(dir.path()).unwrap().is_empty());
        assert!(read_plugin_path(&dir.path().join("missing.txt"))
            .unwrap()
            .is_empty());
    }

    fn sample_info() -> PluginInfo {
        PluginInfo {
            path: "/plugins/Glow.8bf".into(),
            binary: "/plugins/Glow.8bf".into(),
            format: PluginFormat::Pe,
            archs: vec!["win-x64".into(), "win-x86".into()],
            pipl: PiPL {
                entry_points: [("win-x64".to_string(), "Main".to_string())].into(),
                ..PiPL::default()
            },
            file_size: 10,
            file_mtime: 1.5,
            error: None,
            extra: BTreeMap::new(),
        }
    }

    #[test]
    fn plugin_info_accessors() {
        let mut info = sample_info();
        assert_eq!(info.name(), "Glow");
        assert_eq!(info.category(), "Other");
        assert_eq!(info.key(), "/plugins/Glow.8bf|None|Glow");
        assert_eq!(info.entry_point("win-x64"), Some("Main"));
        assert_eq!(info.entry_point("win-x86"), None);
        assert_eq!(info.runnable_archs(), vec!["win-x64"]);
        info.pipl.name = "Shine".into();
        info.pipl.category = "Light".into();
        info.pipl.resource_id = Some(-5);
        assert_eq!(info.key(), "/plugins/Glow.8bf|-5|Shine");
        assert_eq!(info.category(), "Light");
    }

    #[test]
    fn plugin_info_serde() {
        let info = sample_info();
        let json = serde_json::to_value(&info).unwrap();
        assert_eq!(json["format"], "pe");
        assert_eq!(
            serde_json::to_value(PluginFormat::MacBundle).unwrap(),
            "mac-bundle"
        );
        assert_eq!(
            serde_json::to_value(PluginFormat::Native).unwrap(),
            "native"
        );
        let back: PluginInfo = serde_json::from_value(json).unwrap();
        assert_eq!(back, info);
    }

    #[test]
    fn host_arch_is_known() {
        let known = [
            "win-x64",
            "win-x86",
            "win-arm64",
            "mac-x64",
            "mac-arm64",
            "native",
        ];
        assert!(known.contains(&host_arch()));
        #[cfg(target_os = "linux")]
        assert_eq!(host_arch(), "native");
    }
}
