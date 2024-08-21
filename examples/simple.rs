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
    env_logger::init();
    page_primer::prime().mlock(true).remap(true).run();
    bar();
}
