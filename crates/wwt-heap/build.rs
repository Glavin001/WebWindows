//! Link settings for the WebAssembly build: the module works directly in the
//! machine's memory (imported), and its own stack and data must fit in the
//! guest's null region, which guest code never touches (see `mem.rs`).

fn main() {
    if std::env::var("CARGO_CFG_TARGET_ARCH").as_deref() == Ok("wasm32") {
        for arg in [
            "--import-memory",
            "--stack-first",
            "-zstack-size=16384",
            "--export=__data_end",
        ] {
            println!("cargo:rustc-link-arg-cdylib={arg}");
        }
    }
}
