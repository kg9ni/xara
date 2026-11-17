use std::env;

fn main() {
    println!("cargo::rerun-if-changed=build.rs");
    for c in ["pulse", "alsa", "coreaudio", "hidraw", "wasapi"] {
        println!("cargo::rustc-check-cfg=cfg({c})");
    }
    let os = env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    for (feature, cfg) in [("CARGO_FEATURE_PULSE", "pulse"), ("CARGO_FEATURE_ALSA", "alsa")] {
        if os != "macos" && os != "windows" && env::var_os(feature).is_some() {
            println!("cargo::rustc-cfg={cfg}");
        }
    }
    if os == "macos" {
        println!("cargo::rustc-cfg=coreaudio");
    }
    if os == "windows" {
        println!("cargo::rustc-cfg=wasapi");
    }
    if os == "linux" || os == "freebsd" {
        println!("cargo::rustc-cfg=hidraw");
    }
}
