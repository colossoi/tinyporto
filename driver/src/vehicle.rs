//! Embedded static mesh prepared from the attributed, simplified Fiat GLB.
use anyhow::{ensure, Result};
use wgpu::util::DeviceExt;

const VERTICES: &[u8] = include_bytes!("../../assets/vehicles/fiat-500/scene/vertices.bin");
const INDICES: &[u8] = include_bytes!("../../assets/vehicles/fiat-500/scene/indices.bin");

pub struct Vehicle {
    pub vertices: wgpu::Buffer,
    pub indices: wgpu::Buffer,
    pub vent: wgpu::Texture,
    pub engine: wgpu::Texture,
    pub sampler: wgpu::Sampler,
}

impl Vehicle {
    pub fn new(device: &wgpu::Device, queue: &wgpu::Queue) -> Result<Self> {
        ensure!(
            !VERTICES.is_empty() && VERTICES.len() % 64 == 0,
            "invalid Fiat vertex buffer"
        );
        ensure!(
            !INDICES.is_empty() && INDICES.len() % 12 == 0,
            "invalid Fiat triangle buffer"
        );
        for index in INDICES.chunks_exact(4) {
            ensure!(
                (u32::from_le_bytes(index.try_into().unwrap()) as usize) < VERTICES.len() / 64,
                "Fiat index outside vertex buffer"
            );
        }
        let buffer = |label, contents| {
            device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some(label),
                contents,
                usage: wgpu::BufferUsages::STORAGE,
            })
        };
        Ok(Self {
            vertices: buffer("Fiat vertices", VERTICES),
            indices: buffer("Fiat indices", INDICES),
            vent: crate::materials::upload(
                device,
                queue,
                "Fiat rear vents",
                &crate::materials::prepared::FIAT_VENT,
            ),
            engine: crate::materials::upload(
                device,
                queue,
                "Fiat engine cover",
                &crate::materials::prepared::FIAT_ENGINE,
            ),
            sampler: device.create_sampler(&wgpu::SamplerDescriptor {
                label: Some("Fiat textures"),
                mag_filter: wgpu::FilterMode::Linear,
                min_filter: wgpu::FilterMode::Linear,
                mipmap_filter: wgpu::FilterMode::Linear,
                ..Default::default()
            }),
        })
    }
}
