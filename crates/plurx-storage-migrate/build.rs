fn object(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .filter(|value| value.len() == 40 && value.bytes().all(|byte| byte.is_ascii_hexdigit()))
}

fn main() {
    for name in ["PLURX_BUILD_SHA", "PLURX_BUILD_TREE", "PLURX_BUILD_REF"] {
        println!("cargo:rerun-if-env-changed={name}");
    }
    let commit = object("PLURX_BUILD_SHA").unwrap_or_else(|| "unavailable".into());
    let tree = object("PLURX_BUILD_TREE")
        .or_else(|| object("PLURX_BUILD_REF"))
        .unwrap_or_else(|| "unavailable".into());
    println!("cargo:rustc-env=PLURX_HELPER_COMMIT={commit}");
    println!("cargo:rustc-env=PLURX_HELPER_TREE={tree}");
    println!(
        "cargo:rustc-env=PLURX_HELPER_TARGET={}",
        std::env::var("TARGET").expect("Cargo target")
    );
}
