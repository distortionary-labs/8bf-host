//! Running a filter: the `FilterRecord`, the host callbacks, the selector loop.
//!
//! [`FilterHost`] holds the state that outlives a run: loaded modules and
//! each filter's globals. A run builds a `SessionCore`, publishes a pointer
//! to it in a thread-local, and calls the parameters, prepare, start,
//! continue and finish selectors. Host callbacks find the session through
//! that thread-local.
//!
//! The filter holds pointers into the session (the record, pixel buffers,
//! suites) while it runs, so everything it can see is heap-allocated with a
//! fixed address, and host code only touches the session in short scopes
//! that never span a call into the filter.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::ffi::{c_char, c_void, CStr};
use std::rc::Rc;
use std::sync::OnceLock;
use std::time::Instant;

use super::colors;
use super::constants as c;
use super::context::{self, guard, with_managers};
use super::descriptors::{Descriptor, READ_PROCS, WRITE_PROCS};
use super::discovery::{host_arch, PluginInfo};
use super::display;
use super::image::{scale_factor, PlaneImage, Rect};
use super::loader::{FilterModule, LoadError, Unloaded};
use super::masking::{apply_mask, mask_bounds};
use super::pipl::{FilterCaseInfo, PiPL};
use super::pixels;
use super::structs::{self as s, FilterRecord, Handle};

/// The most continue calls a filter may need before the host gives up.
pub const MAX_CONTINUE_CALLS: u64 = 10_000_000;

/// The largest coordinate a 16-bit `Rect` can hold.
const MAX_RECT16: i32 = i16::MAX as i32;

/// The edge length of the tiles the host suggests to filters.
const SUGGESTED_TILE: i32 = 1024;

/// Spare bytes after every in, out and mask buffer. Some filters write a
/// byte or two past the end of the rect they asked for (Cybia's EdgeWorks
/// and Fotomatic filters do in grayscale); without room for that the write
/// lands on the next heap block and the worker crashes later, far from the
/// cause. One row of slack covers off-by-one row loops as well.
const PORT_SLACK_MIN: usize = 64;

/// The capacity of the Pascal string a filter can fill in for errReportString.
const ERROR_STRING_BYTES: usize = 256;

/// `(13 << 16)`: the Photoshop version reported through the property suite.
const REPORTED_PHOTOSHOP_VERSION: isize = 13 << 16;

/// How the filter run ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RunStatus {
    Ok,
    Cancelled,
    Error,
}

/// Everything a filter run needs.
///
/// `image` holds `color_channels` color planes followed by one alpha plane
/// when `has_alpha` is set, with 16-bit samples in Photoshop's 0..32768
/// range. `mask` is an optional 8-bit selection the same size as the image
/// (255 = selected).
#[derive(Clone, Debug)]
pub struct FilterRequest {
    pub image: PlaneImage,
    pub color_channels: usize,
    pub has_alpha: bool,
    pub mask: Option<PlaneImage>,
    pub filter_rect: Option<Rect>,
    pub foreground: [u8; 3],
    pub background: [u8; 3],
    pub resolution: f64,
    pub show_ui: bool,
    pub parameters: Option<Vec<u8>>,
    pub descriptor: Option<Descriptor>,
    pub parent_window: isize,
    pub document_title: String,
    pub layer_name: String,
    pub allow_8bit_fallback: bool,
}

impl FilterRequest {
    /// A request for `image` with default settings and no dialog.
    pub fn new(image: PlaneImage, color_channels: usize, has_alpha: bool) -> Self {
        Self {
            image,
            color_channels,
            has_alpha,
            mask: None,
            filter_rect: None,
            foreground: [0, 0, 0],
            background: [255, 255, 255],
            resolution: 72.0,
            show_ui: false,
            parameters: None,
            descriptor: None,
            parent_window: 0,
            document_title: String::new(),
            layer_name: String::new(),
            allow_8bit_fallback: true,
        }
    }

    /// Checks that the request describes an image a filter can be given.
    ///
    /// Requests arrive from the command line, the HTTP server and worker job
    /// files; this is the one place their shape is checked before a filter
    /// sees pointers into them.
    ///
    /// # Errors
    ///
    /// Returns a description of the first problem: a color model other than
    /// gray or RGB, a plane count that disagrees with `color_channels` and
    /// `has_alpha`, dimensions that don't fit Photoshop's 32-bit
    /// coordinates, a mask of the wrong shape, or a resolution that is not a
    /// positive number.
    ///
    /// # Examples
    ///
    /// ```
    /// use eightbf_host::host::image::PlaneImage;
    /// use eightbf_host::host::session::FilterRequest;
    ///
    /// let image = PlaneImage::new(2, 2, 4, 8).unwrap();
    /// assert!(FilterRequest::new(image.clone(), 3, true).validate().is_ok());
    /// assert!(FilterRequest::new(image, 3, false).validate().is_err());
    /// ```
    pub fn validate(&self) -> Result<(), String> {
        let image = &self.image;
        if !matches!(self.color_channels, 1 | 3) {
            return Err(format!(
                "images must have 1 or 3 color channels, not {}",
                self.color_channels
            ));
        }
        let planes = self.color_channels + usize::from(self.has_alpha);
        if image.channels != planes {
            return Err(format!(
                "the image has {} planes but the request describes {planes}",
                image.channels
            ));
        }
        let fits = |size: usize| size > 0 && i32::try_from(size).is_ok();
        if !fits(image.width) || !fits(image.height) {
            return Err(format!(
                "a {}x{} image cannot be filtered",
                image.width, image.height
            ));
        }
        if let Some(mask) = &self.mask {
            let matches_image = mask.width == image.width
                && mask.height == image.height
                && mask.channels == 1
                && mask.depth == super::image::DEPTH_8;
            if !matches_image {
                return Err(format!(
                    "the mask must be an 8-bit, one-channel {}x{} image",
                    image.width, image.height
                ));
            }
        }
        if !(self.resolution.is_finite() && self.resolution > 0.0) {
            return Err(format!(
                "the resolution must be a positive number, not {}",
                self.resolution
            ));
        }
        Ok(())
    }
}

/// Callbacks into the caller while a filter runs.
pub struct Hooks {
    /// Called with `(done, total)` as the filter reports progress.
    pub progress: Box<dyn FnMut(i32, i32)>,
    /// Polled by the filter; returning true asks it to stop.
    pub abort: Box<dyn FnMut() -> bool>,
    /// Receives diagnostic lines.
    pub log: Box<dyn FnMut(&str)>,
}

impl Default for Hooks {
    fn default() -> Self {
        Self {
            progress: Box::new(|_, _| {}),
            abort: Box::new(|| false),
            log: Box::new(|_| {}),
        }
    }
}

/// The outcome of a run.
#[derive(Clone, Debug)]
pub struct FilterResult {
    pub status: RunStatus,
    pub message: String,
    pub image: Option<PlaneImage>,
    pub parameters: Option<Vec<u8>>,
    pub descriptor: Option<Descriptor>,
    pub code: i16,
    pub log: Vec<String>,
}

impl FilterResult {
    pub fn error(message: impl Into<String>, code: i16) -> Self {
        Self {
            status: RunStatus::Error,
            message: message.into(),
            image: None,
            parameters: None,
            descriptor: None,
            code,
            log: Vec::new(),
        }
    }

    pub fn is_ok(&self) -> bool {
        self.status == RunStatus::Ok
    }
}

/// Globals a filter keeps between runs in the same process.
struct PluginState {
    data: isize,
    parameters: Handle,
}

/// Process-wide (per-thread) host state shared by every filter run.
///
/// Not `Send`: filters, their memory and their globals belong to the thread
/// that runs them.
#[derive(Default)]
pub struct FilterHost {
    modules: HashMap<(String, String, String), FilterModule>,
    states: HashMap<String, Box<PluginState>>,
}

impl FilterHost {
    pub fn new() -> Self {
        Self::default()
    }

    fn module_for(&mut self, info: &PluginInfo, arch: &str) -> Result<&FilterModule, LoadError> {
        let entry = info.entry_point(arch).unwrap_or_default().to_string();
        let key = (info.binary.clone(), entry, arch.to_string());
        if !self.modules.contains_key(&key) {
            let module = FilterModule::for_plugin(info, Some(arch))?;
            self.modules.insert(key.clone(), module);
        }
        self.modules
            .get(&key)
            .ok_or_else(|| LoadError("module cache lost an entry".into()))
    }

    fn state_for(&mut self, key: &str) -> *mut PluginState {
        let state = self.states.entry(key.to_string()).or_insert_with(|| {
            Box::new(PluginState {
                data: 0,
                parameters: std::ptr::null_mut(),
            })
        });
        &mut **state
    }

    /// Loads a filter (if needed) and runs it on `request`.
    ///
    /// Load failures and filter errors are reported in the result rather
    /// than as Rust errors, so callers get the log either way.
    pub fn run(
        &mut self,
        info: &PluginInfo,
        request: FilterRequest,
        hooks: Hooks,
        arch: Option<&str>,
    ) -> FilterResult {
        if let Err(problem) = request.validate() {
            return FilterResult::error(format!("Invalid request: {problem}."), c::PARAM_ERR);
        }
        let arch = arch.unwrap_or(host_arch()).to_string();
        let state = self.state_for(&info.key());
        let module = match self.module_for(info, &arch) {
            Ok(module) => module,
            Err(error) => return FilterResult::error(error.0, c::PARAM_ERR),
        };
        let sink = LogSink::install(hooks.log);
        let result = run_session(
            module,
            &info.pipl,
            state,
            request,
            hooks.progress,
            hooks.abort,
        );
        sink.finish(result)
    }

    /// Unloads the filter's code, as Photoshop does after every run. Its
    /// globals and parameters stay, so the next run loads the code again
    /// and carries on with them. Says what became of each module.
    pub fn unload(&mut self, info: &PluginInfo) -> Vec<Unloaded> {
        let keys: Vec<_> = self
            .modules
            .keys()
            .filter(|(binary, _, _)| *binary == info.binary)
            .cloned()
            .collect();
        keys.iter()
            .filter_map(|key| self.modules.remove(key))
            .map(FilterModule::unload)
            .collect()
    }

