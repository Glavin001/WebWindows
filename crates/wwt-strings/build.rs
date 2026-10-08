//! Link settings for the WebAssembly build: the module works directly in the
//! machine's memory (imported). Its data (from 0x9000) and then its stack go
//! in the guest's null region, which guest code never touches, above what the
//! native heap uses there (`crates/wwt-heap`: its stack and data below
//! 0x8000, its state at 0x8000) and this module's state (0x8100);
//! `runtime/wine/strings.mjs` checks that they end below 0x10000.

fn main() {
    if std::env::var("CARGO_CFG_TARGET_ARCH").as_deref() == Ok("wasm32") {
        for arg in [
            "--import-memory",
            "--no-stack-first",
            "--global-base=36864",
            "-zstack-size=8192",
            "--export=__data_end",
            "--export=__heap_base",
        ] {
            println!("cargo:rustc-link-arg-cdylib={arg}");
        }
    }
}
