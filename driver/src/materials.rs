//! Decode the CC0 source maps once, isolate brick faces, and upload mipmapped
//! material maps. Color RGB holds palette-relative variation; alpha is roughness.
use anyhow::{ensure, Context, Result};
use std::io::Cursor;
use std::sync::LazyLock;

static LINEAR: LazyLock<[f32; 256]> =
    LazyLock::new(|| std::array::from_fn(|i| linear(i as f32 / 255.0)));

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

struct Image {
    width: usize,
    height: usize,
    pixels: Vec<[u8; 4]>,
}

fn decode(bytes: &[u8]) -> Result<Image> {
    let mut decoder = png::Decoder::new(Cursor::new(bytes));
    decoder.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    let mut reader = decoder.read_info().context("read material PNG")?;
    let mut bytes = vec![0; reader.output_buffer_size()];
    let info = reader
        .next_frame(&mut bytes)
        .context("decode material PNG")?;
    ensure!(
        info.bit_depth == png::BitDepth::Eight,
        "expected decoded 8-bit PNG"
    );
    let channels = info.color_type.samples();
    let pixels = bytes[..info.buffer_size()]
        .chunks_exact(channels)
        .map(|p| match info.color_type {
            png::ColorType::Rgb => [p[0], p[1], p[2], 255],
            png::ColorType::Rgba => [p[0], p[1], p[2], p[3]],
            png::ColorType::Grayscale => [p[0], p[0], p[0], 255],
            png::ColorType::GrayscaleAlpha => [p[0], p[0], p[0], p[1]],
            png::ColorType::Indexed => unreachable!("EXPAND resolves palettes"),
        })
        .collect();
    Ok(Image {
        width: info.width as usize,
        height: info.height as usize,
        pixels,
    })
}

fn linear(s: f32) -> f32 {
    if s <= 0.04045 {
        s / 12.92
    } else {
        ((s + 0.055) / 1.055).powf(2.4)
    }
}

fn srgb(l: f32) -> f32 {
    if l <= 0.0031308 {
        l * 12.92
    } else {
        1.055 * l.powf(1.0 / 2.4) - 0.055
    }
}

impl Image {
    fn sample(&self, u: f32, v: f32, color: bool) -> [f32; 4] {
        let x = (u * self.width as f32 - 0.5).clamp(0.0, (self.width - 1) as f32);
        let y = (v * self.height as f32 - 0.5).clamp(0.0, (self.height - 1) as f32);
        let x0 = x as usize;
        let y0 = y as usize;
        let mut value = [0.0; 4];
        for (px, py, weight) in [
            (x0, y0, (1.0 - x.fract()) * (1.0 - y.fract())),
            (
                (x0 + 1).min(self.width - 1),
                y0,
                x.fract() * (1.0 - y.fract()),
            ),
            (
                x0,
                (y0 + 1).min(self.height - 1),
                (1.0 - x.fract()) * y.fract(),
            ),
            (
                (x0 + 1).min(self.width - 1),
                (y0 + 1).min(self.height - 1),
                x.fract() * y.fract(),
            ),
        ] {
            if weight == 0.0 {
                continue;
            }
            for (c, result) in value.iter_mut().enumerate() {
                let byte = self.pixels[py * self.width + px][c];
                let sample = f32::from(byte) / 255.0;
                *result += weight
                    * if color && c < 3 {
                        LINEAR[byte as usize]
                    } else {
                        sample
                    };
            }
        }
        value
    }
}

type Pixels = Vec<[f32; 4]>;

