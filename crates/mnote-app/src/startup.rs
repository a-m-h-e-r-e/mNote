//! How long mNote takes to start: from the process starting to its first
//! frame, printed with `MNOTE_LATENCY` (the cold-start budget is 200 ms).

use std::sync::OnceLock;
use std::time::Instant;

static STARTED: OnceLock<Instant> = OnceLock::new();
static SHOWN: OnceLock<()> = OnceLock::new();

/// Called first thing in `main`.
pub fn mark() {
    let _ = STARTED.set(Instant::now());
}

/// Called each frame; the first prints `mnote start: N ms` (with
/// `MNOTE_LATENCY`).
pub fn first_frame() {
    if SHOWN.set(()).is_err() {
        return;
    }
    if let Some(started) = STARTED.get()
        && std::env::var_os("MNOTE_LATENCY").is_some()
    {
        eprintln!(
            "mnote start: {:.1} ms",
            started.elapsed().as_secs_f32() * 1000.
        );
    }
}
