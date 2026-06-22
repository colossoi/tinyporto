//! Loads build-time-compiled SPIR-V into wgpu shader modules.
//!
//! Shaders are compiled by `build.rs` (`wyn compile`) and embedded via
//! `include_bytes!` (see the generated `SHADER_MODULES`). The driver consumes
//! SPIR-V — the portable native interchange format — through naga's SPIR-V
//! frontend (cross-backend). Entry points are referenced by source name
//! (SPIR-V `OpEntryPoint` == Wyn entry name, no mangling).

use std::borrow::Cow;

/// Build a wgpu shader module from embedded SPIR-V bytes.
pub fn load_module_bytes(device: &wgpu::Device, label: &str, bytes: &[u8]) -> wgpu::ShaderModule {
    assert!(bytes.len() % 4 == 0, "SPIR-V '{label}' is not a whole number of 32-bit words");
    let words: Vec<u32> = bytes
        .chunks_exact(4)
        .map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect();
    device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some(label),
        source: wgpu::ShaderSource::SpirV(Cow::Owned(words)),
    })
}
