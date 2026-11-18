//! Line maps of original (pre-mapper) texts, shared across diagnostics.
//!
//! Go builds a new line map per diagnostic in `newOriginalTextFile`, which is
//! quadratic for a file with many diagnostics. The map of the last original
//! text (same allocation, kept alive here) is reused; the caller's file value
//! stays a new one per diagnostic, as the error summary compares files by
//! identity.

use std::cell::RefCell;
use std::sync::Arc;

thread_local! {
    static LAST: RefCell<Option<(&'static str, Arc<[u32]>)>> = const { RefCell::new(None) };
}

/// The ECMAScript line starts of `text`, shared with the previous call when
/// it was the same text.
pub fn original_line_map(text: &'static str) -> Arc<[u32]> {
    LAST.with(|last| {
        let mut last = last.borrow_mut();
        if let Some((prev, map)) = last.as_ref()
            && std::ptr::eq(prev.as_ptr(), text.as_ptr())
            && prev.len() == text.len()
        {
            return Arc::clone(map);
        }
        let map: Arc<[u32]> = tsrs_core::compute_ecma_line_starts(text).into();
        *last = Some((text, Arc::clone(&map)));
        map
    })
}
