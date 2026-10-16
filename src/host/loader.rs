//! Loading filter code and resolving its entry point.

use std::ffi::c_void;

use super::discovery::{host_arch, PluginFormat, PluginInfo};
use super::structs::FilterEntryProc;

/// A filter's code could not be loaded.
#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct LoadError(pub String);

/// What unloading a filter left behind.
#[derive(Debug)]
pub struct Unloaded {
    /// What became of the module, for the log.
    pub note: String,
    /// Threads the filter started that are still running now its code is
    /// gone. They crash the host when they next wake.
    pub stray_threads: usize,
}

/// A loaded filter binary and its entry point.
///
/// The library stays loaded for as long as this value lives. Filters keep
/// globals between runs, so hosts keep modules around rather than
/// reloading them for every run.
pub struct FilterModule {
    pub binary: String,
    pub entry_name: String,
    entry: FilterEntryProc,
    _library: Option<libloading::Library>,
}

impl FilterModule {
    /// Loads the code for `info` built for `arch` (default: this binary's).
    ///
    /// # Errors
    ///
    /// Fails when the filter has no entry point or no code for `arch`, or
    /// when the operating system cannot load the binary.
    pub fn for_plugin(info: &PluginInfo, arch: Option<&str>) -> Result<Self, LoadError> {
        let arch = arch.unwrap_or(host_arch());
        let Some(entry_name) = info.entry_point(arch) else {
            let available: Vec<&str> = info.pipl.entry_points.keys().map(String::as_str).collect();
            return Err(LoadError(format!(
                "{} has no entry point for {arch} (available: {})",
                info.name(),
                if available.is_empty() {
                    "none".to_string()
                } else {
                    available.join(", ")
                }
            )));
        };
        if !info.archs.iter().any(|candidate| candidate == arch) {
            return Err(LoadError(format!(
                "{} contains no {arch} code (has: {})",
                info.name(),
                if info.archs.is_empty() {
                    "unknown".to_string()
                } else {
                    info.archs.join(", ")
                }
            )));
        }
        if info.format == PluginFormat::MacBundle {
            return Self::load_bundle(info, entry_name);
        }
        Self::load_library(&info.binary, entry_name)
    }

    /// Loads a shared library and resolves `entry_name` in it.
    ///
    /// # Errors
    ///
    /// Fails when the library cannot be loaded or does not export the symbol.
    pub fn load_library(binary: &str, entry_name: &str) -> Result<Self, LoadError> {
        let library = open_library(binary)?;
        let mut symbol_name = entry_name.as_bytes().to_vec();
        symbol_name.push(0);
        // SAFETY: the symbol is declared by the filter's PiPL as its entry
        // point, which the plug-in API defines with this C signature.
        let entry = unsafe { library.get::<FilterEntryProc>(&symbol_name) }
            .map(|symbol| *symbol)
            .map_err(|error| {
                LoadError(format!(
                    "{} does not export {entry_name}: {error}",
                    file_name(binary)
                ))
            })?;
        Ok(Self {
            binary: binary.to_string(),
            entry_name: entry_name.to_string(),
            entry,
            _library: Some(library),
        })
    }

    #[cfg(target_os = "macos")]
    fn load_bundle(info: &PluginInfo, entry_name: &str) -> Result<Self, LoadError> {
        let entry = bundle::load(&info.path, entry_name)?;
        Ok(Self {
            binary: info.binary.clone(),
            entry_name: entry_name.to_string(),
            entry,
            _library: None,
        })
    }

    #[cfg(not(target_os = "macos"))]
    fn load_bundle(info: &PluginInfo, entry_name: &str) -> Result<Self, LoadError> {
        // Outside macOS a bundle can only be loaded through its executable.
        Self::load_library(&info.binary, entry_name)
    }

    /// Unloads the filter's code and says what became of it.
    ///
    /// On Windows the address range the module occupied is then reserved
    /// with no access. Photoshop maps other DLLs into a freed filter's range,
    /// so code the filter left running (a thread pool, a callback) runs into
    /// whatever lands there; reserving the range makes that fault at once,
    /// and the next load goes to a new address. A range that is still
    /// mapped means the filter pinned itself or something else holds it.
    pub fn unload(self) -> Unloaded {
        let name = file_name(&self.binary).to_string();
        let Some(library) = self._library else {
            return Unloaded {
                note: format!("{name} stays loaded (macOS bundles are never unloaded)"),
                stray_threads: 0,
            };
        };
        unload_library(library, &name)
    }

