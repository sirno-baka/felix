use std::time::{SystemTime, UNIX_EPOCH};

fn main() {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("build host time is before the Unix epoch")
        .as_secs();
    println!("cargo:rustc-env=TWITCH_RADIO_BUILD_UNIX={seconds}");
    println!("cargo:rerun-if-changed=build.rs");
}