fn downsample(pixels: &[[f32; 4]], width: usize, height: usize, roughness: bool) -> Pixels {
    let w = (width / 2).max(1);
    let h = (height / 2).max(1);
    (0..w * h)
        .map(|i| {
            let mut result = [0.0; 4];
            for dy in 0..2 {
                for dx in 0..2 {
                    let p = pixels[((i / w * 2 + dy).min(height - 1)) * width
                        + (i % w * 2 + dx).min(width - 1)];
                    for c in 0..4 {
                        result[c] += if roughness && c == 3 {
                            p[c] * p[c] * 0.25
                        } else {
                            p[c] * 0.25
                        };
                    }
                }
            }
            if roughness {
                result[3] = result[3].sqrt();
            }
            result
        })
        .collect()
}

fn upload(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    label: &str,
    mut tiles: Vec<Pixels>,
    mut width: usize,
    mut height: usize,
    columns: usize,
    color: bool,
) -> wgpu::Texture {
    let rows = tiles.len() / columns;
    // Stop before an atlas tile would disappear. Each tile's mips are built
    // independently, so mortar or neighboring brick faces cannot bleed into it.
    let levels = width.min(height).ilog2() + 1;
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some(label),
        size: wgpu::Extent3d {
            width: (width * columns) as u32,
            height: (height * rows) as u32,
            depth_or_array_layers: 1,
        },
        mip_level_count: levels,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: if color {
            wgpu::TextureFormat::Rgba8UnormSrgb
        } else {
            wgpu::TextureFormat::Rgba8Unorm
        },
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    for level in 0..levels {
        let atlas_width = width * columns;
        let atlas_height = height * rows;
        let mut bytes = vec![0; atlas_width * atlas_height * 4];
        for (tile, pixels) in tiles.iter().enumerate() {
            for (i, pixel) in pixels.iter().enumerate() {
                let x = tile % columns * width + i % width;
                let y = tile / columns * height + i / width;
                for c in 0..4 {
                    let value = if color && c < 3 {
                        srgb(pixel[c])
                    } else {
                        pixel[c]
                    };
                    bytes[(y * atlas_width + x) * 4 + c] =
                        (value.clamp(0.0, 1.0) * 255.0).round() as u8;
                }
            }
        }
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &texture,
                mip_level: level,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &bytes,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some((atlas_width * 4) as u32),
                rows_per_image: Some(atlas_height as u32),
            },
            wgpu::Extent3d {
                width: atlas_width as u32,
                height: atlas_height as u32,
                depth_or_array_layers: 1,
            },
        );
        if level + 1 < levels {
            tiles = tiles
                .iter()
                .map(|p| downsample(p, width, height, color))
                .collect();
            width = (width / 2).max(1);
            height = (height / 2).max(1);
        }
    }
    texture
}

fn load(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    name: &str,
    sources: [&[u8]; 3],
    regions: &[[f32; 4]],
    width: usize,
    height: usize,
    columns: usize,
) -> Result<Maps> {
    let color = decode(sources[0])?;
    let normal = decode(sources[1])?;
    let roughness = decode(sources[2])?;
    ensure!(
        (color.width, color.height) == (normal.width, normal.height)
            && (color.width, color.height) == (roughness.width, roughness.height),
        "{name}: material map dimensions differ"
    );
    let mut colors = Vec::new();
    let mut normals = Vec::new();
    let mut average = [0.0f64; 3];
    for &[x, y, w, h] in regions {
        let mut c = Vec::with_capacity(width * height);
        let mut n = Vec::with_capacity(width * height);
        for i in 0..width * height {
            let u = x + (i % width) as f32 * w / width as f32 + w / (2 * width) as f32;
            let v = y + (i / width) as f32 * h / height as f32 + h / (2 * height) as f32;
            let mut value = color.sample(u, v, true);
            for channel in 0..3 {
                average[channel] += f64::from(value[channel]);
            }
            value[3] = roughness.sample(u, v, false)[0];
            c.push(value);
            n.push(normal.sample(u, v, false));
        }
        colors.push(c);
        normals.push(n);
    }
    // Preserve the existing per-piece palette and the coarse GI proxy colors.
    // The source contributes relative RGB weathering, not a second lighting bake.
    let count = (width * height * regions.len()) as f64;
    for tile in &mut colors {
        for pixel in tile {
            for channel in 0..3 {
                pixel[channel] =
                    (pixel[channel] * 0.5 / (average[channel] / count).max(0.001) as f32).min(1.0);
            }
        }
    }
    Ok(Maps {
        color: upload(
            device,
            queue,
            &format!("{name} color / roughness"),
            colors,
            width,
            height,
            columns,
            true,
        ),
        normal: upload(
            device,
            queue,
            &format!("{name} normal"),
            normals,
            width,
            height,
            columns,
            false,
        ),
    })
}