    /// Calls the entry point with one selector.
    ///
    /// # Safety
    ///
    /// `record` must point at the record type the selector expects
    /// (an `AboutRecord` for the About selector, a `FilterRecord` for the
    /// others), and everything it references must stay valid for the call.
    pub unsafe fn call(&self, selector: i16, record: *mut c_void, data: &mut isize) -> i16 {
        let mut result: i16 = 0;
        // SAFETY: the caller guarantees `record`; `data` and `result` are
        // valid for writes for the duration of the call.
        unsafe { (self.entry)(selector, record, data, &mut result) };
        result
    }
}

fn file_name(path: &str) -> &str {
    std::path::Path::new(path)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(path)
}

#[cfg(windows)]
fn open_library(binary: &str) -> Result<libloading::Library, LoadError> {
    use libloading::os::windows::{Library, LOAD_WITH_ALTERED_SEARCH_PATH};
    // Resolve the plug-in's own DLL dependencies from its folder first, as
    // Photoshop does.
    // SAFETY: loading a filter runs its DllMain; running filter code is the
    // purpose of this host.
    unsafe { Library::load_with_flags(binary, LOAD_WITH_ALTERED_SEARCH_PATH) }
        .map(Into::into)
        .map_err(|error| LoadError(format!("could not load {binary}: {error}")))
}

#[cfg(windows)]
fn unload_library(library: libloading::Library, name: &str) -> Unloaded {
    use windows_sys::Win32::System::Memory::{
        VirtualAlloc, VirtualFree, MEM_RELEASE, MEM_RESERVE, PAGE_NOACCESS,
    };

    let note = |note: String| Unloaded {
        note,
        stray_threads: 0,
    };
    let handle = libloading::os::windows::Library::from(library).into_raw();
    let base = handle as *mut c_void;
    // SAFETY: a module handle is its base address, where the loader
    // mapped the PE headers.
    let size = unsafe { image_size(base.cast()) };
    // SAFETY: `handle` came from into_raw and is given up here.
    if let Err(error) = unsafe { libloading::os::windows::Library::from_raw(handle) }.close() {
        return note(format!("could not unload {name}: {error}"));
    }
    // SAFETY: reserving address space leaves existing mappings alone; it
    // fails if any of the range is still in use.
    let fence = unsafe { VirtualAlloc(base, size, MEM_RESERVE, PAGE_NOACCESS) };
    if fence.is_null() {
        return note(format!(
            "{name} is still mapped at {base:p} after unloading (pinned?)"
        ));
    }
    if fence != base {
        // SAFETY: the reservation just made.
        unsafe { VirtualFree(fence, 0, MEM_RELEASE) };
        return note(format!(
            "{name} unloaded; could not reserve its range at {base:p}"
        ));
    }
    let stray_threads = threads_started_in(base as usize, size);
    let note = if stray_threads == 0 {
        format!("{name} unloaded; its range at {base:p} (+{size:#x}) is now no-access")
    } else {
        format!(
            "{name} unloaded with {stray_threads} of its threads still running \
             in its freed code at {base:p} (+{size:#x})"
        )
    };
    Unloaded {
        note,
        stray_threads,
    }
}

/// `SizeOfImage` from the PE headers at `base`.
///
/// # Safety
///
/// `base` must be the base of a mapped PE image.
#[cfg(windows)]
unsafe fn image_size(base: *const u8) -> usize {
    // SAFETY: e_lfanew is at 0x3c; SizeOfImage is 0x50 past the PE signature.
    unsafe {
        let pe = base.add(base.add(0x3c).cast::<u32>().read_unaligned() as usize);
        pe.add(0x50).cast::<u32>().read_unaligned() as usize
    }
}

/// How many of this process's threads started at an address in
/// `base..base + size`.
#[cfg(windows)]
fn threads_started_in(base: usize, size: usize) -> usize {
    use windows_sys::Wdk::System::Threading::{
        NtQueryInformationThread, ThreadQuerySetWin32StartAddress,
    };
    use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Thread32First, Thread32Next, TH32CS_SNAPTHREAD, THREADENTRY32,
    };
    use windows_sys::Win32::System::Threading::{
        GetCurrentProcessId, OpenThread, THREAD_QUERY_INFORMATION,
    };

    let range = base..base + size;
    // SAFETY (whole function): plain Win32 calls on handles opened and
    // closed here, writing only to locals sized for them.
    let process = unsafe { GetCurrentProcessId() };
    let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) };
    if snapshot == INVALID_HANDLE_VALUE {
        return 0;
    }
    let mut entry: THREADENTRY32 = unsafe { std::mem::zeroed() };
    entry.dwSize = std::mem::size_of::<THREADENTRY32>() as u32;
    let mut count = 0;
    let mut more = unsafe { Thread32First(snapshot, &mut entry) } != 0;
    while more {
        if entry.th32OwnerProcessID == process {
            let thread = unsafe { OpenThread(THREAD_QUERY_INFORMATION, 0, entry.th32ThreadID) };
            if !thread.is_null() {
                let mut start: usize = 0;
                let status = unsafe {
                    NtQueryInformationThread(
                        thread,
                        ThreadQuerySetWin32StartAddress,
                        (&raw mut start).cast(),
                        std::mem::size_of::<usize>() as u32,
                        std::ptr::null_mut(),
                    )
                };
                if status >= 0 && range.contains(&start) {
                    count += 1;
                }
                unsafe { CloseHandle(thread) };
            }
        }
        more = unsafe { Thread32Next(snapshot, &mut entry) } != 0;
    }
    unsafe { CloseHandle(snapshot) };
    count
}

