//! Memory managed on behalf of filters: handles, buffers and PICA blocks.
//!
//! A Photoshop `Handle` is officially opaque, but many filters treat it as
//! a classic Mac handle: a pointer to a master pointer that points at the
//! data. [`HandleManager`] builds real master pointers so both styles work.
//!
//! All memory is owned by the managers and stays valid until the filter
//! disposes of it. Allocation failures are reported to the filter as
//! out-of-memory errors instead of aborting the process.

use std::collections::{HashMap, HashSet};
use std::ffi::c_void;

use super::constants as c;
use super::structs::Handle;

/// The free space reported to filters: generous, but safe for `int32` fields.
pub const DEFAULT_SPACE: i64 = 1 << 30;

/// The largest single allocation a filter may make: the free space the
/// host reports. Without a ceiling, a request for tens of gigabytes can
/// succeed under memory overcommit and then get the process killed while
/// the block is zeroed, instead of returning an out-of-memory error.
pub const MAX_ALLOCATION: usize = DEFAULT_SPACE as usize;

/// Allocates `size` zeroed bytes (at least one), or `None` when the size is
/// over [`MAX_ALLOCATION`] or memory is short.
fn zeroed_block(size: usize) -> Option<Vec<u8>> {
    if size > MAX_ALLOCATION {
        return None;
    }
    let size = size.max(1);
    let mut block = Vec::new();
    block.try_reserve_exact(size).ok()?;
    block.resize(size, 0);
    Some(block)
}

struct HandleRecord {
    /// The master pointer. Boxed so its address, which is the handle, never moves.
    master: Box<*mut u8>,
    block: Vec<u8>,
    size: usize,
    locks: u32,
    /// Blocks a locked handle grew out of. The filter may still hold
    /// pointers into them, so they are freed only once it unlocks.
    retired: Vec<Vec<u8>>,
}

/// Implements the Photoshop handle procs and the PICA handle suite.
#[derive(Default)]
pub struct HandleManager {
    handles: HashMap<usize, HandleRecord>,
    /// Addresses of handles this manager issued and has since freed. A
    /// filter that disposes a handle twice must not have the address
    /// treated as a Windows `HGLOBAL`: the freed master pointer came from
    /// the process heap, so `GlobalSize` accepts it and `GlobalFree` would
    /// free whatever host allocation now lives there.
    disposed: HashSet<usize>,
}

impl HandleManager {
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns true when `handle` was created by this manager and is live.
    pub fn contains(&self, handle: Handle) -> bool {
        !handle.is_null() && self.handles.contains_key(&(handle as usize))
    }

    /// Number of live handles.
    pub fn len(&self) -> usize {
        self.handles.len()
    }

    pub fn is_empty(&self) -> bool {
        self.handles.is_empty()
    }

    /// Creates a zeroed handle of `size` bytes. Returns null on failure.
    pub fn allocate(&mut self, size: i32) -> Handle {
        let Ok(size) = usize::try_from(size) else {
            return std::ptr::null_mut();
        };
        let Some(mut block) = zeroed_block(size) else {
            return std::ptr::null_mut();
        };
        let mut master = Box::new(block.as_mut_ptr());
        let handle = (&mut *master as *mut *mut u8).cast::<c_void>();
        self.disposed.remove(&(handle as usize));
        self.handles.insert(
            handle as usize,
            HandleRecord {
                master,
                block,
                size,
                locks: 0,
                retired: Vec::new(),
            },
        );
        handle
    }

    /// Creates a handle holding a copy of `data`. Returns null on failure.
    pub fn allocate_from(&mut self, data: &[u8]) -> Handle {
        let Ok(size) = i32::try_from(data.len()) else {
            return std::ptr::null_mut();
        };
        let handle = self.allocate(size);
        if let Some(record) = self.handles.get_mut(&(handle as usize)) {
            record.block[..data.len()].copy_from_slice(data);
        }
        handle
    }

    /// Frees a handle. Returns false when the handle is not one of ours.
    pub fn dispose(&mut self, handle: Handle) -> bool {
        if handle.is_null() {
            return true;
        }
        if self.handles.remove(&(handle as usize)).is_some() {
            self.disposed.insert(handle as usize);
            return true;
        }
        self.is_foreign(handle) && global::dispose(handle)
    }

    /// True when `handle` may be a filter's own `HGLOBAL`: it is not null
    /// and not the address of a handle this manager freed.
    fn is_foreign(&self, handle: Handle) -> bool {
        !handle.is_null() && !self.disposed.contains(&(handle as usize))
    }

    /// The logical size of a handle, or 0 for an unknown handle.
    pub fn size(&self, handle: Handle) -> i32 {
        match self.handles.get(&(handle as usize)) {
            Some(record) => i32::try_from(record.size).unwrap_or(i32::MAX),
            None if self.is_foreign(handle) => global::size(handle),
            None => 0,
        }
    }

