//! Tidewater's OceanFFT spectrum initialization, adapted to its two shortest bands.
//! Upstream 4811ba48d795197de5621985f404e765c0b7c0ef, OceanFFT.js lines 195-344.
//! Copyright (c) 2026 DRG Software Solutions LLC; see licenses/tidewater-MIT.txt.
//! Only initial conditions are CPU-owned. Wyn evolves and transforms them each frame.

use std::f64::consts::{PI, TAU};
pub const SIZE: usize = 256;
pub const COUNT: usize = 2 * SIZE * SIZE;
const GRAVITY: f64 = 9.81;
const DEPTH: f64 = 500.0; // Keep upstream dispersion; this is not a canal depth model.

pub struct Spectrum {
    pub h0: wgpu::Buffer,
    pub waves: wgpu::Buffer,
}

impl Spectrum {
    pub fn new(device: &wgpu::Device, queue: &wgpu::Queue) -> Self {
        let (h0, waves) = initial_conditions();
        let upload = |label, values: &[[f32; 4]]| {
            let buffer = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some(label),
                size: std::mem::size_of_val(values) as u64,
                usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            queue.write_buffer(&buffer, 0, bytemuck::cast_slice(values));
            buffer
        };
        Self {
            h0: upload("Tidewater short-band h0/conjugate", &h0),
            waves: upload("Tidewater short-band k/omega and FFT twiddles", &waves),
        }
    }
}

fn pcg(v: u32) -> u32 {
    let state = v.wrapping_mul(747796405).wrapping_add(2891336453);
    let word = ((state >> ((state >> 28) + 4)) ^ state).wrapping_mul(277803737);
    (word >> 22) ^ word
}
fn unit(v: u32) -> f64 {
    ((v >> 8) as f64 + 0.5) / 16_777_216.0
}

// Same two wave systems as new OceanFFT(renderer), without options.
// scale, speed, direction degrees, fetch km, spread, swell, gamma, short-wave fade.
const SYSTEMS: [[f64; 8]; 2] = [
    [1.0, 7.0, 25.0, 120.0, 0.85, 0.05, 3.3, 0.01],
    [0.48, 6.0, 5.0, 1200.0, 1.0, 0.9, 3.3, 0.1],
];

fn spectrum(k: f64, theta: f64, omega: f64, s: [f64; 8]) -> f64 {
    let [scale, speed, direction, fetch, spread, swell, gamma, fade] = s;
    let alpha = 0.076 * (GRAVITY * fetch * 1000.0 / (speed * speed)).powf(-0.22);
    let peak = 22.0 * (speed * fetch * 1000.0 / (GRAVITY * GRAVITY)).powf(-0.33);
    let sigma: f64 = if omega <= peak { 0.07 } else { 0.09 };
    let r = (-(omega - peak).powi(2) / (2.0 * sigma * sigma * peak * peak)).exp();
    let oh = omega * (DEPTH / GRAVITY).sqrt();
    let tma = if oh <= 1.0 {
        oh * oh * 0.5
    } else if oh < 2.0 {
        1.0 - (2.0 - oh).powi(2) * 0.5
    } else {
        1.0
    };
    let jonswap = scale
        * tma
        * alpha
        * GRAVITY
        * GRAVITY
        * omega.powi(-5)
        * (-1.25 * (peak / omega).powi(4)).exp()
        * gamma.abs().powf(r);
    let ratio = omega / peak;
    let power = if omega > peak {
        ratio.powf(-2.5) * 9.77
    } else {
        ratio.powi(5) * 6.97
    };
    let p = power + ratio.min(20.0).tanh() * 16.0 * swell * swell;
    let norm = if p < 5.0 {
        -0.000564 * p.powi(4) + 0.00776 * p.powi(3) - 0.044 * p * p + 0.192 * p + 0.163
    } else {
        -4.80e-8 * p.powi(4) + 1.07e-5 * p.powi(3) - 9.53e-4 * p * p + 5.90e-2 * p + 3.93e-1
    };
    let angle = theta - direction.to_radians();
    let directional = norm * (angle * 0.5).cos().abs().powf(2.0 * p);
    let base = if angle.cos() > 0.0 {
        angle.cos().powi(2) * 2.0 / PI
    } else {
        0.0
    };
    jonswap * (base * (1.0 - spread) + directional * spread) * (-fade * fade * k * k).exp()
}

