fn main() {
    // Only needed on Windows — Servo forces ANGLE (libEGL/libGLESv2) which must
    // be present next to the executable at runtime.
    #[cfg(target_os = "windows")]
    copy_angle_dlls_to_binary_dir();
}

#[cfg(target_os = "windows")]
fn copy_angle_dlls_to_binary_dir() {
    use std::path::Path;

    // OUT_DIR = {target}/{profile}/build/{crate}-{hash}/out
    // Binary  = {target}/{profile}/
    let out_dir = std::env::var("OUT_DIR").unwrap();
    let binary_dir = Path::new(&out_dir)
        .parent().unwrap()  // strip "out"
        .parent().unwrap()  // strip "{crate}-{hash}"
        .parent().unwrap(); // strip "build"

    let build_dir = binary_dir.join("build");

    for dll in &["libEGL.dll", "libGLESv2.dll"] {
        let dest = binary_dir.join(dll);
        if dest.exists() {
            continue;
        }
        if let Ok(entries) = std::fs::read_dir(&build_dir) {
            for entry in entries.flatten() {
                let name = entry.file_name();
                let name = name.to_string_lossy();
                if !name.starts_with("mozangle-") {
                    continue;
                }
                let src = entry.path().join("out").join(dll);
                if src.exists() {
                    if let Err(e) = std::fs::copy(&src, &dest) {
                        println!("cargo:warning=Failed to copy {dll} to binary dir: {e}");
                    }
                    break;
                }
            }
        }
    }

    // Re-run if mozangle's output changes
    println!("cargo:rerun-if-changed=build.rs");
}
