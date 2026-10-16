//! Per-thread host state reached from `extern "C"` callbacks.
//!
//! Filters call the host back on the thread that invoked them, through
//! plain C function pointers that carry no user data. The host therefore
//! keeps its memory managers in a thread-local [`Managers`] value, and
//! every callback borrows it for the duration of one call. Nothing holds a
//! borrow while control is inside filter code, so callbacks never collide.
//!
//! One consequence: everything a filter allocates belongs to the thread it
//! ran on. Run all filters of one host on a single thread.

use std::cell::RefCell;
use std::panic::{catch_unwind, AssertUnwindSafe};

use super::descriptors::DescriptorSuite;
use super::memory::{BlockAllocator, BufferManager, HandleManager};

/// The memory and descriptor state shared by every filter run on a thread.
#[derive(Default)]
pub struct Managers {
    pub handles: HandleManager,
    pub buffers: BufferManager,
    pub blocks: BlockAllocator,
    pub descriptors: DescriptorSuite,
}

type LogSink = Box<dyn FnMut(&str)>;

thread_local! {
    static MANAGERS: RefCell<Managers> = RefCell::new(Managers::default());
    static LOG_SINK: RefCell<Option<LogSink>> = const { RefCell::new(None) };
}

/// Runs `f` with this thread's managers.
///
/// Returns `None` if the managers are already borrowed further up the
/// stack, which would mean a host callback re-entered another one.
pub fn with_managers<R>(f: impl FnOnce(&mut Managers) -> R) -> Option<R> {
    MANAGERS.with(|cell| {
        cell.try_borrow_mut()
            .ok()
            .map(|mut managers| f(&mut managers))
    })
}

/// Sends a diagnostic line to the current log sink, if one is installed.
pub fn log(message: &str) {
    LOG_SINK.with(|cell| {
        if let Ok(mut sink) = cell.try_borrow_mut() {
            if let Some(sink) = sink.as_mut() {
                sink(message);
            }
        }
    });
}

/// Installs a log sink for this thread and returns the previous one.
pub fn set_log_sink(sink: Option<LogSink>) -> Option<LogSink> {
    LOG_SINK.with(|cell| std::mem::replace(&mut *cell.borrow_mut(), sink))
}

/// Runs a callback body, turning a panic into `default`.
///
/// Unwinding into filter code is undefined behaviour, so every
/// `extern "C"` callback wraps its body in this.
pub fn guard<R>(name: &str, default: R, f: impl FnOnce() -> R) -> R {
    match catch_unwind(AssertUnwindSafe(f)) {
        Ok(value) => value,
        Err(payload) => {
            // Nothing may unwind from here either: the log sink is caller
            // code that could panic, and so could the payload's `Drop`.
            std::mem::forget(payload);
            let _ = catch_unwind(AssertUnwindSafe(|| {
                log(&format!("host callback {name} panicked"));
            }));
            default
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    #[test]
    fn nested_borrows_are_refused_instead_of_panicking() {
        let inner = with_managers(|_| with_managers(|_| ()));
        assert_eq!(inner, Some(None));
    }

    #[test]
    fn guard_turns_panics_into_defaults() {
        let lines = Arc::new(Mutex::new(Vec::new()));
        let sink_lines = Arc::clone(&lines);
        set_log_sink(Some(Box::new(move |line| {
            sink_lines.lock().unwrap().push(line.to_string())
        })));
        let value = guard("test", 7, || -> i32 { panic!("boom") });
        set_log_sink(None);
        assert_eq!(value, 7);
        assert_eq!(
            lines.lock().unwrap().as_slice(),
            ["host callback test panicked"]
        );
    }
}
