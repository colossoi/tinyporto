//! Final-image history lifetime. GI owns its separate history in app.rs.
use crate::camera::Camera;

#[derive(Clone, Copy, PartialEq)]
pub struct HistoryKey {
    pub enabled: bool,
    pub gi_mode: u32,
    pub textures: bool,
    pub mods: u32,
    pub sky: [f32; 3],
}

#[derive(Clone, Copy, Default)]
pub struct TemporalFrame {
    pub jitter: [f32; 2],
    pub previous_jitter: [f32; 2],
    pub enabled: u32,
    pub valid: u32,
}

pub struct TemporalState {
    images: [wgpu::Texture; 2],
    read: usize,
    sequence: u32,
    previous: Option<(Camera, HistoryKey, [f32; 2])>,
}

fn halton(mut index: u32, base: u32) -> f32 {
    let (mut sum, mut scale) = (0.0, 1.0);
    while index != 0 {
        scale /= base as f32;
        sum += (index % base) as f32 * scale;
        index /= base;
    }
    sum
}

fn camera_cut(old: &Camera, new: &Camera) -> bool {
    let distance = old
        .target
        .iter()
        .zip(new.target)
        .map(|(a, b)| (a - b).powi(2))
        .sum::<f32>()
        .sqrt();
    let angle = (new.az - old.az).sin().atan2((new.az - old.az).cos()).abs();
    distance > new.dist * 0.1
        || angle > 0.25
        || (new.elev - old.elev).abs() > 0.25
        || (new.dist / old.dist).ln().abs() > 0.2
}

impl TemporalState {
    pub fn new(device: &wgpu::Device, width: u32, height: u32) -> Self {
        let image = || {
            device.create_texture(&wgpu::TextureDescriptor {
                label: Some("TAA HDR/depth history"),
                size: wgpu::Extent3d {
                    width,
                    height,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba32Float,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                    | wgpu::TextureUsages::TEXTURE_BINDING
                    | wgpu::TextureUsages::COPY_SRC,
                view_formats: &[],
            })
        };
        Self {
            images: [image(), image()],
            read: 0,
            sequence: 0,
            previous: None,
        }
    }

    pub fn input(&self) -> &wgpu::Texture {
        &self.images[self.read]
    }
    pub fn output(&self) -> &wgpu::Texture {
        &self.images[1 - self.read]
    }

    // Pure preparation: pipeline warmup/discard must not advance history.
    pub fn frame(&self, camera: &Camera, key: HistoryKey, reset: bool) -> TemporalFrame {
        let index = self.sequence % 16 + 1;
        let jitter = if key.enabled {
            [
                2.0 * (halton(index, 2) - 0.5) / self.input().width() as f32,
                -2.0 * (halton(index, 3) - 0.5) / self.input().height() as f32,
            ]
        } else {
            [0.0; 2]
        };
        TemporalFrame {
            jitter,
            previous_jitter: self.previous.map_or([0.0; 2], |(_, _, j)| j),
            enabled: u32::from(key.enabled),
            valid: u32::from(
                key.enabled
                    && !reset
                    && self.previous.is_some_and(|(old, old_key, _)| {
                        old_key == key && !camera_cut(&old, camera)
                    }),
            ),
        }
    }

    // Only after the command buffer was submitted successfully.
    pub fn commit(&mut self, camera: Camera, key: HistoryKey) {
        let frame = self.frame(&camera, key, false);
        self.previous = Some((camera, key, frame.jitter));
        self.read = 1 - self.read;
        self.sequence = self.sequence.wrapping_add(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jitter_is_subpixel_and_camera_cut_ignores_angle_wrap() {
        for index in 1..=16 {
            for base in [2, 3] {
                assert!((0.0..1.0).contains(&halton(index, base)));
            }
        }
        let old = Camera::default();
        let mut next = old;
        next.az += std::f32::consts::TAU;
        assert!(!camera_cut(&old, &next));
        next.az += 0.4;
        assert!(camera_cut(&old, &next));
        next = old;
        next.target[0] += old.dist;
        assert!(camera_cut(&old, &next));
    }
}