    /// Shows the filter's About box.
    pub fn about(
        &mut self,
        info: &PluginInfo,
        parent_window: isize,
        log: Box<dyn FnMut(&str)>,
        arch: Option<&str>,
    ) -> FilterResult {
        let arch = arch.unwrap_or(host_arch()).to_string();
        let state = self.state_for(&info.key());
        let module = match self.module_for(info, &arch) {
            Ok(module) => module,
            Err(error) => return FilterResult::error(error.0, c::PARAM_ERR),
        };
        let sink = LogSink::install(log);
        let result = run_about(module, &info.pipl, state, parent_window);
        sink.finish(result)
    }
}

/// Collects log lines for the result while forwarding them to the caller.
struct LogSink {
    lines: Rc<RefCell<Vec<String>>>,
}

impl LogSink {
    fn install(mut forward: Box<dyn FnMut(&str)>) -> Self {
        let lines = Rc::new(RefCell::new(Vec::new()));
        let collected = Rc::clone(&lines);
        context::set_log_sink(Some(Box::new(move |line: &str| {
            if let Ok(mut lines) = collected.try_borrow_mut() {
                lines.push(line.to_string());
            }
            forward(line);
        })));
        Self { lines }
    }

    fn finish(self, mut result: FilterResult) -> FilterResult {
        context::set_log_sink(None);
        result.log = self.lines.take();
        result
    }
}

fn fixed(value: f64) -> i32 {
    (value * f64::from(c::FIXED_ONE)).round() as i32
}

fn clamp16(value: i32) -> i16 {
    value.clamp(i32::from(i16::MIN), MAX_RECT16) as i16
}

fn to_ps_rect(rect: Rect) -> s::PsRect {
    s::PsRect {
        top: clamp16(rect.top),
        left: clamp16(rect.left),
        bottom: clamp16(rect.bottom),
        right: clamp16(rect.right),
    }
}

fn to_vrect(rect: Rect) -> s::VRect {
    s::VRect {
        top: rect.top,
        left: rect.left,
        bottom: rect.bottom,
        right: rect.right,
    }
}

fn mode_name(mode: i16) -> String {
    match mode {
        c::MODE_RGB => "8-bit RGB".into(),
        c::MODE_RGB48 => "16-bit RGB".into(),
        c::MODE_GRAYSCALE => "8-bit grayscale".into(),
        c::MODE_GRAY16 => "16-bit grayscale".into(),
        other => format!("mode {other}"),
    }
}

fn filter_name(pipl: &PiPL) -> String {
    if pipl.name.is_empty() {
        "The filter".into()
    } else {
        pipl.name.clone()
    }
}

/// Why a run stopped early.
enum Stop {
    Cancelled,
    Failed(String, i16),
}

/// A heap allocation the filter may hold a pointer to.
///
/// Host code reaches the value only through the raw pointer, so filter
/// writes through its own copy of the pointer never conflict with a live
/// Rust reference.
struct Owned<T>(*mut T);

impl<T> Owned<T> {
    fn new(value: T) -> Self {
        Self(Box::into_raw(Box::new(value)))
    }

    fn from_box(value: Box<T>) -> Self {
        Self(Box::into_raw(value))
    }

    fn ptr(&self) -> *mut T {
        self.0
    }
}

impl<T> Drop for Owned<T> {
    fn drop(&mut self) {
        // SAFETY: the pointer came from Box::into_raw and is freed once.
        drop(unsafe { Box::from_raw(self.0) });
    }
}

/// The PICA suites a filter acquired, kept alive for the session.
enum Suite {
    Handle1(Owned<s::PSHandleSuite1>),
    Handle2(Owned<s::PSHandleSuite2>),
    Buffer(Owned<s::PSBufferSuite2>),
    UiHooks(Owned<s::PSUIHooksSuite4>),
    Property(Owned<s::PSPropertySuite1>),
}