    /// Resizes a handle, keeping its contents and zeroing any new bytes.
    pub fn set_size(&mut self, handle: Handle, size: i32) -> i16 {
        let Ok(size) = usize::try_from(size) else {
            return c::PARAM_ERR;
        };
        let Some(record) = self.handles.get_mut(&(handle as usize)) else {
            if !self.is_foreign(handle) {
                return c::NIL_HANDLE_ERR;
            }
            return global::set_size(handle, size);
        };
        if size <= record.block.len() {
            if size > record.size {
                record.block[record.size..size].fill(0);
            }
            record.size = size;
            return c::NO_ERR;
        }
        let Some(mut block) = zeroed_block(size) else {
            return c::MEM_FULL_ERR;
        };
        block[..record.size].copy_from_slice(&record.block[..record.size]);
        let old = std::mem::replace(&mut record.block, block);
        if record.locks > 0 {
            record.retired.push(old);
        }
        record.size = size;
        *record.master = record.block.as_mut_ptr();
        c::NO_ERR
    }

    /// Locks a handle and returns its data pointer, or null for an unknown handle.
    pub fn lock(&mut self, handle: Handle) -> *mut c_void {
        let foreign = self.is_foreign(handle);
        match self.handles.get_mut(&(handle as usize)) {
            Some(record) => {
                record.locks += 1;
                record.block.as_mut_ptr().cast()
            }
            None if foreign => global::lock(handle),
            None => std::ptr::null_mut(),
        }
    }

    pub fn unlock(&mut self, handle: Handle) {
        let foreign = self.is_foreign(handle);
        match self.handles.get_mut(&(handle as usize)) {
            Some(record) => {
                record.locks = record.locks.saturating_sub(1);
                if record.locks == 0 {
                    record.retired.clear();
                }
            }
            None if foreign => global::unlock(handle),
            None => {}
        }
    }

    /// Returns a copy of a handle's contents (empty for an unknown handle).
    pub fn read(&self, handle: Handle) -> Vec<u8> {
        match self.handles.get(&(handle as usize)) {
            Some(record) => record.block[..record.size].to_vec(),
            None if self.is_foreign(handle) => global::read(handle),
            None => Vec::new(),
        }
    }

    /// Replaces a handle's contents, resizing it as needed.
    pub fn write(&mut self, handle: Handle, data: &[u8]) -> i16 {
        let Ok(size) = i32::try_from(data.len()) else {
            return c::MEM_FULL_ERR;
        };
        let error = self.set_size(handle, size);
        if error != c::NO_ERR {
            return error;
        }
        let pointer = self.lock(handle);
        if pointer.is_null() {
            return c::NIL_HANDLE_ERR;
        }
        // SAFETY: `set_size` just made the handle at least `data.len()`
        // bytes long and `lock` returned the start of its block.
        unsafe { std::ptr::copy_nonoverlapping(data.as_ptr(), pointer.cast(), data.len()) };
        self.unlock(handle);
        c::NO_ERR
    }
}

/// Filters that hand the host a Windows `HGLOBAL` instead of one of our handles.
#[cfg(windows)]
mod global {
    use std::ffi::c_void;
    use windows_sys::Win32::Foundation::GlobalFree;
    use windows_sys::Win32::System::Memory::{
        GlobalLock, GlobalReAlloc, GlobalSize, GlobalUnlock, GMEM_MOVEABLE, GMEM_ZEROINIT,
    };

    use crate::host::constants as c;

    fn owns(handle: *mut c_void) -> bool {
        // SAFETY: GlobalSize validates the handle and returns 0 for anything
        // that is not a live global memory object.
        !handle.is_null() && unsafe { GlobalSize(handle) } > 0
    }

    pub fn dispose(handle: *mut c_void) -> bool {
        if !owns(handle) {
            return false;
        }
        // SAFETY: `handle` is a live HGLOBAL (checked above).
        unsafe { GlobalFree(handle) };
        true
    }

    pub fn size(handle: *mut c_void) -> i32 {
        if !owns(handle) {
            return 0;
        }
        // SAFETY: `handle` is a live HGLOBAL.
        i32::try_from(unsafe { GlobalSize(handle) }).unwrap_or(i32::MAX)
    }

    pub fn set_size(handle: *mut c_void, size: usize) -> i16 {
        if !owns(handle) {
            return c::NIL_HANDLE_ERR;
        }
        // SAFETY: `handle` is a live HGLOBAL.
        let resized = unsafe { GlobalReAlloc(handle, size, GMEM_MOVEABLE | GMEM_ZEROINIT) };
        if resized.is_null() {
            c::MEM_FULL_ERR
        } else {
            c::NO_ERR
        }
    }

