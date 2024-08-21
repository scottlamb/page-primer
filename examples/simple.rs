//! A simple example that does a remap then sleeps a while so it can be examined externally while
//! running, including its `/proc/<PID>/smaps` and trying to capture stack traces with debugging
//! tools.
//!
//! Run with the `RUST_LOG=page_primer=trace,simple=trace` environment variable set to see debugging
//! info.

#[inline(never)]
fn foo() {
    log::info!("about to sleep");
    std::thread::sleep(std::time::Duration::from_secs(60));
    log::info!("done sleeping")
}

#[inline(never)]
fn bar() {
    foo()
}

fn main() {
    // Typically this runs right at the start of `main`. Save the result for printing later.
    let prime_out = page_primer::prime().mlock(true).remap(true).run();

    env_logger::init();

    // Now logging is available, so use it.
    prime_out.log();

    bar();
}