impl Suite {
    fn address(&self) -> *const c_void {
        match self {
            Suite::Handle1(suite) => suite.ptr().cast_const().cast(),
            Suite::Handle2(suite) => suite.ptr().cast_const().cast(),
            Suite::Buffer(suite) => suite.ptr().cast_const().cast(),
            Suite::UiHooks(suite) => suite.ptr().cast_const().cast(),
            Suite::Property(suite) => suite.ptr().cast_const().cast(),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Port {
    In,
    Out,
    Mask,
}

/// The rect, planes, rate and padding a pixel buffer was filled for.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct FillState {
    rect: Rect,
    lo: usize,
    hi: usize,
    rate: i32,
    padding: i16,
}

/// Everything callbacks need during one run.
struct SessionCore {
    request: FilterRequest,
    pipl: PiPL,
    progress: Box<dyn FnMut(i32, i32)>,
    abort: Box<dyn FnMut() -> bool>,
    record: Owned<FilterRecord>,
    big: Owned<s::BigDocumentStruct>,
    platform: Owned<s::PlatformData>,
    error_string: Owned<[u8; ERROR_STRING_BYTES]>,
    handle_procs: Owned<s::HandleProcs>,
    buffer_procs: Owned<s::BufferProcs>,
    resource_procs: Owned<s::ResourceProcs>,
    property_procs: Owned<s::PropertyProcs>,
    descriptor_parameters: Owned<s::PIDescriptorParameters>,
    basic_suite: Owned<s::SPBasicSuite>,
    suites: HashMap<(Vec<u8>, i32), Suite>,
    resources: Vec<(u32, Vec<u8>)>,
    buffers: HashMap<u8, Vec<u8>>,
    last: HashMap<u8, FillState>,
    scaled: HashMap<(u8, u32), PlaneImage>,
    case: i16,
    case_info: Option<FilterCaseInfo>,
    image_mode: i16,
    depth: u32,
    downgraded: bool,
    writes_outside_selection: bool,
    filter_rect: Rect,
    work_in: PlaneImage,
    work_out: PlaneImage,
}

thread_local! {
    static ACTIVE: Cell<*mut SessionCore> = const { Cell::new(std::ptr::null_mut()) };
    static BUSY: Cell<bool> = const { Cell::new(false) };
}

/// Runs `f` with the active session, or returns `default` when there is
/// none or the session is already in use further up the stack.
fn with_core<R>(default: R, f: impl FnOnce(&mut SessionCore) -> R) -> R {
    let core = ACTIVE.with(Cell::get);
    if core.is_null() || BUSY.with(Cell::get) {
        return default;
    }
    /// Clears BUSY on the way out, including when `f` panics.
    struct Release;
    impl Drop for Release {
        fn drop(&mut self) {
            BUSY.with(|busy| busy.set(false));
        }
    }
    BUSY.with(|busy| busy.set(true));
    let _release = Release;
    // SAFETY: ACTIVE points at the session owned by the running
    // `run_session` frame, and BUSY guarantees this is the only reference.
    f(unsafe { &mut *core })
}

/// Publishes a session in ACTIVE and clears it again on drop.
struct Activation;

impl Activation {
    fn new(core: *mut SessionCore) -> Self {
        ACTIVE.with(|active| active.set(core));
        Self
    }
}

impl Drop for Activation {
    fn drop(&mut self) {
        ACTIVE.with(|active| active.set(std::ptr::null_mut()));
        BUSY.with(|busy| busy.set(false));
    }
}

impl SessionCore {
    fn new(
        request: FilterRequest,
        pipl: &PiPL,
        progress: Box<dyn FnMut(i32, i32)>,
        abort: Box<dyn FnMut() -> bool>,
    ) -> Self {
        let platform = s::PlatformData {
            hwnd: request.parent_window,
        };
        let empty = request.image.clone();
        Self {
            pipl: pipl.clone(),
            progress,
            abort,
            record: Owned::from_box(FilterRecord::zeroed()),
            big: Owned::new(s::BigDocumentStruct::default()),
            platform: Owned::new(platform),
            error_string: Owned::new([0; ERROR_STRING_BYTES]),
            handle_procs: Owned::new(handle_procs()),
            buffer_procs: Owned::new(buffer_procs()),
            resource_procs: Owned::new(resource_procs()),
            property_procs: Owned::new(property_procs()),
            descriptor_parameters: Owned::new(s::PIDescriptorParameters {
                descriptorParametersVersion: 0,
                playInfo: 0,
                recordInfo: 0,
                descriptor: std::ptr::null_mut(),
                writeDescriptorProcs: &WRITE_PROCS,
                readDescriptorProcs: &READ_PROCS,
            }),
            basic_suite: Owned::new(basic_suite()),
            suites: HashMap::new(),
            resources: Vec::new(),
            buffers: HashMap::new(),
            last: HashMap::new(),
            scaled: HashMap::new(),
            case: c::FILTER_CASE_UNSUPPORTED,
            case_info: None,
            image_mode: c::MODE_RGB,
            depth: 8,
            downgraded: false,
            writes_outside_selection: false,
            filter_rect: Rect::default(),
            work_out: empty.clone(),
            work_in: empty,
            request,
        }
    }

    fn record(&mut self) -> &mut FilterRecord {
        // SAFETY: the record lives as long as the session, and filter code
        // is not running while host code holds this reference.
        unsafe { &mut *self.record.ptr() }
    }

    fn big(&mut self) -> &mut s::BigDocumentStruct {
        // SAFETY: as for `record`.
        unsafe { &mut *self.big.ptr() }
    }

    fn rect(&mut self, port: Port) -> Rect {
        let use32 = self.big().PluginUsing32BitCoordinates != 0;
        if use32 {
            let big = *self.big();
            let rect = match port {
                Port::In => big.inRect32,
                Port::Out => big.outRect32,
                Port::Mask => big.maskRect32,
            };
            return Rect::new(rect.top, rect.left, rect.bottom, rect.right);
        }
        let record = self.record();
        let rect = match port {
            Port::In => record.inRect,
            Port::Out => record.outRect,
            Port::Mask => record.maskRect,
        };
        Rect::new(
            rect.top.into(),
            rect.left.into(),
            rect.bottom.into(),
            rect.right.into(),
        )
    }

    // Filter case and work image --------------------------------------------

    fn opaque_alpha(&self) -> Vec<u8> {
        if self.request.image.depth == 8 {
            vec![u8::MAX]
        } else {
            c::PS_16BIT_MAX.to_ne_bytes().to_vec()
        }
    }

    fn has_transparency(&self) -> bool {
        if !self.request.has_alpha {
            return false;
        }
        let alpha = self.request.image.plane(self.request.color_channels);
        let opaque = self.opaque_alpha();
        alpha
            .chunks_exact(opaque.len())
            .any(|sample| sample != opaque.as_slice())
    }

    fn choose_case(&self) -> Result<(i16, Option<FilterCaseInfo>), Stop> {
        let offset = i16::from(self.request.mask.is_some());
        let mut candidates = Vec::new();
        if self.has_transparency() {
            candidates.push(c::FILTER_CASE_EDITABLE_TRANSPARENCY_NO_SELECTION + offset);
            candidates.push(c::FILTER_CASE_PROTECTED_TRANSPARENCY_NO_SELECTION + offset);
        }
        candidates.push(c::FILTER_CASE_FLAT_NO_SELECTION + offset);
        for case in candidates {
            match self.pipl.case_info(case) {
                None => return Ok((case, None)),
                Some(info) if info.can_filter() => return Ok((case, Some(info.clone()))),
                Some(_) => {}
            }
        }
        Err(Stop::Failed(
            format!(
                "{} cannot filter this kind of image.",
                filter_name(&self.pipl)
            ),
            c::FILTER_BAD_MODE,
        ))
    }

    fn mode_for(&self, depth: u32) -> i16 {
        let gray = self.request.color_channels == 1;
        match (depth, gray) {
            (16, true) => c::MODE_GRAY16,
            (16, false) => c::MODE_RGB48,
            (_, true) => c::MODE_GRAYSCALE,
            (_, false) => c::MODE_RGB,
        }
    }

    fn is_flat(&self) -> bool {
        matches!(
            self.case,
            c::FILTER_CASE_FLAT_NO_SELECTION | c::FILTER_CASE_FLAT_WITH_SELECTION
        )
    }

    /// Builds the image the filter sees (planes, depth) for the chosen case.
    fn prepare_work_image(&mut self) -> Result<(), Stop> {
        let (case, info) = self.choose_case()?;
        self.case = case;
        self.case_info = info;
        let color = self.request.color_channels;
        let planes = if self.is_flat() { color } else { color + 1 };
        let source_depth = self.request.image.depth;
        let mut depth = source_depth;
        let mut mode = self.mode_for(depth);
        if !self.pipl.supports_mode(mode) {
            let fallback = self.mode_for(8);
            if depth == 16 && self.request.allow_8bit_fallback && self.pipl.supports_mode(fallback)
            {
                context::log("filter lacks 16-bit support; running it on an 8-bit copy");
                self.downgraded = true;
                depth = 8;
                mode = fallback;
            } else {
                return Err(Stop::Failed(
                    format!(
                        "{} does not support {} images.",
                        filter_name(&self.pipl),
                        mode_name(mode)
                    ),
                    c::FILTER_BAD_MODE,
                ));
            }
        }
        let source = &self.request.image;
        let work = if planes == source.channels && depth == source.depth {
            source.clone()
        } else {
            let mut work = PlaneImage::new(source.width, source.height, planes, depth)
                .map_err(|error| Stop::Failed(error.to_string(), c::MEM_FULL_ERR))?;
            for plane in 0..planes {
                let values = source.plane(plane);
                if depth == source.depth {
                    work.set_plane(plane, &values);
                } else {
                    work.set_plane(plane, &pixels::ps16_to_ps8(&values));
                }
            }
            work
        };
        self.image_mode = mode;
        self.depth = depth;
        self.work_out = work.clone();
        self.work_in = work;
        Ok(())
    }

    fn compute_filter_rect(&self) -> Result<Rect, Stop> {
        let image = &self.request.image;
        let width = i32::try_from(image.width).unwrap_or(i32::MAX);
        let height = i32::try_from(image.height).unwrap_or(i32::MAX);
        let rect = self
            .request
            .filter_rect
            .or_else(|| self.request.mask.as_ref().map(mask_bounds))
            .unwrap_or(Rect::new(0, 0, height, width));
        let rect = Rect::new(
            rect.top.max(0),
            rect.left.max(0),
            rect.bottom.min(height),
            rect.right.min(width),
        );
        if rect.is_empty() {
            return Err(Stop::Failed("The selection is empty.".into(), c::PARAM_ERR));
        }
        Ok(rect)
    }

    // FilterRecord setup -------------------------------------------------------

    fn setup_record(&mut self) -> Result<(), Stop> {
        let rect = self.compute_filter_rect()?;
        self.filter_rect = rect;
        let width = i32::try_from(self.work_in.width).unwrap_or(i32::MAX);
        let height = i32::try_from(self.work_in.height).unwrap_or(i32::MAX);
        let planes = i16::try_from(self.work_in.channels).unwrap_or(i16::MAX);
        let request = self.request.clone_settings();
        let space =
            with_managers(|m| m.buffers.space).unwrap_or(crate::host::memory::DEFAULT_SPACE);
        let space32 = i32::try_from(space).unwrap_or(i32::MAX);
        let (fore_gray, back_gray) = if request.color_channels == 1 {
            (gray_of(request.foreground), gray_of(request.background))
        } else {
            (0, 0)
        };
        let image_mode = self.image_mode;
        let case = self.case;
        let depth = self.depth;
        let platform = self.platform.ptr();
        let buffer_procs = self.buffer_procs.ptr().cast_const();
        let resource_procs = self.resource_procs.ptr().cast_const();
        let handle_procs = self.handle_procs.ptr().cast_const();
        let property_procs = self.property_procs.ptr().cast_const();
        let descriptor_parameters = self.descriptor_parameters.ptr();
        let error_string = self.error_string.ptr().cast::<u8>();
        let basic_suite = self.basic_suite.ptr().cast_const();
        let big = self.big.ptr();
        let has_mask = request.has_mask;
        let writes_outside = self
            .case_info
            .as_ref()
            .is_some_and(|info| info.flags1 & c::CASE_FLAG_WRITES_OUTSIDE_SELECTION != 0);
        self.writes_outside_selection = writes_outside;
        let tile = clamp16(SUGGESTED_TILE.min(width.max(height)));

        let record = self.record();
        record.serialNumber = 0;
        record.abortProc = Some(cb_abort);
        record.progressProc = Some(cb_progress);
        record.parameters = std::ptr::null_mut();
        record.imageSize = s::Point {
            v: clamp16(height),
            h: clamp16(width),
        };
        record.planes = planes;
        record.filterRect = to_ps_rect(rect);
        record.foreground = to_ps_color(request.foreground);
        record.background = to_ps_color(request.background);
        if request.color_channels == 1 {
            record.foreColor = [fore_gray, 0, 0, 0];
            record.backColor = [back_gray, 0, 0, 0];
        } else {
            let [r, g, b] = request.foreground;
            record.foreColor = [r, g, b, 0];
            let [r, g, b] = request.background;
            record.backColor = [r, g, b, 0];
        }
        record.maxSpace = space32;
        record.bufferSpace = space32;
        record.maxSpace64 = space;
        record.bufferSpace64 = space;
        record.hostSig = c::PHOTOSHOP_SIGNATURE;
        record.hostProc = Some(cb_host_proc);
        record.imageMode = image_mode;
        record.imageHRes = fixed(request.resolution);
        record.imageVRes = fixed(request.resolution);
        record.wholeSize = record.imageSize;
        record.monitor = s::PlugInMonitor {
            gamma: fixed(2.2),
            redX: fixed(0.64),
            redY: fixed(0.33),
            greenX: fixed(0.30),
            greenY: fixed(0.60),
            blueX: fixed(0.15),
            blueY: fixed(0.06),
            whiteX: fixed(0.3127),
            whiteY: fixed(0.3290),
            ambient: fixed(1.0),
        };
        record.platformData = platform;
        record.bufferProcs = buffer_procs;
        record.resourceProcs = resource_procs;
        record.processEvent = Some(cb_process_event);
        record.displayPixels = Some(cb_display_pixels);
        record.handleProcs = handle_procs;
        record.supportsDummyChannels = 0;
        record.supportsAlternateLayouts = 0;
        record.wantLayout = 0;
        record.filterCase = case;
        record.dummyPlaneValue = -1;
        record.advanceState = Some(cb_advance_state);
        record.supportsAbsolute = 0;
        record.wantsAbsolute = 0;
        record.getPropertyObsolete = Some(cb_get_property);
        record.cannotUndo = 0;
        record.supportsPadding = 1;
        record.inputPadding = c::PADDING_ERROR_ON_BOUNDS;
        record.outputPadding = c::PADDING_ERROR_ON_BOUNDS;
        record.maskPadding = c::PADDING_ERROR_ON_BOUNDS;
        record.samplingSupport = c::SAMPLING_INTEGRAL;
        record.inputRate = c::FIXED_ONE;
        record.maskRate = c::FIXED_ONE;
        record.colorServices = Some(cb_color_services);
        record.propertyProcs = property_procs;
        record.inTileHeight = tile;
        record.inTileWidth = tile;
        record.absTileHeight = tile;
        record.absTileWidth = tile;
        record.outTileHeight = tile;
        record.outTileWidth = tile;
        record.maskTileHeight = tile;
        record.maskTileWidth = tile;
        record.descriptorParameters = descriptor_parameters;
        record.errorString = error_string;
        record.sSPBasic = basic_suite;
        record.depth = i32::try_from(depth).unwrap_or(8);
        record.canUseICCProfiles = 0;
        record.bigDocumentData = big;
        record.isFloating = 0;
        record.haveMask = u8::from(has_mask);
        record.autoMask = u8::from(has_mask && !writes_outside);

        let big = self.big();
        big.imageSize32 = s::VPoint {
            v: height,
            h: width,
        };
        big.filterRect32 = to_vrect(rect);
        big.wholeSize32 = big.imageSize32;

        self.set_plane_layout();
        self.setup_descriptor();
        Ok(())
    }

    fn set_plane_layout(&mut self) {
        let color = i16::try_from(self.request.color_channels).unwrap_or(i16::MAX);
        let bps = i32::try_from(self.depth / 8).unwrap_or(1);
        let planes = i32::try_from(self.work_in.channels).unwrap_or(1);
        let case = self.case;
        let flat = self.is_flat();
        let protected = matches!(
            case,
            c::FILTER_CASE_PROTECTED_TRANSPARENCY_NO_SELECTION
                | c::FILTER_CASE_PROTECTED_TRANSPARENCY_WITH_SELECTION
        );
        let record = self.record();
        if flat {
            record.inLayerPlanes = 0;
            record.inTransparencyMask = 0;
            record.inNonLayerPlanes = color;
        } else {
            record.inLayerPlanes = color;
            record.inTransparencyMask = 1;
            record.inNonLayerPlanes = 0;
        }
        if protected {
            record.outLayerPlanes = color;
            record.outTransparencyMask = 0;
            record.outNonLayerPlanes = 0;
        } else {
            record.outLayerPlanes = record.inLayerPlanes;
            record.outTransparencyMask = record.inTransparencyMask;
            record.outNonLayerPlanes = record.inNonLayerPlanes;
        }
        record.absLayerPlanes = record.inLayerPlanes;
        record.absTransparencyMask = record.inTransparencyMask;
        record.absNonLayerPlanes = record.inNonLayerPlanes;
        record.inColumnBytes = planes * bps;
        record.outColumnBytes = planes * bps;
        record.inPlaneBytes = bps;
        record.outPlaneBytes = bps;
    }

    fn setup_descriptor(&mut self) {
        let show_ui = self.request.show_ui;
        let descriptor = self.request.descriptor.clone();
        let handle = descriptor
            .and_then(|descriptor| {
                with_managers(|m| m.descriptors.handle_for(&mut m.handles, &descriptor))
            })
            .unwrap_or(std::ptr::null_mut());
        // SAFETY: the parameters block lives as long as the session.
        let params = unsafe { &mut *self.descriptor_parameters.ptr() };
        params.descriptorParametersVersion = 0;
        params.recordInfo = c::RECORD_DIALOG_OPTIONAL;
        params.playInfo = if show_ui {
            c::PLAY_DIALOG_DISPLAY
        } else {
            c::PLAY_DIALOG_DONT_DISPLAY
        };
        params.descriptor = handle;
    }

    // Pixel exchange ------------------------------------------------------------

    fn advance_state(&mut self) -> i16 {
        self.flush_output();
        let (have_mask, mask_padding, mask_rate, input_padding, input_rate, in_lo, in_hi) = {
            let record = self.record();
            (
                record.haveMask != 0,
                record.maskPadding,
                record.maskRate,
                record.inputPadding,
                record.inputRate,
                record.inLoPlane,
                record.inHiPlane,
            )
        };
        let (output_padding, out_lo, out_hi) = {
            let record = self.record();
            (record.outputPadding, record.outLoPlane, record.outHiPlane)
        };
        let error = self.fill(Port::Mask, have_mask, mask_padding, mask_rate, 0, 0);
        if error != c::NO_ERR {
            return error;
        }
        let error = self.fill(Port::In, true, input_padding, input_rate, in_lo, in_hi);
        if error != c::NO_ERR {
            return error;
        }
        self.fill(
            Port::Out,
            true,
            output_padding,
            c::FIXED_ONE,
            out_lo,
            out_hi,
        )
    }

    /// Publishes a port's buffer to the filter. `column_bytes` is the
    /// distance between pixels in that buffer, which holds only the planes
    /// the filter asked for, not every plane of the image.
    fn set_port_pointer(
        &mut self,
        port: Port,
        data: *mut c_void,
        row_bytes: i32,
        column_bytes: i32,
    ) {
        let record = self.record();
        match port {
            Port::In => {
                record.inData = data;
                record.inRowBytes = row_bytes;
                record.inColumnBytes = column_bytes;
            }
            Port::Out => {
                record.outData = data;
                record.outRowBytes = row_bytes;
                record.outColumnBytes = column_bytes;
            }
            Port::Mask => {
                record.maskData = data;
                record.maskRowBytes = row_bytes;
            }
        }
    }

    fn fill(
        &mut self,
        port: Port,
        enabled: bool,
        padding: i16,
        rate: i32,
        lo: i16,
        hi: i16,
    ) -> i16 {
        let key = port as u8;
        let rect = self.rect(port);
        let has_image = enabled && (port != Port::Mask || self.request.mask.is_some());
        if !has_image || rect.is_empty() {
            self.buffers.remove(&key);
            self.last.remove(&key);
            self.set_port_pointer(port, std::ptr::null_mut(), 0, 0);
            return c::NO_ERR;
        }
        let (lo, hi) = if port == Port::Mask { (0, 0) } else { (lo, hi) };
        let (Ok(lo), Ok(hi)) = (usize::try_from(lo), usize::try_from(hi)) else {
            return c::PARAM_ERR;
        };
        if hi < lo {
            return c::PARAM_ERR;
        }
        let state = FillState {
            rect,
            lo,
            hi,
            rate,
            padding,
        };
        let factor = if port == Port::Out {
            1
        } else {
            scale_factor(rate)
        };
        let bps = match port {
            Port::Mask => 1,
            _ => self.work_in.bytes_per_sample(),
        };
        let width = usize::try_from(rect.width()).unwrap_or(0);
        let height = usize::try_from(rect.height()).unwrap_or(0);
        let Some(row_bytes) = width
            .checked_mul(hi - lo + 1)
            .and_then(|n| n.checked_mul(bps))
        else {
            return c::MEM_FULL_ERR;
        };
        let Ok(row_bytes32) = i32::try_from(row_bytes) else {
            return c::MEM_FULL_ERR;
        };
        // No larger than `row_bytes` (the rect is not empty), so it fits.
        let column_bytes = i32::try_from((hi - lo + 1) * bps).unwrap_or(i32::MAX);
        if self.last.get(&key) == Some(&state) {
            if let Some(buffer) = self.buffers.get_mut(&key) {
                // Publish the pointers again: the filter may have moved or
                // cleared its copies since the last call.
                let pointer = buffer.as_mut_ptr().cast::<c_void>();
                self.set_port_pointer(port, pointer, row_bytes32, column_bytes);
                return c::NO_ERR;
            }
        }
        // The rect and plane range come from the filter and are checked
        // against the image only after the buffer exists, so bound the
        // buffer by the space the host reports first.
        let Some(total) = row_bytes
            .checked_mul(height)
            .filter(|&total| total <= crate::host::memory::MAX_ALLOCATION)
        else {
            return c::MEM_FULL_ERR;
        };
        // The filter is told about `total` bytes; the slack after them only
        // absorbs stray writes, and flush_output never reads it.
        let Some(allocated) = row_bytes.max(PORT_SLACK_MIN).checked_add(total) else {
            return c::MEM_FULL_ERR;
        };
        let mut buffer = Vec::new();
        if buffer.try_reserve_exact(allocated).is_err() {
            return c::MEM_FULL_ERR;
        }
        buffer.resize(allocated, 0);
        if factor > 1 && !self.scaled.contains_key(&(key, factor)) {
            let source = match port {
                Port::Mask => self.request.mask.as_ref(),
                _ => Some(&self.work_in),
            };
            if let Some(source) = source {
                let scaled = source.scaled(factor);
                self.scaled.insert((key, factor), scaled);
            }
        }
        let source = if factor > 1 {
            self.scaled.get(&(key, factor))
        } else {
            match port {
                Port::In => Some(&self.work_in),
                Port::Out => Some(&self.work_out),
                Port::Mask => self.request.mask.as_ref(),
            }
        };
        let Some(source) = source else {
            return c::PARAM_ERR;
        };
        let error = source.read_rect(rect, lo, hi, &mut buffer, row_bytes, padding, 0);
        if error != c::NO_ERR {
            context::log(&format!(
                "{port:?} request {rect:?} (planes {lo}-{hi}) rejected: out of bounds"
            ));
            return error;
        }
        let pointer = buffer.as_mut_ptr().cast::<c_void>();
        self.buffers.insert(key, buffer);
        self.last.insert(key, state);
        self.set_port_pointer(port, pointer, row_bytes32, column_bytes);
        c::NO_ERR
    }

    fn flush_output(&mut self) {
        let key = Port::Out as u8;
        let (Some(buffer), Some(last)) = (self.buffers.get(&key), self.last.get(&key)) else {
            return;
        };
        let width = usize::try_from(last.rect.width()).unwrap_or(0);
        let row_bytes = width * (last.hi - last.lo + 1) * self.work_out.bytes_per_sample();
        self.work_out
            .write_rect(last.rect, last.lo, last.hi, buffer, row_bytes);
        // Cached scaled copies are of work_in, which the filter never writes.
    }

    // Results ---------------------------------------------------------------------

    fn finish_image(&mut self) -> PlaneImage {
        let request = &self.request;
        let source = &request.image;
        let mut result = source.clone();
        let mut writable = request.color_channels;
        if matches!(
            self.case,
            c::FILTER_CASE_EDITABLE_TRANSPARENCY_NO_SELECTION
                | c::FILTER_CASE_EDITABLE_TRANSPARENCY_WITH_SELECTION
        ) {
            writable += 1;
        }
        for plane in 0..writable {
            let values = self.work_out.plane(plane);
            if self.downgraded {
                if values == self.work_in.plane(plane) {
                    // Untouched by the filter: keep the full 16-bit data.
                    continue;
                }
                result.set_plane(plane, &pixels::ps8_to_ps16(&values));
            } else {
                result.set_plane(plane, &values);
            }
        }
        let fills_mask = self
            .case_info
            .as_ref()
            .is_some_and(|info| info.output_handling == c::HANDLING_FILL_MASK);
        if fills_mask && request.has_alpha && writable > request.color_channels {
            let opaque = self.opaque_alpha();
            let count = source.width * source.height;
            result.set_plane(request.color_channels, &opaque.repeat(count));
        }
        let auto_mask = self.record().autoMask != 0;
        let request = &self.request;
        if let Some(mask) = &request.mask {
            if auto_mask && !self.writes_outside_selection {
                apply_mask(&request.image, &mut result, mask, Some(self.filter_rect));
            }
        }
        result
    }

    fn error_text(&mut self) -> String {
        // SAFETY: the error string buffer lives as long as the session.
        let buffer = unsafe { &*self.error_string.ptr() };
        let length = usize::from(buffer[0]).min(ERROR_STRING_BYTES - 1);
        decode_text(&buffer[1..=length])
    }

    // Callbacks -------------------------------------------------------------------

    fn channel_names(&mut self) -> Vec<&'static str> {
        let mut names = if self.request.color_channels == 1 {
            vec!["Gray"]
        } else {
            vec!["Red", "Green", "Blue"]
        };
        let planes = usize::try_from(self.record().planes).unwrap_or(0);
        if planes > names.len() {
            names.push("Transparency");
        }
        names
    }

    fn simple_property(&mut self, key: u32) -> Option<isize> {
        let width = isize::try_from(self.request.image.width).unwrap_or(isize::MAX);
        let height = isize::try_from(self.request.image.height).unwrap_or(isize::MAX);
        let record = self.record();
        Some(match key {
            c::PROP_NUMBER_OF_CHANNELS => record.planes.into(),
            c::PROP_IMAGE_MODE => record.imageMode.into(),
            c::PROP_DOCUMENT_WIDTH => width,
            c::PROP_DOCUMENT_HEIGHT => height,
            c::PROP_BIG_NUDGE_H | c::PROP_BIG_NUDGE_V => fixed(10.0) as isize,
            c::PROP_INTERPOLATION_METHOD => 1,
            c::PROP_GRID_MAJOR => fixed(1.0) as isize,
            c::PROP_GRID_MINOR => 4,
            c::PROP_TOOL_TIPS => 1,
            c::PROP_NUMBER_OF_LAYERS => 1,
            c::PROP_DOCUMENT_ID => 1,
            c::PROP_VERSION => REPORTED_PHOTOSHOP_VERSION,
            c::PROP_PIXEL_SCALE_FACTOR => 1,
            c::PROP_NUMBER_OF_PATHS
            | c::PROP_RULER_UNITS
            | c::PROP_RULER_ORIGIN_H
            | c::PROP_RULER_ORIGIN_V
            | c::PROP_WATCH_SUSPENSION
            | c::PROP_COPYRIGHT
            | c::PROP_COPYRIGHT2
            | c::PROP_WATERMARK
            | c::PROP_PLAY_IN_PROGRESS
            | c::PROP_RECORD_IN_PROGRESS
            | c::PROP_TARGET_LAYER_INDEX
            | c::PROP_APP_IS_MODAL
            | c::PROP_TARGET_LAYER_LOCK => 0,
            _ => return None,
        })
    }

    fn complex_property(&mut self, key: u32, index: i32) -> Option<Vec<u8>> {
        let title = if self.request.document_title.is_empty() {
            "Untitled".to_string()
        } else {
            self.request.document_title.clone()
        };
        let layer = if self.request.layer_name.is_empty() {
            "Layer".to_string()
        } else {
            self.request.layer_name.clone()
        };
        let channel = usize::try_from(index)
            .ok()
            .and_then(|index| self.channel_names().get(index).copied());
        match key {
            c::PROP_TITLE => Some(title.into_bytes()),
            c::PROP_UNICODE_NAME => Some(utf16le(&title)),
            c::PROP_UNICODE_NAME_WITHOUT_EXTENSION => {
                let stem = title
                    .rsplit_once('.')
                    .map_or(title.as_str(), |(stem, _)| stem);
                Some(utf16le(stem))
            }
            c::PROP_LAYER_NAME => Some(layer.into_bytes()),
            c::PROP_UNICODE_LAYER_NAME => Some(utf16le(&layer)),
            c::PROP_CHANNEL_NAME => channel.map(|name| name.as_bytes().to_vec()),
            c::PROP_UNICODE_CHANNEL_NAME => channel.map(utf16le),
            c::PROP_SERIAL_STRING | c::PROP_SERIAL_STRING2 => Some(b"0".to_vec()),
            c::PROP_CAPTION | c::PROP_URL => Some(Vec::new()),
            _ => None,
        }
    }

    fn sample_point(&mut self, info: &mut s::ColorServicesInfo) -> i16 {
        // SAFETY: for the sample-point selector the union holds a point pointer.
        let point = unsafe { info.selectorParameter.globalSamplePoint };
        if point.is_null() {
            return c::ERR_INVALID_SAMPLE_POINT;
        }
        // SAFETY: checked non-null; the filter owns the point.
        let point = unsafe { *point };
        let image = &self.request.image;
        let (Ok(x), Ok(y)) = (usize::try_from(point.h), usize::try_from(point.v)) else {
            return c::ERR_INVALID_SAMPLE_POINT;
        };
        if x >= image.width || y >= image.height {
            return c::ERR_INVALID_SAMPLE_POINT;
        }
        let bps = image.bytes_per_sample();
        let offset = y * image.row_bytes() + x * image.pixel_bytes();
        let mut values = Vec::new();
        for channel in 0..self.request.color_channels {
            let start = offset + channel * bps;
            let sample = if bps == 2 {
                let raw = u16::from_ne_bytes([image.data[start], image.data[start + 1]]);
                i32::from(raw >> 7).min(255)
            } else {
                i32::from(image.data[start])
            };
            values.push(sample);
        }
        let rgb = match values.as_slice() {
            [gray] => [*gray; 3],
            [r, g, b, ..] => [*r, *g, *b],
            _ => return c::ERR_INVALID_SAMPLE_POINT,
        };
        store_color(info, rgb)
    }

    fn color_services(&mut self, info: &mut s::ColorServicesInfo) -> i16 {
        match info.selector {
            c::COLOR_SERVICES_CHOOSE_COLOR => {
                let initial = if info.sourceSpace >= 0 {
                    colors::space_to_rgb(info.colorComponents, info.sourceSpace)
                        .unwrap_or(widen(self.request.foreground))
                } else {
                    widen(self.request.foreground)
                };
                let Some(chosen) = colors::choose_color(initial, self.request.parent_window) else {
                    return if cfg!(windows) {
                        c::USER_CANCELED_ERR
                    } else {
                        c::ERR_HOST_DOES_NOT_SUPPORT_COLOR_STUFF
                    };
                };
                store_color(info, chosen)
            }
            c::COLOR_SERVICES_CONVERT_COLOR => {
                if info.resultSpace < 0 {
                    info.resultSpace = info.sourceSpace;
                }
                match colors::convert(info.colorComponents, info.sourceSpace, info.resultSpace) {
                    Ok(converted) => {
                        info.colorComponents = converted;
                        if info.resultGamutInfoValid != 0 {
                            info.resultInGamut = 1;
                        }
                        c::NO_ERR
                    }
                    Err(_) => c::ERR_UNSUPPORTED_COLOR_CONVERSION,
                }
            }
            c::COLOR_SERVICES_GET_SPECIAL_COLOR => {
                // SAFETY: for this selector the union holds the color id.
                let which = unsafe { info.selectorParameter.specialColorID };
                let rgb = match which {
                    c::SPECIAL_COLOR_FOREGROUND => self.request.foreground,
                    c::SPECIAL_COLOR_BACKGROUND => self.request.background,
                    _ => return c::PARAM_ERR,
                };
                store_color(info, widen(rgb))
            }
            c::COLOR_SERVICES_SAMPLE_POINT => self.sample_point(info),
            _ => c::PARAM_ERR,
        }
    }

    fn acquire_suite(&mut self, name: &[u8], version: i32) -> Option<*const c_void> {
        let key = (name.to_vec(), version);
        if let Some(suite) = self.suites.get(&key) {
            return Some(suite.address());
        }
        let suite = match (name, version) {
            (c::SUITE_HANDLE, 1) => Suite::Handle1(Owned::new(s::PSHandleSuite1 {
                New: Some(cb_handle_new),
                Dispose: Some(cb_handle_dispose),
                SetLock: Some(cb_handle_set_lock),
                GetSize: Some(cb_handle_size),
                SetSize: Some(cb_handle_set_size),
                RecoverSpace: Some(cb_recover_space),
            })),
            (c::SUITE_HANDLE, 2) => Suite::Handle2(Owned::new(s::PSHandleSuite2 {
                New: Some(cb_handle_new),
                Dispose: Some(cb_handle_dispose),
                DisposeRegularHandle: Some(cb_handle_dispose),
                SetLock: Some(cb_handle_set_lock),
                GetSize: Some(cb_handle_size),
                SetSize: Some(cb_handle_set_size),
                RecoverSpace: Some(cb_recover_space),
            })),
            (c::SUITE_BUFFER, 1 | 2) => Suite::Buffer(Owned::new(s::PSBufferSuite2 {
                New: Some(cb_buffer_new),
                Dispose: Some(cb_buffer_dispose),
                GetSize: Some(cb_buffer_get_size),
                GetSpace: Some(cb_buffer_get_space),
                New64: Some(cb_buffer_new64),
                GetSize64: Some(cb_buffer_get_size64),
                GetSpace64: Some(cb_buffer_get_space64),
            })),
            (c::SUITE_UI_HOOKS, 1..=4) => Suite::UiHooks(Owned::new(s::PSUIHooksSuite4 {
                processEvent: Some(cb_process_event),
                displayPixels: Some(cb_display_pixels),
                progressBar: Some(cb_progress),
                testAbort: Some(cb_abort),
                MainAppWindow: Some(cb_main_app_window),
                SetCursor: Some(cb_set_cursor),
                TickCount: Some(cb_tick_count),
                GetPluginName: Some(cb_unimplemented_2),
                SetPluginMenuEntry: Some(cb_unimplemented_3),
                GetLocalizedFileName: Some(cb_unimplemented_2),
                MainAppRootWindow: Some(cb_main_app_window),
            })),
            (c::SUITE_PROPERTY, 1) => Suite::Property(Owned::new(s::PSPropertySuite1 {
                getPropertyProc: Some(cb_get_property),
                setPropertyProc: Some(cb_set_property),
            })),
            _ => return None,
        };
        let address = suite.address();
        self.suites.insert(key, suite);
        Some(address)
    }
}

/// The parts of a request `setup_record` needs after `self` is borrowed.
struct RequestSettings {
    color_channels: usize,
    foreground: [u8; 3],
    background: [u8; 3],
    resolution: f64,
    has_mask: bool,
}

impl FilterRequest {
    fn clone_settings(&self) -> RequestSettings {
        RequestSettings {
            color_channels: self.color_channels,
            foreground: self.foreground,
            background: self.background,
            resolution: self.resolution,
            has_mask: self.mask.is_some(),
        }
    }
}

fn widen(rgb: [u8; 3]) -> [i32; 3] {
    rgb.map(i32::from)
}

fn to_ps_color(rgb: [u8; 3]) -> s::PSRGBColor {
    let [red, green, blue] = rgb.map(|value| u16::from(value) * 257);
    s::PSRGBColor { red, green, blue }
}

fn gray_of(rgb: [u8; 3]) -> u8 {
    colors::rgb_to_space(widen(rgb), c::COLOR_SPACE_GRAY)
        .map(|components| components[0].clamp(0, 255) as u8)
        .unwrap_or(0)
}

fn store_color(info: &mut s::ColorServicesInfo, rgb: [i32; 3]) -> i16 {
    let space = if info.resultSpace >= 0 {
        info.resultSpace
    } else {
        c::COLOR_SPACE_RGB
    };
    match colors::rgb_to_space(rgb, space) {
        Ok(components) => {
            info.resultSpace = space;
            info.colorComponents = components;
            c::NO_ERR
        }
        Err(_) => c::ERR_UNSUPPORTED_COLOR_CONVERSION,
    }
}

fn utf16le(text: &str) -> Vec<u8> {
    text.encode_utf16().flat_map(u16::to_le_bytes).collect()
}

fn decode_text(raw: &[u8]) -> String {
    match std::str::from_utf8(raw) {
        Ok(text) => text.to_string(),
        Err(_) => raw.iter().map(|&b| b as char).collect(),
    }
}

// The selector loop -------------------------------------------------------------

/// Calls one selector with no Rust reference to the session alive.
///
/// # Safety
///
/// `core` must point at the active session.
unsafe fn call_selector(
    module: &FilterModule,
    core: *mut SessionCore,
    state: *mut PluginState,
    selector: i16,
) -> i16 {
    // SAFETY: reading two raw pointers out of the live session.
    let record = unsafe { (*core).record.ptr() };
    // SAFETY: the record and the plug-in's data slot outlive the call.
    unsafe { module.call(selector, record.cast(), &mut (*state).data) }
}

fn check(result: i16, stage: &str) -> Result<(), Stop> {
    match result {
        c::NO_ERR => Ok(()),
        c::USER_CANCELED_ERR => Err(Stop::Cancelled),
        c::ERR_REPORT_STRING => {
            let message = with_core(String::new(), |core| core.error_text());
            let message = if message.is_empty() {
                "unknown error".to_string()
            } else {
                message
            };
            Err(Stop::Failed(
                format!("Could not complete the filter because {message}"),
                result,
            ))
        }
        _ => {
            let name = with_core("The filter".to_string(), |core| filter_name(&core.pipl));
            let message =
                c::error_message(result).map_or_else(|| format!("error {result}"), str::to_string);
            Err(Stop::Failed(
                format!("{name} failed during {stage}: {message}"),
                result,
            ))
        }
    }
}

fn record_parameters() -> Handle {
    with_core(std::ptr::null_mut(), |core| core.record().parameters)
}

fn any_rect_requested() -> bool {
    with_core(false, |core| {
        [Port::In, Port::Out, Port::Mask]
            .into_iter()
            .any(|port| !core.rect(port).is_empty())
    })
}

fn run_selectors(
    module: &FilterModule,
    core: *mut SessionCore,
    state: *mut PluginState,
) -> Result<(), Stop> {
    // SAFETY (whole function): `core` and `state` point at live values owned
    // by `run_session`; they are only dereferenced in short statements that
    // do not overlap filter calls.
    let (request_parameters, show_ui) = with_core((None, false), |core| {
        (core.request.parameters.clone(), core.request.show_ui)
    });

    let existing = unsafe { (*state).parameters };
    if let Some(bytes) = request_parameters {
        // A stored preset replaces whatever the filter used last time. If
        // it can't be loaded, stop: running with the previous or default
        // settings would report success with the wrong result.
        let loaded = if existing.is_null() {
            let handle =
                with_managers(|m| m.handles.allocate_from(&bytes)).unwrap_or(std::ptr::null_mut());
            unsafe { (*state).parameters = handle };
            !handle.is_null()
        } else {
            with_managers(|m| m.handles.write(existing, &bytes)) == Some(c::NO_ERR)
        };
        if !loaded {
            return Err(Stop::Failed(
                "could not load the preset's parameters".into(),
                c::MEM_FULL_ERR,
            ));
        }
    }
    let parameters = unsafe { (*state).parameters };
    with_core((), |core| core.record().parameters = parameters);

    if show_ui || parameters.is_null() {
        let result = unsafe { call_selector(module, core, state, c::SELECTOR_PARAMETERS) };
        check(result, "setup")?;
        unsafe { (*state).parameters = record_parameters() };
    }

    let result = unsafe { call_selector(module, core, state, c::SELECTOR_PREPARE) };
    check(result, "prepare")?;
    unsafe { (*state).parameters = record_parameters() };

    let result = unsafe { call_selector(module, core, state, c::SELECTOR_START) };
    check(result, "start")?;

    let processed = (|| {
        let mut calls: u64 = 0;
        while any_rect_requested() {
            let error = with_core(c::PARAM_ERR, SessionCore::advance_state);
            check(error, "pixel transfer")?;
            let result = unsafe { call_selector(module, core, state, c::SELECTOR_CONTINUE) };
            check(result, "processing")?;
            calls += 1;
            if calls > MAX_CONTINUE_CALLS {
                return Err(Stop::Failed(
                    "the filter never finished processing".into(),
                    c::PARAM_ERR,
                ));
            }
            if with_core(false, |core| (core.abort)()) {
                return Err(Stop::Cancelled);
            }
        }
        with_core((), SessionCore::flush_output);
        Ok(())
    })();
    if let Err(stop) = processed {
        unsafe { call_selector(module, core, state, c::SELECTOR_FINISH) };
        return Err(stop);
    }
    let result = unsafe { call_selector(module, core, state, c::SELECTOR_FINISH) };
    check(result, "finish")?;
    unsafe { (*state).parameters = record_parameters() };
    Ok(())
}

fn run_session(
    module: &FilterModule,
    pipl: &PiPL,
    state: *mut PluginState,
    request: FilterRequest,
    progress: Box<dyn FnMut(i32, i32)>,
    abort: Box<dyn FnMut() -> bool>,
) -> FilterResult {
    let mut core = Box::new(SessionCore::new(request, pipl, progress, abort));
    let core_ptr: *mut SessionCore = &mut *core;
    let activation = Activation::new(core_ptr);

    let prepared = with_core(
        Err(Stop::Failed("host busy".into(), c::PARAM_ERR)),
        |core| {
            core.prepare_work_image()?;
            core.setup_record()
        },
    );
    let outcome = prepared.and_then(|()| run_selectors(module, core_ptr, state));
    let result = match outcome {
        Err(Stop::Cancelled) => FilterResult {
            status: RunStatus::Cancelled,
            message: "Cancelled.".into(),
            code: c::USER_CANCELED_ERR,
            ..FilterResult::error("", 0)
        },
        Err(Stop::Failed(message, code)) => FilterResult::error(message, code),
        Ok(()) => with_core(FilterResult::error("host busy", c::PARAM_ERR), |core| {
            let image = core.finish_image();
            // SAFETY: `state` is owned by the FilterHost for the whole run.
            let parameters_handle = unsafe { (*state).parameters };
            let descriptor_handle = {
                // SAFETY: the parameters block lives as long as the session.
                unsafe { (*core.descriptor_parameters.ptr()).descriptor }
            };
            let (parameters, descriptor) = with_managers(|m| {
                let parameters =
                    (!parameters_handle.is_null()).then(|| m.handles.read(parameters_handle));
                let descriptor = m.descriptors.descriptor_for(&m.handles, descriptor_handle);
                (parameters, descriptor)
            })
            .unwrap_or((None, None));
            FilterResult {
                status: RunStatus::Ok,
                message: String::new(),
                image: Some(image),
                parameters,
                descriptor,
                code: c::NO_ERR,
                log: Vec::new(),
            }
        }),
    };
    drop(activation);
    drop(core);
    result
}

fn run_about(
    module: &FilterModule,
    pipl: &PiPL,
    state: *mut PluginState,
    parent_window: isize,
) -> FilterResult {
    let placeholder = match PlaneImage::new(1, 1, 3, 8) {
        Ok(image) => image,
        Err(error) => return FilterResult::error(error.to_string(), c::MEM_FULL_ERR),
    };
    let mut request = FilterRequest::new(placeholder, 3, false);
    request.parent_window = parent_window;
    let mut core = Box::new(SessionCore::new(
        request,
        pipl,
        Box::new(|_, _| {}),
        Box::new(|| false),
    ));
    let core_ptr: *mut SessionCore = &mut *core;
    let activation = Activation::new(core_ptr);
    let mut about = Box::new(s::AboutRecord {
        platformData: core.platform.ptr(),
        sSPBasic: core.basic_suite.ptr().cast_const(),
        plugInRef: std::ptr::null_mut(),
        reserved: [0; 244],
    });
    let about_ptr: *mut s::AboutRecord = &mut *about;
    // SAFETY: the About record and everything it points at outlive the call;
    // `state` is owned by the FilterHost.
    let result = unsafe { module.call(c::SELECTOR_ABOUT, about_ptr.cast(), &mut (*state).data) };
    drop(activation);
    if result == c::NO_ERR || result == c::USER_CANCELED_ERR {
        return FilterResult {
            status: RunStatus::Ok,
            ..FilterResult::error("", 0)
        };
    }
    let message =
        c::error_message(result).map_or_else(|| format!("error {result}"), str::to_string);
    FilterResult::error(message, result)
}

// Static proc tables ---------------------------------------------------------------

fn handle_procs() -> s::HandleProcs {
    s::HandleProcs {
        handleProcsVersion: 1,
        numHandleProcs: 8,
        newProc: Some(cb_handle_new),
        disposeProc: Some(cb_handle_dispose),
        getSizeProc: Some(cb_handle_size),
        setSizeProc: Some(cb_handle_set_size),
        lockProc: Some(cb_handle_lock),
        unlockProc: Some(cb_handle_unlock),
        recoverSpaceProc: Some(cb_recover_space),
        disposeRegularHandleProc: Some(cb_handle_dispose),
    }
}

fn buffer_procs() -> s::BufferProcs {
    s::BufferProcs {
        bufferProcsVersion: 2,
        numBufferProcs: 8,
        allocateProc: Some(cb_buffer_allocate),
        lockProc: Some(cb_buffer_lock),
        unlockProc: Some(cb_buffer_unlock),
        freeProc: Some(cb_buffer_free),
        spaceProc: Some(cb_buffer_space),
        reserveProc: Some(cb_buffer_reserve),
        allocateProc64: Some(cb_buffer_allocate64),
        spaceProc64: Some(cb_buffer_space64),
    }
}

fn resource_procs() -> s::ResourceProcs {
    s::ResourceProcs {
        resourceProcsVersion: 3,
        numResourceProcs: 4,
        countProc: Some(cb_resource_count),
        getProc: Some(cb_resource_get),
        deleteProc: Some(cb_resource_delete),
        addProc: Some(cb_resource_add),
    }
}

fn property_procs() -> s::PropertyProcs {
    s::PropertyProcs {
        propertyProcsVersion: 1,
        numPropertyProcs: 2,
        getPropertyProc: Some(cb_get_property),
        setPropertyProc: Some(cb_set_property),
    }
}

fn basic_suite() -> s::SPBasicSuite {
    s::SPBasicSuite {
        AcquireSuite: Some(cb_acquire_suite),
        ReleaseSuite: Some(cb_release_suite),
        IsEqual: Some(cb_is_equal),
        AllocateBlock: Some(cb_allocate_block),
        FreeBlock: Some(cb_free_block),
        ReallocateBlock: Some(cb_reallocate_block),
        Undefined: Some(cb_undefined),
    }
}

// extern "C" callbacks ---------------------------------------------------------------
//
// Each callback validates the pointers it was handed, wraps its body in
// `guard` so a host panic becomes an error code, and keeps its borrow of
// the session or the managers to the length of the call.

unsafe extern "C" fn cb_abort() -> s::Boolean {
    guard("abortProc", 0, || {
        with_core(0, |core| u8::from((core.abort)()))
    })
}

unsafe extern "C" fn cb_progress(done: i32, total: i32) {
    guard("progressProc", (), || {
        with_core((), |core| (core.progress)(done, total))
    });
}

unsafe extern "C" fn cb_host_proc(_selector: i16, _data: *mut isize) {}

unsafe extern "C" fn cb_process_event(_event: *mut c_void) {}

unsafe extern "C" fn cb_advance_state() -> s::OSErr {
    guard("advanceState", c::MEM_FULL_ERR, || {
        with_core(c::PARAM_ERR, SessionCore::advance_state)
    })
}

unsafe extern "C" fn cb_display_pixels(
    map: *const s::PSPixelMap,
    src: *const s::VRect,
    dst_row: i32,
    dst_col: i32,
    context: *mut c_void,
) -> s::OSErr {
    guard("displayPixels", c::PARAM_ERR, || {
        if map.is_null() || src.is_null() {
            return c::PARAM_ERR;
        }
        // SAFETY: checked non-null; the filter owns the pixel map.
        let map = unsafe { copy_pixel_map(map) };
        // SAFETY: checked non-null; the rect belongs to the filter.
        let src = unsafe { &*src };
        // SAFETY: the filter vouches for its pixel map and drawing context,
        // as the plug-in API requires.
        unsafe { display::display_pixels(&map, src, dst_row, dst_col, context) }
    })
}

/// Copies the part of a filter's pixel map its version says exists.
///
/// Older filters pass shorter structs, so a reference over the whole
/// current struct would cover memory the filter never allocated. Fields
/// past the filter's version are left zero.
///
/// # Safety
///
/// `map` must be non-null and point at a pixel map of the version it
/// declares.
unsafe fn copy_pixel_map(map: *const s::PSPixelMap) -> s::PSPixelMap {
    use std::mem::{offset_of, size_of};
    // SAFETY: every version starts with `version`.
    let version = unsafe { std::ptr::addr_of!((*map).version).read_unaligned() };
    let len = match version {
        ..=0 => offset_of!(s::PSPixelMap, mat),
        1 => offset_of!(s::PSPixelMap, pixelOverlays),
        2 => offset_of!(s::PSPixelMap, depth),
        _ => size_of::<s::PSPixelMap>(),
    };
    // SAFETY: the struct holds only integers and raw pointers, for which
    // all-zero bytes are valid values.
    let mut copy: s::PSPixelMap = unsafe { std::mem::zeroed() };
    // SAFETY: `len` bytes are readable at `map` for its declared version,
    // and `copy` is at least `len` bytes long.
    unsafe {
        std::ptr::copy_nonoverlapping(
            map.cast::<u8>(),
            std::ptr::addr_of_mut!(copy).cast::<u8>(),
            len,
        );
    }
    copy
}

unsafe extern "C" fn cb_color_services(info: *mut s::ColorServicesInfo) -> s::OSErr {
    guard("colorServices", c::PARAM_ERR, || {
        if info.is_null() {
            return c::PARAM_ERR;
        }
        // The filter states the size of its block; a block too small for
        // the struct would be read and written past its end.
        // SAFETY: checked non-null; every version starts with `infoSize`.
        let size = unsafe { std::ptr::addr_of!((*info).infoSize).read_unaligned() };
        if usize::try_from(size).map_or(true, |size| {
            size < std::mem::size_of::<s::ColorServicesInfo>()
        }) {
            return c::PARAM_ERR;
        }
        // SAFETY: checked non-null and large enough; the filter owns the block.
        let info = unsafe { &mut *info };
        with_core(c::PARAM_ERR, |core| core.color_services(info))
    })
}

unsafe extern "C" fn cb_get_property(
    _signature: u32,
    key: u32,
    index: i32,
    simple: *mut isize,
    complex: *mut Handle,
) -> s::OSErr {
    guard("getProperty", c::ERR_PLUGIN_PROPERTY_UNDEFINED, || {
        let found = with_core(Err(c::ERR_PLUGIN_PROPERTY_UNDEFINED), |core| {
            if let Some(value) = core.simple_property(key) {
                return Ok(Ok(value));
            }
            core.complex_property(key, index)
                .map(Err)
                .ok_or(c::ERR_PLUGIN_PROPERTY_UNDEFINED)
        });
        match found {
            Ok(Ok(value)) => {
                if !simple.is_null() {
                    // SAFETY: checked non-null; the filter provided the slot.
                    unsafe { *simple = value };
                }
                c::NO_ERR
            }
            Ok(Err(bytes)) => {
                if complex.is_null() {
                    return c::PARAM_ERR;
                }
                let handle = with_managers(|m| m.handles.allocate_from(&bytes))
                    .unwrap_or(std::ptr::null_mut());
                if handle.is_null() {
                    return c::MEM_FULL_ERR;
                }
                // SAFETY: checked non-null; the filter provided the slot.
                unsafe { *complex = handle };
                c::NO_ERR
            }
            Err(code) => code,
        }
    })
}

unsafe extern "C" fn cb_set_property(
    _signature: u32,
    key: u32,
    _index: i32,
    _simple: isize,
    _complex: Handle,
) -> s::OSErr {
    // Accept writes to properties filters commonly set; nothing persists.
    match key {
        c::PROP_BIG_NUDGE_H
        | c::PROP_BIG_NUDGE_V
        | c::PROP_CAPTION
        | c::PROP_COPYRIGHT
        | c::PROP_URL
        | c::PROP_WATERMARK
        | c::PROP_EXIF_DATA
        | c::PROP_XMP => c::NO_ERR,
        _ => c::ERR_PLUGIN_PROPERTY_UNDEFINED,
    }
}

unsafe extern "C" fn cb_handle_new(size: i32) -> Handle {
    guard("newHandle", std::ptr::null_mut(), || {
        with_managers(|m| m.handles.allocate(size)).unwrap_or(std::ptr::null_mut())
    })
}

unsafe extern "C" fn cb_handle_dispose(handle: Handle) {
    guard("disposeHandle", (), || {
        if with_managers(|m| m.handles.dispose(handle)) == Some(false) {
            context::log(&format!("dispose of unknown handle {handle:p} ignored"));
        }
    });
}

unsafe extern "C" fn cb_handle_size(handle: Handle) -> i32 {
    guard("getHandleSize", 0, || {
        with_managers(|m| m.handles.size(handle)).unwrap_or(0)
    })
}

unsafe extern "C" fn cb_handle_set_size(handle: Handle, size: i32) -> s::OSErr {
    guard("setHandleSize", c::MEM_FULL_ERR, || {
        with_managers(|m| m.handles.set_size(handle, size)).unwrap_or(c::MEM_FULL_ERR)
    })
}

unsafe extern "C" fn cb_handle_lock(handle: Handle, _move_high: s::Boolean) -> s::Ptr {
    guard("lockHandle", std::ptr::null_mut(), || {
        let pointer = with_managers(|m| m.handles.lock(handle)).unwrap_or(std::ptr::null_mut());
        if pointer.is_null() {
            context::log(&format!("lock of unknown handle {handle:p}"));
        }
        pointer
    })
}

unsafe extern "C" fn cb_handle_unlock(handle: Handle) {
    guard("unlockHandle", (), || {
        with_managers(|m| m.handles.unlock(handle));
    });
}

unsafe extern "C" fn cb_handle_set_lock(
    handle: Handle,
    lock: s::Boolean,
    address: *mut s::Ptr,
    old_lock: *mut s::Boolean,
) {
    guard("SetLock", (), || {
        if !old_lock.is_null() {
            // SAFETY: checked non-null; the filter provided the slot.
            unsafe { *old_lock = 0 };
        }
        if lock == 0 {
            with_managers(|m| m.handles.unlock(handle));
            return;
        }
        let pointer = with_managers(|m| m.handles.lock(handle)).unwrap_or(std::ptr::null_mut());
        if !address.is_null() {
            // SAFETY: checked non-null; the filter provided the slot.
            unsafe { *address = pointer };
        }
    });
}

unsafe extern "C" fn cb_recover_space(_size: i32) {}

fn allocate_buffer(size: i64, out: *mut *mut c_void) -> s::OSErr {
    if out.is_null() {
        return c::PARAM_ERR;
    }
    let address = with_managers(|m| m.buffers.allocate(size)).unwrap_or(std::ptr::null_mut());
    if address.is_null() {
        return c::MEM_FULL_ERR;
    }
    // SAFETY: checked non-null; the filter provided the slot.
    unsafe { *out = address };
    c::NO_ERR
}

unsafe extern "C" fn cb_buffer_allocate(size: i32, out: *mut *mut c_void) -> s::OSErr {
    guard("allocateBuffer", c::MEM_FULL_ERR, || {
        allocate_buffer(size.into(), out)
    })
}

unsafe extern "C" fn cb_buffer_allocate64(size: i64, out: *mut *mut c_void) -> s::OSErr {
    guard("allocateBuffer64", c::MEM_FULL_ERR, || {
        allocate_buffer(size, out)
    })
}

unsafe extern "C" fn cb_buffer_lock(buffer: *mut c_void, _move_high: s::Boolean) -> s::Ptr {
    buffer
}

unsafe extern "C" fn cb_buffer_unlock(_buffer: *mut c_void) {}

unsafe extern "C" fn cb_buffer_free(buffer: *mut c_void) {
    guard("freeBuffer", (), || {
        with_managers(|m| m.buffers.free(buffer));
    });
}

fn buffer_space() -> i64 {
    with_managers(|m| m.buffers.space).unwrap_or(crate::host::memory::DEFAULT_SPACE)
}

unsafe extern "C" fn cb_buffer_space() -> i32 {
    i32::try_from(buffer_space()).unwrap_or(i32::MAX)
}

unsafe extern "C" fn cb_buffer_space64() -> i64 {
    buffer_space()
}

unsafe extern "C" fn cb_buffer_reserve(_size: i32) -> s::OSErr {
    c::NO_ERR
}

fn buffer_new(requested: u64, minimum: u64) -> (s::Ptr, u64) {
    with_managers(|m| {
        let mut size = requested;
        let mut address = m.buffers.allocate(i64::try_from(size).unwrap_or(-1));
        if address.is_null() && minimum < requested {
            size = minimum;
            address = m.buffers.allocate(i64::try_from(size).unwrap_or(-1));
        }
        (address, size)
    })
    .unwrap_or((std::ptr::null_mut(), 0))
}

unsafe extern "C" fn cb_buffer_new(size: *mut u32, minimum: u32) -> s::Ptr {
    guard("Buffer New", std::ptr::null_mut(), || {
        // SAFETY: the size pointer is optional; read it only when non-null.
        let requested = if size.is_null() {
            minimum
        } else {
            unsafe { *size }
        };
        let (address, granted) = buffer_new(requested.into(), minimum.into());
        if !address.is_null() && !size.is_null() {
            // SAFETY: checked non-null; granted never exceeds the u32 request.
            unsafe { *size = u32::try_from(granted).unwrap_or(u32::MAX) };
        }
        address
    })
}

unsafe extern "C" fn cb_buffer_new64(size: *mut u64, minimum: u64) -> s::Ptr {
    guard("Buffer New64", std::ptr::null_mut(), || {
        // SAFETY: the size pointer is optional; read it only when non-null.
        let requested = if size.is_null() {
            minimum
        } else {
            unsafe { *size }
        };
        let (address, granted) = buffer_new(requested, minimum);
        if !address.is_null() && !size.is_null() {
            // SAFETY: checked non-null.
            unsafe { *size = granted };
        }
        address
    })
}

unsafe extern "C" fn cb_buffer_dispose(buffer: *mut s::Ptr) {
    guard("Buffer Dispose", (), || {
        if buffer.is_null() {
            return;
        }
        // SAFETY: checked non-null; the filter passes the address of its pointer.
        let address = unsafe { *buffer };
        if address.is_null() {
            return;
        }
        with_managers(|m| m.buffers.free(address));
        // SAFETY: as above.
        unsafe { *buffer = std::ptr::null_mut() };
    });
}

unsafe extern "C" fn cb_buffer_get_size(buffer: s::Ptr) -> u32 {
    guard("Buffer GetSize", 0, || {
        let size = with_managers(|m| m.buffers.size(buffer)).unwrap_or(0);
        u32::try_from(size).unwrap_or(u32::MAX)
    })
}

unsafe extern "C" fn cb_buffer_get_size64(buffer: s::Ptr) -> u64 {
    guard("Buffer GetSize64", 0, || {
        let size = with_managers(|m| m.buffers.size(buffer)).unwrap_or(0);
        u64::try_from(size).unwrap_or(u64::MAX)
    })
}

unsafe extern "C" fn cb_buffer_get_space() -> u32 {
    u32::try_from(buffer_space()).unwrap_or(u32::MAX)
}

unsafe extern "C" fn cb_buffer_get_space64() -> u64 {
    u64::try_from(buffer_space()).unwrap_or(0)
}

fn matching_resources(core: &SessionCore, kind: u32) -> Vec<usize> {
    core.resources
        .iter()
        .enumerate()
        .filter(|(_, (resource_kind, _))| *resource_kind == kind)
        .map(|(index, _)| index)
        .collect()
}

/// Maps a 1-based resource index to a position in the resource list.
fn resource_position(core: &SessionCore, kind: u32, index: i16) -> Option<usize> {
    let index = usize::try_from(index).ok()?.checked_sub(1)?;
    matching_resources(core, kind).get(index).copied()
}

unsafe extern "C" fn cb_resource_count(kind: u32) -> i16 {
    guard("countResources", 0, || {
        with_core(0, |core| {
            i16::try_from(matching_resources(core, kind).len()).unwrap_or(i16::MAX)
        })
    })
}

unsafe extern "C" fn cb_resource_get(kind: u32, index: i16) -> Handle {
    guard("getResource", std::ptr::null_mut(), || {
        let data = with_core(None, |core| {
            resource_position(core, kind, index).map(|position| core.resources[position].1.clone())
        });
        match data {
            Some(data) => {
                with_managers(|m| m.handles.allocate_from(&data)).unwrap_or(std::ptr::null_mut())
            }
            None => std::ptr::null_mut(),
        }
    })
}

unsafe extern "C" fn cb_resource_delete(kind: u32, index: i16) {
    guard("deleteResource", (), || {
        with_core((), |core| {
            if let Some(position) = resource_position(core, kind, index) {
                core.resources.remove(position);
            }
        });
    });
}

unsafe extern "C" fn cb_resource_add(kind: u32, handle: Handle) -> s::OSErr {
    guard("addResource", c::MEM_FULL_ERR, || {
        let data = with_managers(|m| m.handles.read(handle)).unwrap_or_default();
        with_core(c::MEM_FULL_ERR, |core| {
            core.resources.push((kind, data));
            c::NO_ERR
        })
    })
}

unsafe extern "C" fn cb_acquire_suite(
    name: *const c_char,
    version: i32,
    suite: *mut *const c_void,
) -> s::SPErr {
    guard("AcquireSuite", c::SP_SUITE_NOT_FOUND, || {
        if suite.is_null() {
            return c::SP_BAD_PARAMETER;
        }
        let name_bytes = if name.is_null() {
            Vec::new()
        } else {
            // SAFETY: suite names are NUL-terminated C strings.
            unsafe { CStr::from_ptr(name) }.to_bytes().to_vec()
        };
        let found = with_core(None, |core| core.acquire_suite(&name_bytes, version));
        match found {
            Some(address) => {
                // SAFETY: checked non-null; the filter provided the slot.
                unsafe { *suite = address };
                c::SP_NO_ERROR
            }
            None => {
                context::log(&format!(
                    "suite not available: {} v{version}",
                    String::from_utf8_lossy(&name_bytes)
                ));
                // SAFETY: as above.
                unsafe { *suite = std::ptr::null() };
                c::SP_SUITE_NOT_FOUND
            }
        }
    })
}

unsafe extern "C" fn cb_release_suite(_name: *const c_char, _version: i32) -> s::SPErr {
    c::SP_NO_ERROR
}

unsafe extern "C" fn cb_is_equal(a: *const c_char, b: *const c_char) -> s::Boolean {
    guard("IsEqual", 0, || {
        if a.is_null() || b.is_null() {
            return u8::from(a == b);
        }
        // SAFETY: both are NUL-terminated C strings from the filter.
        let equal = unsafe { CStr::from_ptr(a) == CStr::from_ptr(b) };
        u8::from(equal)
    })
}

unsafe extern "C" fn cb_allocate_block(size: usize, block: *mut *mut c_void) -> s::SPErr {
    guard("AllocateBlock", c::SP_OUT_OF_MEMORY, || {
        if block.is_null() {
            return c::SP_BAD_PARAMETER;
        }
        let address = with_managers(|m| m.blocks.allocate(size)).unwrap_or(std::ptr::null_mut());
        if address.is_null() {
            return c::SP_OUT_OF_MEMORY;
        }
        // SAFETY: checked non-null; the filter provided the slot.
        unsafe { *block = address };
        c::SP_NO_ERROR
    })
}

unsafe extern "C" fn cb_free_block(block: *mut c_void) -> s::SPErr {
    guard("FreeBlock", c::SP_NO_ERROR, || {
        with_managers(|m| m.blocks.free(block));
        c::SP_NO_ERROR
    })
}

unsafe extern "C" fn cb_reallocate_block(
    block: *mut c_void,
    size: usize,
    out: *mut *mut c_void,
) -> s::SPErr {
    guard("ReallocateBlock", c::SP_OUT_OF_MEMORY, || {
        if out.is_null() {
            return c::SP_BAD_PARAMETER;
        }
        let address =
            with_managers(|m| m.blocks.reallocate(block, size)).unwrap_or(std::ptr::null_mut());
        if address.is_null() {
            return c::SP_OUT_OF_MEMORY;
        }
        // SAFETY: checked non-null; the filter provided the slot.
        unsafe { *out = address };
        c::SP_NO_ERROR
    })
}

unsafe extern "C" fn cb_undefined() -> s::SPErr {
    c::SP_UNIMPLEMENTED
}

unsafe extern "C" fn cb_main_app_window() -> isize {
    guard("MainAppWindow", 0, || {
        with_core(0, |core| core.request.parent_window)
    })
}

unsafe extern "C" fn cb_set_cursor(_cursor: *const c_char) -> s::SPErr {
    c::SP_NO_ERROR
}

unsafe extern "C" fn cb_tick_count() -> u32 {
    static START: OnceLock<Instant> = OnceLock::new();
    let elapsed = START.get_or_init(Instant::now).elapsed();
    // Classic Mac OS ticks: sixtieths of a second, wrapping at 32 bits.
    ((elapsed.as_millis() * 60 / 1000) & u128::from(u32::MAX)) as u32
}

unsafe extern "C" fn cb_unimplemented_2(_a: *mut c_void, _b: *mut *mut c_void) -> s::SPErr {
    c::SP_UNIMPLEMENTED
}

unsafe extern "C" fn cb_unimplemented_3(_a: *mut c_void, _b: *mut c_void, _c: u32) -> s::SPErr {
    c::SP_UNIMPLEMENTED
}
