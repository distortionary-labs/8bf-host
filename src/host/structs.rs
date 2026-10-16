//! `#[repr(C)]` mirrors of the Photoshop plug-in API structures.
//!
//! Layout notes:
//!
//! * `FilterRecord` is declared inside `#pragma pack(push, 4)` on Windows
//!   (32- and 64-bit) and with natural alignment on macOS. This crate uses
//!   the Windows layout when built for Windows or with the `pack4` feature,
//!   and natural alignment otherwise. On 64-bit the two differ: 608 bytes
//!   against 648.
//! * Every other structure the host hands to a filter uses natural
//!   alignment on all platforms.
//! * All callbacks use the C calling convention. `MACPASCAL` and `SPAPI`
//!   expand to nothing in current SDKs.

#![allow(non_snake_case)]

use std::ffi::{c_char, c_void};

pub type Boolean = u8;
pub type OSErr = i16;
pub type Fixed = i32;
pub type OSType = u32;
/// An opaque Photoshop handle. Filters may treat it as `void**`.
pub type Handle = *mut c_void;
pub type Ptr = *mut c_void;
pub type SPErr = i32;

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Point {
    pub v: i16,
    pub h: i16,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PsRect {
    pub top: i16,
    pub left: i16,
    pub bottom: i16,
    pub right: i16,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct VPoint {
    pub v: i32,
    pub h: i32,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct VRect {
    pub top: i32,
    pub left: i32,
    pub bottom: i32,
    pub right: i32,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct PSRGBColor {
    pub red: u16,
    pub green: u16,
    pub blue: u16,
}

pub type FilterColor = [u8; 4];

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct PlugInMonitor {
    pub gamma: Fixed,
    pub redX: Fixed,
    pub redY: Fixed,
    pub greenX: Fixed,
    pub greenY: Fixed,
    pub blueX: Fixed,
    pub blueY: Fixed,
    pub whiteX: Fixed,
    pub whiteY: Fixed,
    pub ambient: Fixed,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct PlatformData {
    pub hwnd: isize,
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct PSPixelMask {
    pub next: *mut PSPixelMask,
    pub maskData: *mut c_void,
    pub rowBytes: i32,
    pub colBytes: i32,
    pub maskDescription: i32,
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct PSPixelMap {
    pub version: i32,
    pub bounds: VRect,
    pub imageMode: i32,
    pub rowBytes: i32,
    pub colBytes: i32,
    pub planeBytes: i32,
    pub baseAddr: *mut c_void,
    // Version 1 fields.
    pub mat: *mut PSPixelMask,
    pub masks: *mut PSPixelMask,
    pub maskPhaseRow: i32,
    pub maskPhaseCol: i32,
    // Version 2 fields.
    pub pixelOverlays: *mut c_void,
    pub colorManagementOptions: u32,
    // Version 3 fields.
    pub depth: i16,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub union SelectorParameter {
    pub pickerPrompt: *mut c_void,
    pub globalSamplePoint: *mut Point,
    pub specialColorID: i32,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct ColorServicesInfo {
    pub infoSize: i32,
    pub selector: i16,
    pub sourceSpace: i16,
    pub resultSpace: i16,
    pub resultGamutInfoValid: Boolean,
    pub resultInGamut: Boolean,
    pub reservedSourceSpaceInfo: *mut c_void,
    pub reservedResultSpaceInfo: *mut c_void,
    pub colorComponents: [i16; 4],
    pub reserved: *mut c_void,
    pub selectorParameter: SelectorParameter,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct BigDocumentStruct {
    pub PluginUsing32BitCoordinates: i32,
    pub imageSize32: VPoint,
    pub filterRect32: VRect,
    pub inRect32: VRect,
    pub outRect32: VRect,
    pub maskRect32: VRect,
    pub floatCoord32: VPoint,
    pub wholeSize32: VPoint,
}

// Callback signatures. `Option<fn>` is guaranteed to have the size of a
// nullable C function pointer.
pub type TestAbortProc = Option<unsafe extern "C" fn() -> Boolean>;
pub type ProgressProc = Option<unsafe extern "C" fn(i32, i32)>;
pub type HostProc = Option<unsafe extern "C" fn(i16, *mut isize)>;
pub type ProcessEventProc = Option<unsafe extern "C" fn(*mut c_void)>;
pub type DisplayPixelsProc =
    Option<unsafe extern "C" fn(*const PSPixelMap, *const VRect, i32, i32, *mut c_void) -> OSErr>;
pub type AdvanceStateProc = Option<unsafe extern "C" fn() -> OSErr>;
pub type ColorServicesProc = Option<unsafe extern "C" fn(*mut ColorServicesInfo) -> OSErr>;

pub type NewPIHandleProc = Option<unsafe extern "C" fn(i32) -> Handle>;
pub type DisposePIHandleProc = Option<unsafe extern "C" fn(Handle)>;
pub type GetPIHandleSizeProc = Option<unsafe extern "C" fn(Handle) -> i32>;
pub type SetPIHandleSizeProc = Option<unsafe extern "C" fn(Handle, i32) -> OSErr>;
pub type LockPIHandleProc = Option<unsafe extern "C" fn(Handle, Boolean) -> Ptr>;
pub type UnlockPIHandleProc = Option<unsafe extern "C" fn(Handle)>;
pub type RecoverSpaceProc = Option<unsafe extern "C" fn(i32)>;
pub type SetPIHandleLockProc =
    Option<unsafe extern "C" fn(Handle, Boolean, *mut Ptr, *mut Boolean)>;

pub type AllocateBufferProc = Option<unsafe extern "C" fn(i32, *mut *mut c_void) -> OSErr>;
pub type AllocateBufferProc64 = Option<unsafe extern "C" fn(i64, *mut *mut c_void) -> OSErr>;
pub type LockBufferProc = Option<unsafe extern "C" fn(*mut c_void, Boolean) -> Ptr>;
pub type UnlockBufferProc = Option<unsafe extern "C" fn(*mut c_void)>;
pub type FreeBufferProc = Option<unsafe extern "C" fn(*mut c_void)>;
pub type BufferSpaceProc = Option<unsafe extern "C" fn() -> i32>;
pub type BufferSpaceProc64 = Option<unsafe extern "C" fn() -> i64>;
pub type ReserveSpaceProc = Option<unsafe extern "C" fn(i32) -> OSErr>;

pub type BufferNewProc = Option<unsafe extern "C" fn(*mut u32, u32) -> Ptr>;
pub type BufferNewProc64 = Option<unsafe extern "C" fn(*mut u64, u64) -> Ptr>;
pub type BufferDisposeProc = Option<unsafe extern "C" fn(*mut Ptr)>;
pub type BufferGetSizeProc = Option<unsafe extern "C" fn(Ptr) -> u32>;
pub type BufferGetSizeProc64 = Option<unsafe extern "C" fn(Ptr) -> u64>;
pub type BufferGetSpaceProc = Option<unsafe extern "C" fn() -> u32>;
pub type BufferGetSpaceProc64 = Option<unsafe extern "C" fn() -> u64>;

pub type CountPIResourcesProc = Option<unsafe extern "C" fn(OSType) -> i16>;
pub type GetPIResourceProc = Option<unsafe extern "C" fn(OSType, i16) -> Handle>;
pub type DeletePIResourceProc = Option<unsafe extern "C" fn(OSType, i16)>;
pub type AddPIResourceProc = Option<unsafe extern "C" fn(OSType, Handle) -> OSErr>;

pub type GetPropertyProc =
    Option<unsafe extern "C" fn(OSType, OSType, i32, *mut isize, *mut Handle) -> OSErr>;
pub type SetPropertyProc =
    Option<unsafe extern "C" fn(OSType, OSType, i32, isize, Handle) -> OSErr>;

/// An open write or read descriptor, as the filter sees it.
pub type DescriptorToken = *mut c_void;

// Descriptor suite.
pub type OpenWriteDescriptorProc = Option<unsafe extern "C" fn() -> DescriptorToken>;
pub type CloseWriteDescriptorProc =
    Option<unsafe extern "C" fn(DescriptorToken, *mut Handle) -> OSErr>;
pub type PutIntegerProc = Option<unsafe extern "C" fn(DescriptorToken, u32, i32) -> OSErr>;
pub type PutFloatProc = Option<unsafe extern "C" fn(DescriptorToken, u32, *const f64) -> OSErr>;
pub type PutUnitFloatProc =
    Option<unsafe extern "C" fn(DescriptorToken, u32, u32, *const f64) -> OSErr>;
pub type PutBooleanProc = Option<unsafe extern "C" fn(DescriptorToken, u32, Boolean) -> OSErr>;
pub type PutTextProc = Option<unsafe extern "C" fn(DescriptorToken, u32, Handle) -> OSErr>;
pub type PutAliasProc = Option<unsafe extern "C" fn(DescriptorToken, u32, Handle) -> OSErr>;
pub type PutEnumeratedProc = Option<unsafe extern "C" fn(DescriptorToken, u32, u32, u32) -> OSErr>;
pub type PutClassProc = Option<unsafe extern "C" fn(DescriptorToken, u32, u32) -> OSErr>;
pub type PutSimpleReferenceProc =
    Option<unsafe extern "C" fn(DescriptorToken, u32, *const c_void) -> OSErr>;
pub type PutObjectProc = Option<unsafe extern "C" fn(DescriptorToken, u32, u32, Handle) -> OSErr>;
pub type PutCountProc = Option<unsafe extern "C" fn(DescriptorToken, u32, u32) -> OSErr>;
pub type PutStringProc = Option<unsafe extern "C" fn(DescriptorToken, u32, *const u8) -> OSErr>;
pub type PutScopedClassProc = Option<unsafe extern "C" fn(DescriptorToken, u32, u32) -> OSErr>;
pub type PutScopedObjectProc =
    Option<unsafe extern "C" fn(DescriptorToken, u32, u32, Handle) -> OSErr>;

pub type OpenReadDescriptorProc =
    Option<unsafe extern "C" fn(Handle, *const u32) -> DescriptorToken>;
pub type CloseReadDescriptorProc = Option<unsafe extern "C" fn(DescriptorToken) -> OSErr>;
pub type GetKeyProc =
    Option<unsafe extern "C" fn(DescriptorToken, *mut u32, *mut u32, *mut i32) -> Boolean>;
pub type GetIntegerProc = Option<unsafe extern "C" fn(DescriptorToken, *mut i32) -> OSErr>;
pub type GetFloatProc = Option<unsafe extern "C" fn(DescriptorToken, *mut f64) -> OSErr>;
pub type GetUnitFloatProc =
    Option<unsafe extern "C" fn(DescriptorToken, *mut u32, *mut f64) -> OSErr>;
pub type GetBooleanProc = Option<unsafe extern "C" fn(DescriptorToken, *mut Boolean) -> OSErr>;
pub type GetTextProc = Option<unsafe extern "C" fn(DescriptorToken, *mut Handle) -> OSErr>;
pub type GetAliasProc = Option<unsafe extern "C" fn(DescriptorToken, *mut Handle) -> OSErr>;
pub type GetEnumeratedProc = Option<unsafe extern "C" fn(DescriptorToken, *mut u32) -> OSErr>;
pub type GetClassProc = Option<unsafe extern "C" fn(DescriptorToken, *mut u32) -> OSErr>;
pub type GetSimpleReferenceProc =
    Option<unsafe extern "C" fn(DescriptorToken, *mut c_void) -> OSErr>;
pub type GetObjectProc =
    Option<unsafe extern "C" fn(DescriptorToken, *mut u32, *mut Handle) -> OSErr>;
pub type GetCountProc = Option<unsafe extern "C" fn(DescriptorToken, *mut u32) -> OSErr>;
pub type GetStringProc = Option<unsafe extern "C" fn(DescriptorToken, *mut u8) -> OSErr>;
pub type GetPinnedIntegerProc =
    Option<unsafe extern "C" fn(DescriptorToken, i32, i32, *mut i32) -> OSErr>;
pub type GetPinnedFloatProc =
    Option<unsafe extern "C" fn(DescriptorToken, *const f64, *const f64, *mut f64) -> OSErr>;
pub type GetPinnedUnitFloatProc = Option<
    unsafe extern "C" fn(DescriptorToken, *const f64, *const f64, *mut u32, *mut f64) -> OSErr,
>;

// PICA basic suite.
pub type AcquireSuiteProc =
    Option<unsafe extern "C" fn(*const c_char, i32, *mut *const c_void) -> SPErr>;
pub type ReleaseSuiteProc = Option<unsafe extern "C" fn(*const c_char, i32) -> SPErr>;
pub type IsEqualProc = Option<unsafe extern "C" fn(*const c_char, *const c_char) -> Boolean>;
pub type AllocateBlockProc = Option<unsafe extern "C" fn(usize, *mut *mut c_void) -> SPErr>;
pub type FreeBlockProc = Option<unsafe extern "C" fn(*mut c_void) -> SPErr>;
pub type ReallocateBlockProc =
    Option<unsafe extern "C" fn(*mut c_void, usize, *mut *mut c_void) -> SPErr>;
pub type UndefinedProc = Option<unsafe extern "C" fn() -> SPErr>;

// UI hooks suite.
pub type MainAppWindowProc = Option<unsafe extern "C" fn() -> isize>;
pub type HostSetCursorProc = Option<unsafe extern "C" fn(*const c_char) -> SPErr>;
pub type HostTickCountProc = Option<unsafe extern "C" fn() -> u32>;
pub type PluginNameProc = Option<unsafe extern "C" fn(*mut c_void, *mut *mut c_void) -> SPErr>;
pub type SetPluginMenuEntryProc =
    Option<unsafe extern "C" fn(*mut c_void, *mut c_void, u32) -> SPErr>;
pub type GetLocalizedFileNameProc =
    Option<unsafe extern "C" fn(*mut c_void, *mut *mut c_void) -> SPErr>;

/// The filter entry point:
/// `void PluginMain(int16 selector, void* record, intptr_t* data, int16* result)`.
pub type FilterEntryProc = unsafe extern "C" fn(i16, *mut c_void, *mut isize, *mut i16);

#[repr(C)]
pub struct HandleProcs {
    pub handleProcsVersion: i16,
    pub numHandleProcs: i16,
    pub newProc: NewPIHandleProc,
    pub disposeProc: DisposePIHandleProc,
    pub getSizeProc: GetPIHandleSizeProc,
    pub setSizeProc: SetPIHandleSizeProc,
    pub lockProc: LockPIHandleProc,
    pub unlockProc: UnlockPIHandleProc,
    pub recoverSpaceProc: RecoverSpaceProc,
    pub disposeRegularHandleProc: DisposePIHandleProc,
}

#[repr(C)]
pub struct BufferProcs {
    pub bufferProcsVersion: i16,
    pub numBufferProcs: i16,
    pub allocateProc: AllocateBufferProc,
    pub lockProc: LockBufferProc,
    pub unlockProc: UnlockBufferProc,
    pub freeProc: FreeBufferProc,
    pub spaceProc: BufferSpaceProc,
    pub reserveProc: ReserveSpaceProc,
    pub allocateProc64: AllocateBufferProc64,
    pub spaceProc64: BufferSpaceProc64,
}

#[repr(C)]
pub struct ResourceProcs {
    pub resourceProcsVersion: i16,
    pub numResourceProcs: i16,
    pub countProc: CountPIResourcesProc,
    pub getProc: GetPIResourceProc,
    pub deleteProc: DeletePIResourceProc,
    pub addProc: AddPIResourceProc,
}

#[repr(C)]
pub struct PropertyProcs {
    pub propertyProcsVersion: i16,
    pub numPropertyProcs: i16,
    pub getPropertyProc: GetPropertyProc,
    pub setPropertyProc: SetPropertyProc,
}

#[repr(C)]
pub struct WriteDescriptorProcs {
    pub writeDescriptorProcsVersion: i16,
    pub numWriteDescriptorProcs: i16,
    pub openWriteDescriptorProc: OpenWriteDescriptorProc,
    pub closeWriteDescriptorProc: CloseWriteDescriptorProc,
    pub putIntegerProc: PutIntegerProc,
    pub putFloatProc: PutFloatProc,
    pub putUnitFloatProc: PutUnitFloatProc,
    pub putBooleanProc: PutBooleanProc,
    pub putTextProc: PutTextProc,
    pub putAliasProc: PutAliasProc,
    pub putEnumeratedProc: PutEnumeratedProc,
    pub putClassProc: PutClassProc,
    pub putSimpleReferenceProc: PutSimpleReferenceProc,
    pub putObjectProc: PutObjectProc,
    pub putCountProc: PutCountProc,
    pub putStringProc: PutStringProc,
    pub putScopedClassProc: PutScopedClassProc,
    pub putScopedObjectProc: PutScopedObjectProc,
}

#[repr(C)]
pub struct ReadDescriptorProcs {
    pub readDescriptorProcsVersion: i16,
    pub numReadDescriptorProcs: i16,
    pub openReadDescriptorProc: OpenReadDescriptorProc,
    pub closeReadDescriptorProc: CloseReadDescriptorProc,
    pub getKeyProc: GetKeyProc,
    pub getIntegerProc: GetIntegerProc,
    pub getFloatProc: GetFloatProc,
    pub getUnitFloatProc: GetUnitFloatProc,
    pub getBooleanProc: GetBooleanProc,
    pub getTextProc: GetTextProc,
    pub getAliasProc: GetAliasProc,
    pub getEnumeratedProc: GetEnumeratedProc,
    pub getClassProc: GetClassProc,
    pub getSimpleReferenceProc: GetSimpleReferenceProc,
    pub getObjectProc: GetObjectProc,
    pub getCountProc: GetCountProc,
    pub getStringProc: GetStringProc,
    pub getPinnedIntegerProc: GetPinnedIntegerProc,
    pub getPinnedFloatProc: GetPinnedFloatProc,
    pub getPinnedUnitFloatProc: GetPinnedUnitFloatProc,
}

#[repr(C)]
pub struct PIDescriptorParameters {
    pub descriptorParametersVersion: i16,
    pub playInfo: i16,
    pub recordInfo: i16,
    pub descriptor: Handle,
    pub writeDescriptorProcs: *const WriteDescriptorProcs,
    pub readDescriptorProcs: *const ReadDescriptorProcs,
}

#[repr(C)]
pub struct SPBasicSuite {
    pub AcquireSuite: AcquireSuiteProc,
    pub ReleaseSuite: ReleaseSuiteProc,
    pub IsEqual: IsEqualProc,
    pub AllocateBlock: AllocateBlockProc,
    pub FreeBlock: FreeBlockProc,
    pub ReallocateBlock: ReallocateBlockProc,
    pub Undefined: UndefinedProc,
}

#[repr(C)]
pub struct PSHandleSuite1 {
    pub New: NewPIHandleProc,
    pub Dispose: DisposePIHandleProc,
    pub SetLock: SetPIHandleLockProc,
    pub GetSize: GetPIHandleSizeProc,
    pub SetSize: SetPIHandleSizeProc,
    pub RecoverSpace: RecoverSpaceProc,
}

#[repr(C)]
pub struct PSHandleSuite2 {
    pub New: NewPIHandleProc,
    pub Dispose: DisposePIHandleProc,
    pub DisposeRegularHandle: DisposePIHandleProc,
    pub SetLock: SetPIHandleLockProc,
    pub GetSize: GetPIHandleSizeProc,
    pub SetSize: SetPIHandleSizeProc,
    pub RecoverSpace: RecoverSpaceProc,
}

/// Version 2 of the buffer suite; version 1 is a prefix of it.
#[repr(C)]
pub struct PSBufferSuite2 {
    pub New: BufferNewProc,
    pub Dispose: BufferDisposeProc,
    pub GetSize: BufferGetSizeProc,
    pub GetSpace: BufferGetSpaceProc,
    pub New64: BufferNewProc64,
    pub GetSize64: BufferGetSizeProc64,
    pub GetSpace64: BufferGetSpaceProc64,
}

/// Version 4 of the UI hooks suite; versions 1-3 are prefixes of it.
#[repr(C)]
pub struct PSUIHooksSuite4 {
    pub processEvent: ProcessEventProc,
    pub displayPixels: DisplayPixelsProc,
    pub progressBar: ProgressProc,
    pub testAbort: TestAbortProc,
    pub MainAppWindow: MainAppWindowProc,
    pub SetCursor: HostSetCursorProc,
    pub TickCount: HostTickCountProc,
    pub GetPluginName: PluginNameProc,
    pub SetPluginMenuEntry: SetPluginMenuEntryProc,
    pub GetLocalizedFileName: GetLocalizedFileNameProc,
    pub MainAppRootWindow: MainAppWindowProc,
}

#[repr(C)]
pub struct PSPropertySuite1 {
    pub getPropertyProc: GetPropertyProc,
    pub setPropertyProc: SetPropertyProc,
}

/// The parameter block handed to every filter selector except About.
///
/// Fields of a `packed(4)` struct cannot be borrowed; read and assign them
/// by value.
#[cfg_attr(any(windows, feature = "pack4"), repr(C, packed(4)))]
#[cfg_attr(not(any(windows, feature = "pack4")), repr(C))]
pub struct FilterRecord {
    pub serialNumber: i32,
    pub abortProc: TestAbortProc,
    pub progressProc: ProgressProc,
    pub parameters: Handle,
    pub imageSize: Point,
    pub planes: i16,
    pub filterRect: PsRect,
    pub background: PSRGBColor,
    pub foreground: PSRGBColor,
    pub maxSpace: i32,
    pub bufferSpace: i32,
    pub inRect: PsRect,
    pub inLoPlane: i16,
    pub inHiPlane: i16,
    pub outRect: PsRect,
    pub outLoPlane: i16,
    pub outHiPlane: i16,
    pub inData: *mut c_void,
    pub inRowBytes: i32,
    pub outData: *mut c_void,
    pub outRowBytes: i32,
    pub isFloating: Boolean,
    pub haveMask: Boolean,
    pub autoMask: Boolean,
    pub maskRect: PsRect,
    pub maskData: *mut c_void,
    pub maskRowBytes: i32,
    pub backColor: FilterColor,
    pub foreColor: FilterColor,
    pub hostSig: OSType,
    pub hostProc: HostProc,
    pub imageMode: i16,
    pub imageHRes: Fixed,
    pub imageVRes: Fixed,
    pub floatCoord: Point,
    pub wholeSize: Point,
    pub monitor: PlugInMonitor,
    pub platformData: *mut PlatformData,
    pub bufferProcs: *const BufferProcs,
    pub resourceProcs: *const ResourceProcs,
    pub processEvent: ProcessEventProc,
    pub displayPixels: DisplayPixelsProc,
    pub handleProcs: *const HandleProcs,
    pub supportsDummyChannels: Boolean,
    pub supportsAlternateLayouts: Boolean,
    pub wantLayout: i16,
    pub filterCase: i16,
    pub dummyPlaneValue: i16,
    pub premiereHook: *mut c_void,
    pub advanceState: AdvanceStateProc,
    pub supportsAbsolute: Boolean,
    pub wantsAbsolute: Boolean,
    pub getPropertyObsolete: GetPropertyProc,
    pub cannotUndo: Boolean,
    pub supportsPadding: Boolean,
    pub inputPadding: i16,
    pub outputPadding: i16,
    pub maskPadding: i16,
    pub samplingSupport: i8,
    pub reservedByte: i8,
    pub inputRate: Fixed,
    pub maskRate: Fixed,
    pub colorServices: ColorServicesProc,
    pub inLayerPlanes: i16,
    pub inTransparencyMask: i16,
    pub inLayerMasks: i16,
    pub inInvertedLayerMasks: i16,
    pub inNonLayerPlanes: i16,
    pub outLayerPlanes: i16,
    pub outTransparencyMask: i16,
    pub outLayerMasks: i16,
    pub outInvertedLayerMasks: i16,
    pub outNonLayerPlanes: i16,
    pub absLayerPlanes: i16,
    pub absTransparencyMask: i16,
    pub absLayerMasks: i16,
    pub absInvertedLayerMasks: i16,
    pub absNonLayerPlanes: i16,
    pub inPreDummyPlanes: i16,
    pub inPostDummyPlanes: i16,
    pub outPreDummyPlanes: i16,
    pub outPostDummyPlanes: i16,
    pub inColumnBytes: i32,
    pub inPlaneBytes: i32,
    pub outColumnBytes: i32,
    pub outPlaneBytes: i32,
    pub imageServicesProcs: *mut c_void,
    pub propertyProcs: *const PropertyProcs,
    pub inTileHeight: i16,
    pub inTileWidth: i16,
    pub inTileOrigin: Point,
    pub absTileHeight: i16,
    pub absTileWidth: i16,
    pub absTileOrigin: Point,
    pub outTileHeight: i16,
    pub outTileWidth: i16,
    pub outTileOrigin: Point,
    pub maskTileHeight: i16,
    pub maskTileWidth: i16,
    pub maskTileOrigin: Point,
    pub descriptorParameters: *mut PIDescriptorParameters,
    pub errorString: *mut u8,
    pub channelPortProcs: *mut c_void,
    pub documentInfo: *mut c_void,
    pub sSPBasic: *const SPBasicSuite,
    pub plugInRef: *mut c_void,
    pub depth: i32,
    pub iCCprofileData: Handle,
    pub iCCprofileSize: i32,
    pub canUseICCProfiles: i32,
    pub hasImageScrap: i32,
    pub bigDocumentData: *mut BigDocumentStruct,
    pub input3DScene: *mut c_void,
    pub output3DScene: *mut c_void,
    pub createNewLayer: Boolean,
    pub iCCWorkingProfileData: Handle,
    pub iCCWorkingProfileSize: i32,
    pub bufferSpace64: i64,
    pub maxSpace64: i64,
    pub allowOverrangeColors: Boolean,
    pub reserved: [u8; 45],
}

impl FilterRecord {
    /// Returns a record with every field zeroed (null pointers, no callbacks).
    pub fn zeroed() -> Box<Self> {
        // SAFETY: every field is an integer, a raw pointer, an array of
        // integers or an `Option` of a function pointer; all-zero bytes
        // are a valid value (0, null, `None`) for each of them.
        unsafe { Box::new(std::mem::zeroed()) }
    }
}

/// The record handed to the About selector.
#[repr(C)]
pub struct AboutRecord {
    pub platformData: *mut PlatformData,
    pub sSPBasic: *const SPBasicSuite,
    pub plugInRef: *mut c_void,
    pub reserved: [u8; 244],
}

/// Returns true when this build uses the Windows `pack(4)` record layout.
pub const fn uses_pack4() -> bool {
    cfg!(any(windows, feature = "pack4"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::mem::{offset_of, size_of};

    #[test]
    fn filter_record_size_matches_the_sdk() {
        let expected = match (size_of::<usize>(), uses_pack4()) {
            (8, true) => 608,
            (8, false) => 648,
            // 32-bit layouts are only checked against the C compiler.
            _ => return,
        };
        assert_eq!(size_of::<FilterRecord>(), expected);
    }

    #[test]
    fn natural_structures_have_sdk_sizes() {
        assert_eq!(size_of::<PsRect>(), 8);
        assert_eq!(size_of::<VRect>(), 16);
        assert_eq!(size_of::<PlugInMonitor>(), 40);
        assert_eq!(size_of::<BigDocumentStruct>(), 4 + 8 + 16 * 4 + 8 + 8);
        assert_eq!(
            offset_of!(PIDescriptorParameters, descriptor),
            size_of::<usize>()
        );
    }
}
