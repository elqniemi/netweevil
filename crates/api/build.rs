//! Embeds the built web console (`frontend/dist`) into the API binary when it
//! exists at compile time, so a single executable serves the console at `/`.
//! Without a built console the binary still compiles and serves the API only.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};

fn main() {
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let dist = manifest_dir
        .join("..")
        .join("..")
        .join("frontend")
        .join("dist");
    let out = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR")).join("console_assets.rs");
    println!("cargo:rerun-if-changed={}", dist.display());
    println!("cargo:rerun-if-changed=build.rs");

    let mut entries = Vec::new();
    if dist.join("index.html").is_file() {
        collect(&dist, &dist, &mut entries);
    }
    let mut source = String::from("pub(crate) static CONSOLE_ASSETS: &[(&str, &[u8])] = &[\n");
    for (relative, absolute) in &entries {
        println!("cargo:rerun-if-changed={}", absolute.display());
        source.push_str(&format!(
            "    ({:?}, include_bytes!({:?})),\n",
            relative,
            absolute.display().to_string()
        ));
    }
    source.push_str("];\n");
    fs::write(&out, source).expect("writing console asset table");
}

fn collect(root: &Path, dir: &Path, entries: &mut Vec<(String, PathBuf)>) {
    let Ok(read) = fs::read_dir(dir) else {
        return;
    };
    let mut paths = read
        .filter_map(Result::ok)
        .map(|e| e.path())
        .collect::<Vec<_>>();
    paths.sort();
    for path in paths {
        if path.is_dir() {
            collect(root, &path, entries);
        } else if let Ok(relative) = path.strip_prefix(root) {
            let relative = relative
                .components()
                .map(|c| c.as_os_str().to_string_lossy().to_string())
                .collect::<Vec<_>>()
                .join("/");
            entries.push((relative, path.clone()));
        }
    }
}
