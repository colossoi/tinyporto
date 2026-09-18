//! GPU pass durations, read back asynchronously without stalling rendering.

use std::sync::mpsc::{self, Receiver, TryRecvError};

struct Readback {
    buffer: wgpu::Buffer,
    pending: Option<Receiver<Result<(), wgpu::BufferAsyncError>>>,
}

pub struct GpuTimer {
    queries: wgpu::QuerySet,
    query_count: u32,
    resolve: wgpu::Buffer,
    readbacks: [Readback; 3],
    timestamp_period: f32,
    total_ms: f64,
    samples: u32,
}

impl GpuTimer {
    pub fn new(device: &wgpu::Device, queue: &wgpu::Queue, pass_count: usize) -> Option<Self> {
        if pass_count == 0 || !device.features().contains(wgpu::Features::TIMESTAMP_QUERY) {
            return None;
        }
        let count = u32::try_from(pass_count.checked_mul(2)?).ok()?;
        let size = u64::from(count) * 8;
        Some(Self {
            queries: device.create_query_set(&wgpu::QuerySetDescriptor {
                label: Some("frame GPU timestamps"),
                ty: wgpu::QueryType::Timestamp,
                count,
            }),
            query_count: count,
            resolve: device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("resolved GPU timestamps"),
                size,
                usage: wgpu::BufferUsages::QUERY_RESOLVE | wgpu::BufferUsages::COPY_SRC,
                mapped_at_creation: false,
            }),
            readbacks: std::array::from_fn(|_| Readback {
                buffer: device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("GPU timestamp readback"),
                    size,
                    usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                    mapped_at_creation: false,
                }),
                pending: None,
            }),
            timestamp_period: queue.get_timestamp_period(),
            total_ms: 0.0,
            samples: 0,
        })
    }

    pub fn queries(&self) -> &wgpu::QuerySet {
        &self.queries
    }

    /// Skip a sample if all readbacks are still in flight; never wait for one.
    pub fn available_slot(&self) -> Option<usize> {
        self.readbacks
            .iter()
            .position(|slot| slot.pending.is_none())
    }

    pub fn resolve(&self, encoder: &mut wgpu::CommandEncoder, slot: usize) {
        encoder.resolve_query_set(&self.queries, 0..self.query_count, &self.resolve, 0);
        encoder.copy_buffer_to_buffer(
            &self.resolve,
            0,
            &self.readbacks[slot].buffer,
            0,
            self.resolve.size(),
        );
    }

    /// Called after submitting the commands that fill this readback buffer.
    pub fn request_readback(&mut self, slot: usize) {
        let slot = &mut self.readbacks[slot];
        debug_assert!(slot.pending.is_none());
        let (sender, receiver) = mpsc::channel();
        slot.buffer
            .slice(..)
            .map_async(wgpu::MapMode::Read, move |result| {
                let _ = sender.send(result);
            });
        slot.pending = Some(receiver);
    }

    pub fn collect(&mut self, device: &wgpu::Device) {
        let _ = device.poll(wgpu::PollType::Poll);
        for slot in &mut self.readbacks {
            let Some(receiver) = &slot.pending else {
                continue;
            };
            match receiver.try_recv() {
                Err(TryRecvError::Empty) => continue,
                Ok(Ok(())) => {
                    let bytes = slot.buffer.slice(..).get_mapped_range();
                    if let Some(ms) = pass_total_ms(&bytes, self.timestamp_period) {
                        self.total_ms += ms;
                        self.samples += 1;
                    }
                }
                Ok(Err(error)) => eprintln!("GPU timing readback: {error}"),
                Err(TryRecvError::Disconnected) => {}
            }
            slot.buffer.unmap();
            slot.pending = None;
        }
    }

    /// Mean of completed GPU samples since the previous title update.
    pub fn take_average_ms(&mut self) -> Option<f64> {
        let average = (self.samples > 0).then(|| self.total_ms / f64::from(self.samples));
        self.total_ms = 0.0;
        self.samples = 0;
        average
    }
}

