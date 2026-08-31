//! PyTime helpers (CPython's internal/pycore_time.h). `_PyTime_t` is an i64
//! count of nanoseconds.

use core::ffi::c_int;
use std::sync::OnceLock;
use std::time::Instant;

static MONOTONIC_START: OnceLock<Instant> = OnceLock::new();

/// PyTime_Monotonic: store the monotonic clock (in nanoseconds) into `tp`.
/// Returns 0 on success, -1 on error.
#[unsafe(no_mangle)]
pub extern "C" fn PyTime_Monotonic(tp: *mut i64) -> c_int {
    if tp.is_null() {
        return -1;
    }
    let start = *MONOTONIC_START.get_or_init(Instant::now);
    unsafe { *tp = start.elapsed().as_nanos() as i64 };
    0
}

/// PyTime_AsSecondsDouble: convert a _PyTime_t value to seconds.
#[unsafe(no_mangle)]
pub extern "C" fn PyTime_AsSecondsDouble(t: i64) -> f64 {
    t as f64 / 1e9
}
