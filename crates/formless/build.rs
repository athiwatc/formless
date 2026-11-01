// Keep the executable on the original main thread (TUI/clipboard affinity).
// In the measured unoptimized binary, Clap's finite command-tree constructor
// alone reserves ~619 KiB; building its nested commands needs ~928 KiB overall
// on Linux before any command runs. Windows' small default main-thread reserve
// leaves too little headroom for this plus ordinary verification/schema work.
// Reserve virtual address space for the PRODUCTION executable, not a test-only
// thread or RUST_MIN_STACK override. Windows commits stack pages on demand.
fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        match std::env::var("CARGO_CFG_TARGET_ENV").as_deref() {
            Ok("msvc") => println!("cargo:rustc-link-arg-bin=formless=/STACK:8388608"),
            Ok("gnu" | "gnullvm") => {
                println!("cargo:rustc-link-arg-bin=formless=-Wl,--stack,8388608")
            }
            _ => (),
        }
    }
}
