//! A counting allocator for tests: the most bytes one thread held live between two points.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

struct Counting;

thread_local! {
    static LIVE: Cell<usize> = const { Cell::new(0) };
    static PEAK: Cell<usize> = const { Cell::new(0) };
}

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        let live = LIVE.with(|c| {
            c.set(c.get() + l.size());
            c.get()
        });
        PEAK.with(|p| p.set(p.get().max(live)));
        System.alloc(l)
    }

    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        LIVE.with(|c| c.set(c.get().saturating_sub(l.size())));
        System.dealloc(p, l)
    }
}

#[global_allocator]
static ALLOC: Counting = Counting;

/// The most bytes `f` held live at once on this thread, beyond what was held when it began.
pub fn peak_of<T>(f: impl FnOnce() -> T) -> (T, usize) {
    let base = LIVE.with(Cell::get);
    PEAK.with(|p| p.set(base));
    let out = f();
    (out, PEAK.with(Cell::get) - base)
}
