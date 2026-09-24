//! Upload material maps and mipmaps prepared by the build script.

pub struct Maps {
    pub color: wgpu::Texture,
    pub normal: wgpu::Texture,
}

pub struct Materials {
    pub brick: Maps,
    pub stone: Maps,
    pub mortar: Maps,
    pub sampler: wgpu::Sampler,
}

/// Metadata and concatenated RGBA8 mip bytes generated together at build time.
pub(crate) struct TextureData {
    width: u32,
    height: u32,
    srgb: bool,
    mip_lengths: &'static [usize],
    bytes: &'static [u8],
}

pub(crate) mod prepared {
    include!(concat!(env!("OUT_DIR"), "/material_textures.rs"));
}

pub(crate) fn upload(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    label: &str,
    data: &TextureData,
) -> wgpu::Texture {
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some(label),
        size: wgpu::Extent3d {
            width: data.width,
            height: data.height,
            depth_or_array_layers: 1,
        },
        mip_level_count: data.mip_lengths.len() as u32,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: if data.srgb {
            wgpu::TextureFormat::Rgba8UnormSrgb
        } else {
            wgpu::TextureFormat::Rgba8Unorm
        },
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    let mut offset = 0;
    for (level, &length) in data.mip_lengths.iter().enumerate() {
        let width = (data.width >> level).max(1);
        let height = (data.height >> level).max(1);
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &texture,
                mip_level: level as u32,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &data.bytes[offset..offset + length],
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(width * 4),
                rows_per_image: Some(height),
            },
            wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
        );
        offset += length;
    }
    texture
}

impl Materials {
    pub fn new(device: &wgpu::Device, queue: &wgpu::Queue) -> Self {
        use prepared::*;
        let maps = |name: &str, color: &TextureData, normal: &TextureData| Maps {
            color: upload(device, queue, &format!("{name} color / roughness"), color),
            normal: upload(device, queue, &format!("{name} normal"), normal),
        };
        Self {
            brick: maps("brick", &BRICK_COLOR, &BRICK_NORMAL),
            stone: maps("stone", &STONE_COLOR, &STONE_NORMAL),
            mortar: maps("mortar", &MORTAR_COLOR, &MORTAR_NORMAL),
            sampler: device.create_sampler(&wgpu::SamplerDescriptor {
                label: Some("material trilinear repeat"),
                address_mode_u: wgpu::AddressMode::Repeat,
                address_mode_v: wgpu::AddressMode::Repeat,
                mag_filter: wgpu::FilterMode::Linear,
                min_filter: wgpu::FilterMode::Linear,
                mipmap_filter: wgpu::FilterMode::Linear,
                ..Default::default()
            }),
        }
    }
}

// Exercise the build-time color/roughness and atlas filtering with cargo test.
#[cfg(test)]
#[allow(dead_code)]
#[path = "../build/materials.rs"]
mod preparation;