    pub fn lock(handle: *mut c_void) -> *mut c_void {
        if !owns(handle) {
            return std::ptr::null_mut();
        }
        // SAFETY: `handle` is a live HGLOBAL.
        unsafe { GlobalLock(handle) }
    }

    pub fn unlock(handle: *mut c_void) {
        if owns(handle) {
            // SAFETY: `handle` is a live HGLOBAL.
            unsafe { GlobalUnlock(handle) };
        }
    }

    pub fn read(handle: *mut c_void) -> Vec<u8> {
        let size = usize::try_from(size(handle)).unwrap_or(0);
        let pointer = lock(handle);
        if pointer.is_null() {
            return Vec::new();
        }
        // SAFETY: a locked HGLOBAL points at `GlobalSize` readable bytes.
        let data = unsafe { std::slice::from_raw_parts(pointer.cast::<u8>(), size) }.to_vec();
        unlock(handle);
        data
    }
}

#[cfg(not(windows))]
mod global {
    use std::ffi::c_void;

    use crate::host::constants as c;

    pub fn dispose(_handle: *mut c_void) -> bool {
        false
    }

    pub fn size(_handle: *mut c_void) -> i32 {
        0
    }

    pub fn set_size(_handle: *mut c_void, _size: usize) -> i16 {
        c::NIL_HANDLE_ERR
    }

    pub fn lock(_handle: *mut c_void) -> *mut c_void {
        std::ptr::null_mut()
    }

    pub fn unlock(_handle: *mut c_void) {}

    pub fn read(_handle: *mut c_void) -> Vec<u8> {
        Vec::new()
    }
}

/// Implements the buffer procs and the PICA buffer suite.
pub struct BufferManager {
    buffers: HashMap<usize, (Vec<u8>, usize)>,
    /// The free space reported to filters.
    pub space: i64,
}

impl Default for BufferManager {
    fn default() -> Self {
        Self {
            buffers: HashMap::new(),
            space: DEFAULT_SPACE,
        }
    }
}

impl BufferManager {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn len(&self) -> usize {
        self.buffers.len()
    }

    pub fn is_empty(&self) -> bool {
        self.buffers.is_empty()
    }

    /// Returns a new zeroed buffer, or null when the size is invalid or memory is short.
    pub fn allocate(&mut self, size: i64) -> *mut c_void {
        let Ok(size) = usize::try_from(size) else {
            return std::ptr::null_mut();
        };
        let Some(mut block) = zeroed_block(size) else {
            return std::ptr::null_mut();
        };
        let address = block.as_mut_ptr();
        self.buffers.insert(address as usize, (block, size));
        address.cast()
    }

    pub fn free(&mut self, address: *mut c_void) {
        self.buffers.remove(&(address as usize));
    }

    /// The requested size of a buffer, or 0 for an unknown address.
    pub fn size(&self, address: *mut c_void) -> usize {
        self.buffers
            .get(&(address as usize))
            .map_or(0, |(_, size)| *size)
    }

    pub fn owns(&self, address: *mut c_void) -> bool {
        self.buffers.contains_key(&(address as usize))
    }
}

/// Implements SPBasic `AllocateBlock`, `FreeBlock` and `ReallocateBlock`.
#[derive(Default)]
pub struct BlockAllocator {
    blocks: HashMap<usize, Vec<u8>>,
}

impl BlockAllocator {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn allocate(&mut self, size: usize) -> *mut c_void {
        let Some(mut block) = zeroed_block(size) else {
            return std::ptr::null_mut();
        };
        let address = block.as_mut_ptr();
        self.blocks.insert(address as usize, block);
        address.cast()
    }

    /// Frees a block. Returns false when the block is not one of ours.
    pub fn free(&mut self, address: *mut c_void) -> bool {
        self.blocks.remove(&(address as usize)).is_some()
    }