pub fn initial_conditions() -> (Vec<[f32; 4]>, Vec<[f32; 4]>) {
    let mut h0 = vec![[0.0; 4]; COUNT];
    let mut waves = vec![[0.0; 4]; COUNT];
    for c in 0..2 {
        let length = [33.3, 7.1][c];
        let dk = TAU / length;
        let low = dk * 6.0;
        let high = if c == 0 { TAU / 7.1 * 6.0 } else { 9999.0 };
        for y in 0..SIZE {
            for x in 0..SIZE {
                let i = c * SIZE * SIZE + y * SIZE + x;
                let kx = (x as f64 - 128.0) * dk;
                let kz = (y as f64 - 128.0) * dk;
                let k = kx.hypot(kz);
                waves[i] = [kx as f32, kz as f32, 0.0, 0.0];
                if k < low || k > high {
                    continue;
                }
                let kd = (k * DEPTH).min(20.0);
                let omega = (k * GRAVITY * kd.tanh()).sqrt();
                let derivative =
                    GRAVITY * (DEPTH * k / kd.cosh().powi(2) + kd.tanh()) / omega * 0.5;
                let energy = SYSTEMS
                    .iter()
                    .map(|&s| spectrum(k, kz.atan2(kx), omega, s))
                    .sum::<f64>()
                    .max(0.0);
                let amp = (energy * derivative.abs() / k * dk * dk).sqrt() * 0.5;
                // Preserve upstream cascade indices 2 and 3 and seed 1337.
                let seed = ((i + 2 * SIZE * SIZE) as u32)
                    .wrapping_mul(4)
                    .wrapping_add(1337 * 7919);
                let r = (-2.0 * unit(pcg(seed)).ln()).sqrt();
                let angle = unit(pcg(seed.wrapping_add(1))) * TAU;
                h0[i] = [
                    (r * angle.cos() * amp) as f32,
                    (r * angle.sin() * amp) as f32,
                    0.0,
                    0.0,
                ];
                waves[i] = [kx as f32, kz as f32, (1.0 / k) as f32, omega as f32];
            }
        }
    }
    for c in 0..2 {
        for y in 0..SIZE {
            for x in 0..SIZE {
                let i = c * SIZE * SIZE + y * SIZE + x;
                let mirror = c * SIZE * SIZE + ((SIZE - y) % SIZE) * SIZE + (SIZE - x) % SIZE;
                h0[i][2] = h0[mirror][0];
                h0[i][3] = -h0[mirror][1];
            }
        }
    }
    waves.extend((0..SIZE / 2).map(|i| {
        let a = TAU * i as f64 / SIZE as f64;
        [a.cos() as f32, a.sin() as f32, 0.0, 0.0]
    }));
    (h0, waves)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn bands_are_hermitian_and_use_dispersive_frequencies() {
        let (h, w) = initial_conditions();
        assert_eq!(h.len(), COUNT);
        assert_eq!(w.len(), COUNT + 128);
        let mut populated = [0; 2];
        for i in 0..COUNT {
            let (c, x, y) = (i / (SIZE * SIZE), i % SIZE, (i / SIZE) % SIZE);
            let j = c * SIZE * SIZE + ((SIZE - y) % SIZE) * SIZE + (SIZE - x) % SIZE;
            assert_eq!(h[i][2], h[j][0]);
            assert_eq!(h[i][3], -h[j][1]);
            assert!(h[i].iter().chain(w[i].iter()).all(|x| x.is_finite()));
            if w[i][3] > 0.0 {
                populated[c] += 1;
                let k = f64::from(w[i][0]).hypot(f64::from(w[i][1]));
                assert!((f64::from(w[i][3]).powi(2) / (GRAVITY * k) - 1.0).abs() < 1e-6);
            }
        }
        assert!(populated[0] > 2000 && populated[1] > 60000);
        assert!(w[COUNT / 2 + 129 * SIZE + 128][3] == 0.0); // Removed long wavelengths.
    }
}
