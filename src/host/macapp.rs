//! Lets a command-line process show Cocoa filter dialogs on macOS.
//!
//! Filters open Cocoa windows for their dialogs. That only works once the
//! process has an `NSApplication` that may come to the front, which a
//! command-line program does not set up by itself.

/// Creates and activates the shared `NSApplication`. Safe to call twice.
///
/// Returns false on other platforms, or when AppKit is unavailable.
#[cfg(target_os = "macos")]
pub fn ensure_application() -> bool {
    use std::ffi::{c_char, c_void};

    const ACTIVATION_POLICY_REGULAR: isize = 0;

    #[link(name = "objc")]
    extern "C" {
        fn objc_getClass(name: *const c_char) -> *mut c_void;
        fn sel_registerName(name: *const c_char) -> *mut c_void;
        fn objc_msgSend();
    }
    #[link(name = "AppKit", kind = "framework")]
    extern "C" {}

    type SendId = unsafe extern "C" fn(*mut c_void, *mut c_void) -> *mut c_void;
    type SendLong = unsafe extern "C" fn(*mut c_void, *mut c_void, isize) -> bool;
    type SendBool = unsafe extern "C" fn(*mut c_void, *mut c_void, bool);

    // SAFETY: objc_msgSend is called through function pointer types that
    // match each method's real signature, as the Objective-C ABI requires.
    unsafe {
        let class = objc_getClass(c"NSApplication".as_ptr());
        if class.is_null() {
            return false;
        }
        let send_id: SendId = std::mem::transmute(objc_msgSend as unsafe extern "C" fn());
        let send_long: SendLong = std::mem::transmute(objc_msgSend as unsafe extern "C" fn());
        let send_bool: SendBool = std::mem::transmute(objc_msgSend as unsafe extern "C" fn());
        let app = send_id(class, sel_registerName(c"sharedApplication".as_ptr()));
        if app.is_null() {
            return false;
        }
        send_long(
            app,
            sel_registerName(c"setActivationPolicy:".as_ptr()),
            ACTIVATION_POLICY_REGULAR,
        );
        send_bool(
            app,
            sel_registerName(c"activateIgnoringOtherApps:".as_ptr()),
            true,
        );
    }
    true
}

/// Creates and activates the shared `NSApplication`. Safe to call twice.
///
/// Returns false on other platforms, or when AppKit is unavailable.
#[cfg(not(target_os = "macos"))]
pub fn ensure_application() -> bool {
    false
}