    /// Moves a block to a new allocation of `size` bytes, keeping its contents.
    ///
    /// A null `address` behaves like `allocate`. Returns null for an unknown
    /// block or when memory is short; the old block is then left untouched.
    pub fn reallocate(&mut self, address: *mut c_void, size: usize) -> *mut c_void {
        if address.is_null() {
            return self.allocate(size);
        }
        let Some(old_len) = self.blocks.get(&(address as usize)).map(Vec::len) else {
            return std::ptr::null_mut();
        };
        let new_address = self.allocate(size);
        if new_address.is_null() {
            return new_address;
        }
        if let Some(old) = self.blocks.remove(&(address as usize)) {
            let count = old_len.min(size.max(1));
            if let Some(new) = self.blocks.get_mut(&(new_address as usize)) {
                new[..count].copy_from_slice(&old[..count]);
            }
        }
        new_address
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn handles_have_real_master_pointers() {
        let mut handles = HandleManager::new();
        let handle = handles.allocate_from(b"abc");
        // SAFETY: the handle points at a live master pointer to 3+ bytes.
        let first = unsafe { **(handle as *mut *mut u8) };
        assert_eq!(first, b'a');
        assert_eq!(handles.size(handle), 3);
    }

    #[test]
    fn a_disposed_handle_is_refused_everywhere() {
        let mut handles = HandleManager::new();
        let handle = handles.allocate_from(b"abc");
        assert!(handles.dispose(handle));
        assert!(!handles.dispose(handle));
        assert_eq!(handles.size(handle), 0);
        assert_eq!(handles.set_size(handle, 8), c::NIL_HANDLE_ERR);
        assert!(handles.lock(handle).is_null());
        assert!(handles.read(handle).is_empty());
    }

    #[test]
    fn growing_a_handle_keeps_data_and_updates_the_master_pointer() {
        let mut handles = HandleManager::new();
        let handle = handles.allocate_from(b"xy");
        assert_eq!(handles.set_size(handle, 4096), c::NO_ERR);
        let data = handles.read(handle);
        assert_eq!(&data[..2], b"xy");
        assert!(data[2..].iter().all(|&b| b == 0));
        // SAFETY: the handle is live and its master pointer was updated.
        let master = unsafe { *(handle as *mut *mut u8) };
        assert_eq!(master.cast::<c_void>(), handles.lock(handle));
    }

    #[test]
    fn growing_a_locked_handle_keeps_the_old_block_until_unlock() {
        let mut handles = HandleManager::new();
        let handle = handles.allocate_from(b"ab");
        let stale = handles.lock(handle).cast::<u8>();
        assert_eq!(handles.set_size(handle, 1 << 20), c::NO_ERR);
        // SAFETY: the old block is retired, not freed, while the lock holds,
        // so a filter writing through its stale pointer stays in bounds.
        unsafe { stale.write(b'z') };
        assert_eq!(&handles.read(handle)[..2], b"ab");
        handles.unlock(handle);
        let record = &handles.handles[&(handle as usize)];
        assert!(record.retired.is_empty());
    }

    #[test]
    fn allocations_over_the_reported_space_fail() {
        assert!(zeroed_block(MAX_ALLOCATION + 1).is_none());
        let mut buffers = BufferManager::new();
        assert!(buffers.allocate(i64::MAX).is_null());
        let mut blocks = BlockAllocator::new();
        assert!(blocks.allocate(usize::MAX / 2).is_null());
    }

    #[test]
    fn shrinking_then_growing_zeroes_the_reused_bytes() {
        let mut handles = HandleManager::new();
        let handle = handles.allocate_from(b"abcd");
        handles.set_size(handle, 1);
        handles.set_size(handle, 4);
        assert_eq!(handles.read(handle), b"a\0\0\0");
    }

    #[test]
    fn invalid_sizes_and_unknown_handles_are_rejected() {
        let mut handles = HandleManager::new();
        assert!(handles.allocate(-1).is_null());
        let bogus = 0x1000 as Handle;
        assert_eq!(handles.size(bogus), 0);
        assert_eq!(handles.set_size(bogus, 4), c::NIL_HANDLE_ERR);
        assert!(!handles.dispose(bogus));
        assert!(handles.dispose(std::ptr::null_mut()));
    }

    #[test]
    fn write_replaces_contents() {
        let mut handles = HandleManager::new();
        let handle = handles.allocate(0);
        assert_eq!(handles.write(handle, b"hello"), c::NO_ERR);
        assert_eq!(handles.read(handle), b"hello");
        assert!(handles.dispose(handle));
        assert!(handles.is_empty());
    }

    #[test]
    fn buffers_track_sizes() {
        let mut buffers = BufferManager::new();
        let buffer = buffers.allocate(100);
        assert!(buffers.owns(buffer));
        assert_eq!(buffers.size(buffer), 100);
        assert!(buffers.allocate(-5).is_null());
        buffers.free(buffer);
        assert!(buffers.is_empty());
    }

    #[test]
    fn reallocate_moves_block_contents() {
        let mut blocks = BlockAllocator::new();
        let block = blocks.allocate(4);
        // SAFETY: the block is 4 bytes long.
        unsafe { std::ptr::copy_nonoverlapping(b"wxyz".as_ptr(), block.cast(), 4) };
        let moved = blocks.reallocate(block, 8);
        // SAFETY: the new block is 8 bytes long.
        let data = unsafe { std::slice::from_raw_parts(moved.cast::<u8>(), 8) };
        assert_eq!(&data[..4], b"wxyz");
        assert!(!blocks.free(block));
        assert!(blocks.free(moved));
        assert!(blocks.reallocate(0x10 as *mut c_void, 4).is_null());
    }
}
