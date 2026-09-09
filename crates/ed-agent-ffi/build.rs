fn main() {
    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    if matches!(target_os.as_str(), "ios" | "tvos" | "visionos" | "watchos") {
        cc::Build::new()
            .file("scripts/tvos_chkstk.s")
            .compile("tvos_chkstk");
        println!("cargo:rerun-if-changed=scripts/tvos_chkstk.s");
    }
}
