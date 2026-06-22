//! "wync" — the Wyn compile + load shim.
//!
//! Invokes `wyn compile` to turn the Wyn program into SPIR-V (+ descriptor), and
//! loads the resulting `.spv` into a wgpu shader module. The driver consumes
//! SPIR-V (the portable native interchange format); wgpu routes it through naga's
//! SPIR-V frontend for cross-backend output. Entry points are referenced by their
//! source names (verified: SPIR-V `OpEntryPoint` == Wyn entry name, no mangling).

use std::path::Path;
use std::process::Command;
use anyhow::{bail, Context, Result};

/// Compile a Wyn root file to `<out>.spv` (+ `<out>.json` descriptor). The
/// descriptor is for build-time validation; the driver loads only the `.spv`.
pub fn compile(root: &Path, out_spv: &Path) -> Result<()> {
    let status = Command::new("wyn")
        .arg("compile")
        .arg("--single-stage")
        .arg(root)
        .arg("-o")
        .arg(out_spv)
        .status()
        .with_context(|| format!("failed to run `wyn compile` on {}", root.display()))?;
    if !status.success() {
        bail!("`wyn compile {}` failed ({status})", root.display());
    }
    Ok(())
}

/// Load a `.spv` file into a wgpu shader module.
pub fn load_module(device: &wgpu::Device, label: &str, spv_path: &Path) -> Result<wgpu::ShaderModule> {
    let bytes = std::fs::read(spv_path)
        .with_context(|| format!("reading SPIR-V {}", spv_path.display()))?;
    if bytes.len() % 4 != 0 {
        bail!("{} is not a whole number of 32-bit words", spv_path.display());
    }
    // SPIR-V is little-endian 32-bit words. Decode to u32 and hand naga the
    // module via ShaderSource::SpirV (validated cross-backend, not passthrough).
    let words: Vec<u32> = bytes
        .chunks_exact(4)
        .map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect();
    Ok(device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some(label),
        source: wgpu::ShaderSource::SpirV(std::borrow::Cow::Owned(words)),
    }))
}