impl Materials {
    pub fn new(device: &wgpu::Device, queue: &wgpu::Queue) -> Result<Self> {
        // Face interiors selected from the 512x512 source preview, then converted
        // to normalized coordinates. All three maps use the identical regions.
        let brick_regions = [
            [28.0, 8.0, 60.0, 11.0],
            [112.0, 8.0, 65.0, 11.0],
            [202.0, 8.0, 58.0, 11.0],
            [286.0, 8.0, 52.0, 11.0],
            [60.0, 34.0, 58.0, 10.0],
            [150.0, 34.0, 57.0, 10.0],
            [237.0, 34.0, 64.0, 10.0],
            [322.0, 34.0, 60.0, 10.0],
        ]
        .map(|r| r.map(|v| v / 512.0));
        let whole = [[0.0, 0.0, 1.0, 1.0]];
        Ok(Self {
            brick: load(
                device,
                queue,
                "brick",
                [
                    include_bytes!("../../assets/textures/red_brick/red_brick_diff_2k.png"),
                    include_bytes!("../../assets/textures/red_brick/red_brick_nor_gl_2k.png"),
                    include_bytes!("../../assets/textures/red_brick/red_brick_rough_2k.png"),
                ],
                &brick_regions,
                256,
                64,
                4,
            )?,
            stone: load(
                device,
                queue,
                "stone",
                [
                    include_bytes!("../../assets/textures/rock_surface/rock_surface_diff_1k.png"),
                    include_bytes!("../../assets/textures/rock_surface/rock_surface_nor_gl_1k.png"),
                    include_bytes!("../../assets/textures/rock_surface/rock_surface_rough_1k.png"),
                ],
                &whole,
                1024,
                1024,
                1,
            )?,
            mortar: load(
                device,
                queue,
                "mortar",
                [
                    include_bytes!(
                        "../../assets/textures/rough_concrete/rough_concrete_diff_1k.png"
                    ),
                    include_bytes!(
                        "../../assets/textures/rough_concrete/rough_concrete_nor_gl_1k.png"
                    ),
                    include_bytes!(
                        "../../assets/textures/rough_concrete/rough_concrete_rough_1k.png"
                    ),
                ],
                &whole,
                1024,
                1024,
                1,
            )?,
            sampler: device.create_sampler(&wgpu::SamplerDescriptor {
                label: Some("material trilinear repeat"),
                address_mode_u: wgpu::AddressMode::Repeat,
                address_mode_v: wgpu::AddressMode::Repeat,
                mag_filter: wgpu::FilterMode::Linear,
                min_filter: wgpu::FilterMode::Linear,
                mipmap_filter: wgpu::FilterMode::Linear,
                ..Default::default()
            }),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn color_mips_average_light_and_preserve_linear_roughness() {
        let pixels = [
            [linear(0.0), 0.0, 0.0, 0.2],
            [linear(1.0), 0.0, 0.0, 0.8],
            [linear(0.0), 0.0, 0.0, 0.2],
            [linear(1.0), 0.0, 0.0, 0.8],
        ];
        let mip = downsample(&pixels, 2, 2, true);
        assert!((srgb(mip[0][0]) - 0.73536).abs() < 0.0001);
        assert!((mip[0][3] - 0.34f32.sqrt()).abs() < 0.0001);
    }
}
