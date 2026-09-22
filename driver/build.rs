//! Compile and include Wyn's Rust/WGPU host module and its sibling SPIR-V.

use std::path::{Path, PathBuf};
use std::process::Command;

const PACKAGES: &[&str] = &["curves", "gfx", "gtao", "noise", "packing", "rng"];

fn which_wyn() -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    let extensions: Vec<String> = if cfg!(windows) {
        std::env::var("PATHEXT")
            .unwrap_or_else(|_| ".EXE".into())
            .split(';')
            .map(str::to_ascii_lowercase)
            .collect()
    } else {
        vec![String::new()]
    };
    std::env::split_paths(&path)
        .flat_map(|dir| {
            extensions
                .iter()
                .map(move |ext| dir.join(format!("wyn{ext}")))
        })
        .find(|path| path.is_file())
}

fn track_sources(dir: &Path) {
    println!("cargo:rerun-if-changed={}", dir.display());
    for entry in std::fs::read_dir(dir).unwrap_or_else(|e| panic!("read {}: {e}", dir.display())) {
        let path = entry.expect("read source directory").path();
        if path.is_dir() {
            track_sources(&path);
        } else if path.extension().is_some_and(|ext| ext == "wyn") {
            println!("cargo:rerun-if-changed={}", path.display());
        }
    }
}

fn main() {
    let manifest = PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").unwrap());
    let repo = manifest.parent().expect("driver has a parent");
    let out = PathBuf::from(std::env::var_os("OUT_DIR").unwrap());
    println!("cargo:rerun-if-changed=build.rs");
    for name in ["WYN", "WYN_PRECOMPILED_DIR", "PATH", "PATHEXT"] {
        println!("cargo:rerun-if-env-changed={name}");
    }

    if let Some(directory) = std::env::var_os("WYN_PRECOMPILED_DIR") {
        let directory = PathBuf::from(directory);
        for extension in ["rs", "spv"] {
            let source = directory.join(format!("main.{extension}"));
            println!("cargo:rerun-if-changed={}", source.display());
            std::fs::copy(&source, out.join(format!("main.{extension}")))
                .unwrap_or_else(|e| panic!("copy {}: {e}", source.display()));
        }
    } else {
        println!("cargo:rerun-if-changed={}", repo.join("wyn.toml").display());
        track_sources(&repo.join("wyn"));
        for package in PACKAGES {
            let path = repo.join("../wyn/pkg").join(package);
            println!("cargo:rerun-if-changed={}", path.join("wyn.toml").display());
            track_sources(&path.join("src"));
        }
        let wyn = std::env::var_os("WYN")
            .map(PathBuf::from)
            .or_else(which_wyn)
            .unwrap_or_else(|| PathBuf::from("wyn"));
        println!("cargo:rerun-if-changed={}", wyn.display());
        let status = Command::new(&wyn)
            .current_dir(repo)
            .args([
                "build",
                "--graphics",
                "-O",
                "--target-double",
                "rust-wgpu",
                "--target",
                "spirv",
            ])
            .arg(repo.join("wyn/main.wyn"))
            .arg("-o")
            .arg(out.join("main.spv"))
            .status()
            .unwrap_or_else(|e| panic!("run {}: {e}; install Wyn or set WYN", wyn.display()));
        assert!(status.success(), "Wyn Rust/WGPU compilation failed");
    }

    // A path module accepts Wyn's inner doc attributes and resolves its
    // include_bytes!("main.spv") next to the generated Rust file.
    let module = format!(
        "#[allow(dead_code, non_snake_case, unused_imports, unused_variables)]\n#[path = {:?}]\nmod generated;\n",
        out.join("main.rs").to_str().expect("UTF-8 output path")
    );
    std::fs::write(out.join("module.rs"), module).expect("write module declaration");
}