#[cfg(not(windows))]
fn unload_library(library: libloading::Library, name: &str) -> Unloaded {
    let note = match library.close() {
        Ok(()) => format!("{name} unloaded"),
        Err(error) => format!("could not unload {name}: {error}"),
    };
    Unloaded {
        note,
        stray_threads: 0,
    }
}

#[cfg(unix)]
fn open_library(binary: &str) -> Result<libloading::Library, LoadError> {
    use libloading::os::unix::{Library, RTLD_LOCAL, RTLD_NOW};
    // SAFETY: loading a filter runs its initializers; running filter code
    // is the purpose of this host.
    unsafe { Library::open(Some(binary), RTLD_NOW | RTLD_LOCAL) }
        .map(Into::into)
        .map_err(|error| LoadError(format!("could not load {binary}: {error}")))
}

/// Loading macOS bundles through CoreFoundation, as Photoshop does.
///
/// Going through CFBundle rather than dlopen lets the filter find its own
/// resources with `CFBundleGetBundleWithIdentifier`.
#[cfg(target_os = "macos")]
mod bundle {
    use std::ffi::{c_char, c_void, CString};
    use std::os::unix::ffi::OsStrExt;

    use super::LoadError;
    use crate::host::structs::FilterEntryProc;

    const K_CF_STRING_ENCODING_UTF8: u32 = 0x0800_0100;

    #[link(name = "CoreFoundation", kind = "framework")]
    extern "C" {
        fn CFURLCreateFromFileSystemRepresentation(
            allocator: *const c_void,
            buffer: *const u8,
            length: isize,
            is_directory: bool,
        ) -> *const c_void;
        fn CFBundleCreate(allocator: *const c_void, url: *const c_void) -> *const c_void;
        fn CFBundleLoadExecutable(bundle: *const c_void) -> bool;
        fn CFStringCreateWithCString(
            allocator: *const c_void,
            text: *const c_char,
            encoding: u32,
        ) -> *const c_void;
        fn CFBundleGetFunctionPointerForName(
            bundle: *const c_void,
            name: *const c_void,
        ) -> *const c_void;
        fn CFRelease(object: *const c_void);
    }

    pub fn load(path: &str, entry_name: &str) -> Result<FilterEntryProc, LoadError> {
        let raw = std::path::Path::new(path).as_os_str().as_bytes();
        let length =
            isize::try_from(raw.len()).map_err(|_| LoadError(format!("path too long: {path}")))?;
        let name = CString::new(entry_name)
            .map_err(|_| LoadError(format!("invalid entry point name {entry_name:?}")))?;
        // SAFETY: plain CoreFoundation calls on valid buffers; every object
        // created here is released except the bundle, which stays loaded
        // for the life of the process.
        unsafe {
            let url = CFURLCreateFromFileSystemRepresentation(
                std::ptr::null(),
                raw.as_ptr(),
                length,
                true,
            );
            if url.is_null() {
                return Err(LoadError(format!("could not create a URL for {path}")));
            }
            let bundle = CFBundleCreate(std::ptr::null(), url);
            CFRelease(url);
            if bundle.is_null() {
                return Err(LoadError(format!("could not open bundle {path}")));
            }
            if !CFBundleLoadExecutable(bundle) {
                CFRelease(bundle);
                return Err(LoadError(format!(
                    "could not load the executable in {path}"
                )));
            }
            let cf_name = CFStringCreateWithCString(
                std::ptr::null(),
                name.as_ptr(),
                K_CF_STRING_ENCODING_UTF8,
            );
            if cf_name.is_null() {
                return Err(LoadError(format!(
                    "could not look up {entry_name} in {path}"
                )));
            }
            let address = CFBundleGetFunctionPointerForName(bundle, cf_name);
            CFRelease(cf_name);
            if address.is_null() {
                return Err(LoadError(format!("{path} does not export {entry_name}")));
            }
            Ok(std::mem::transmute::<*const c_void, FilterEntryProc>(
                address,
            ))
        }
    }
}
