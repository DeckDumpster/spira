//! What a verb says on stderr, held per thread until its caller takes it.

use std::cell::RefCell;

thread_local! {
    static ERR: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
}

pub fn push(line: String) {
    ERR.with(|e| e.borrow_mut().push(line));
}

/// Every line said since the last take, oldest first.
pub fn take() -> Vec<String> {
    ERR.with(|e| std::mem::take(&mut *e.borrow_mut()))
}
