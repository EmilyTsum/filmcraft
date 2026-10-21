//! Retryable CPU readback signalling for preview jobs. An error remains an error, while
//! workers use this flag to avoid caching it as a terminal failure.
use std::cell::Cell;
thread_local! { static PENDING: Cell<bool> = const { Cell::new(false) }; }
/// Mark a retryable readback on this thread.
pub fn mark() {
    PENDING.with(|p| p.set(true));
}
/// Return and clear the retryable-readback flag at a job boundary.
pub fn take() -> bool {
    PENDING.with(|p| p.replace(false))
}