/// Sum pass durations, excluding gaps between passes. Subtract integer ticks
/// before floating-point conversion so large absolute timestamps retain precision.
fn pass_total_ms(bytes: &[u8], period_ns: f32) -> Option<f64> {
    let mut ticks = 0u64;
    for pair in bytes.chunks_exact(16) {
        let start = u64::from_le_bytes(pair[..8].try_into().unwrap());
        let end = u64::from_le_bytes(pair[8..].try_into().unwrap());
        // Discard a sample if the GPU timestamp counter reset/wrapped.
        ticks = ticks.checked_add(end.checked_sub(start)?)?;
    }
    Some(ticks as f64 * f64::from(period_ns) / 1_000_000.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bytes(timestamps: &[u64]) -> Vec<u8> {
        timestamps.iter().flat_map(|t| t.to_le_bytes()).collect()
    }

    #[test]
    fn sums_gpu_passes_without_idle_gaps() {
        let base = 1u64 << 60;
        let timestamps = bytes(&[base, base + 1_000_000, base + 10_000_000, base + 12_000_000]);
        assert_eq!(pass_total_ms(&timestamps, 2.0), Some(6.0));
        assert_eq!(
            pass_total_ms(&bytes(&[base, base + 1]), 1.0),
            Some(0.000001)
        );
    }

    #[test]
    fn discards_reversed_timestamps() {
        assert_eq!(pass_total_ms(&bytes(&[100, 90]), 1.0), None);
    }

    #[test]
    fn gpu_readbacks_can_fill_and_reuse_all_slots() {
        let gfx = crate::gfx::Gfx::new_headless(16, 16).expect("GPU device");
        let Some(mut timer) = GpuTimer::new(&gfx.device, &gfx.queue, 2) else {
            eprintln!("adapter does not support timestamp queries; skipping GPU timing test");
            return;
        };
        let texture = gfx.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("timing test target"),
            size: wgpu::Extent3d {
                width: 16,
                height: 16,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: gfx.config.format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        });
        let view = texture.create_view(&Default::default());
        for _ in 0..2 {
            for _ in 0..3 {
                let slot = timer.available_slot().expect("free readback");
                let mut encoder = gfx.device.create_command_encoder(&Default::default());
                {
                    let _pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                        timestamp_writes: Some(wgpu::ComputePassTimestampWrites {
                            query_set: timer.queries(),
                            beginning_of_pass_write_index: Some(0),
                            end_of_pass_write_index: Some(1),
                        }),
                        ..Default::default()
                    });
                }
                {
                    let _pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                            view: &view,
                            depth_slice: None,
                            resolve_target: None,
                            ops: wgpu::Operations {
                                load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                                store: wgpu::StoreOp::Store,
                            },
                        })],
                        timestamp_writes: Some(wgpu::RenderPassTimestampWrites {
                            query_set: timer.queries(),
                            beginning_of_pass_write_index: Some(2),
                            end_of_pass_write_index: Some(3),
                        }),
                        ..Default::default()
                    });
                }
                timer.resolve(&mut encoder, slot);
                gfx.queue.submit([encoder.finish()]);
                timer.request_readback(slot);
            }
            assert!(timer.available_slot().is_none());
            gfx.device
                .poll(wgpu::PollType::Wait {
                    submission_index: None,
                    timeout: Some(std::time::Duration::from_secs(10)),
                })
                .unwrap();
            timer.collect(&gfx.device);
            assert_eq!(timer.samples, 3);
            let ms = timer
                .take_average_ms()
                .expect("completed timestamp samples");
            assert!(ms.is_finite() && ms >= 0.0);
            assert!(timer.take_average_ms().is_none());
            assert!(timer.readbacks.iter().all(|slot| slot.pending.is_none()));
        }
    }
}
