use std::env;

fn main() {
    println!("cargo:rerun-if-env-changed=PLURX_BUILD_SHA");
    if let Ok(build_sha) = env::var("PLURX_BUILD_SHA") {
        let normalized = build_sha.trim().to_ascii_lowercase();
        assert!(
            normalized.len() == 40 && normalized.bytes().all(|byte| byte.is_ascii_hexdigit()),
            "PLURX_BUILD_SHA must be a full 40-character Git SHA"
        );
        println!("cargo:rustc-env=PLURX_BUILD_SHA={normalized}");
    }
    let profile = env::var("PROFILE").expect("Cargo must identify the active profile");
    let opt_level = env::var("OPT_LEVEL").expect("Cargo must identify the optimization level");
    let debug = env::var("DEBUG").expect("Cargo must identify the debug-info setting");
    println!(
        "cargo:rustc-env=PLURX_BUILD_PROFILE=profile={profile};opt-level={opt_level};debug={debug}"
    );
}
