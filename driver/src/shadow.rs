//! Conservative sun-space bins of masonry boxes. The grid accelerates exact
//! ray/box queries; its resolution never quantizes the shadow silhouette.
use anyhow::{ensure, Result};

pub const DEFAULT_SUN: [f32; 3] = [-0.5, 0.52, 0.35];
const SIDE: usize = 512;
type V3 = [f64; 3];
type V2 = [f64; 2];

fn dot(a: V3, b: V3) -> f64 {
    a.iter().zip(b).map(|(a, b)| a * b).sum()
}
fn cross(a: V3, b: V3) -> V3 {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}
fn scale(v: V3, s: f64) -> V3 {
    v.map(|x| x * s)
}
fn normalized(v: V3) -> V3 {
    scale(v, 1.0 / dot(v, v).sqrt())
}
fn xyz(v: [f32; 4]) -> V3 {
    [v[0] as f64, v[1] as f64, v[2] as f64]
}

pub fn sun_direction(v: [f32; 3]) -> Result<[f32; 3]> {
    ensure!(
        v.iter().all(|x| x.is_finite()),
        "sun direction must be finite"
    );
    let v = v.map(f64::from);
    ensure!(dot(v, v) > 0.0, "sun direction cannot be zero");
    Ok(normalized(v).map(|x| x as f32))
}

#[derive(Clone, Debug)]
pub struct Caster {
    center: V3,
    axes: [V3; 3],
    half: V3,
}

/// Consume the four-vec4 records exported by Wyn's actual placement functions.
pub fn casters(bytes: &[u8]) -> Result<Vec<Caster>> {
    ensure!(bytes.len() % 64 == 0, "invalid shadow geometry buffer size");
    let mut result = Vec::new();
    for bytes in bytes.chunks_exact(64) {
        let mut records = [[0.0; 4]; 4];
        for (value, bytes) in records.iter_mut().flatten().zip(bytes.chunks_exact(4)) {
            *value = f32::from_le_bytes(bytes.try_into().unwrap());
        }
        if records[0][3] < 0.0 {
            continue;
        }
        ensure!(
            records.iter().flatten().all(|v| v.is_finite()),
            "nonfinite caster"
        );
        let half = [
            records[1][3] as f64,
            records[2][3] as f64,
            records[3][3] as f64,
        ];
        ensure!(half.iter().all(|h| *h > 0.0), "degenerate caster");
        let axes = [xyz(records[1]), xyz(records[2]), xyz(records[3])];
        ensure!(
            dot(axes[0], cross(axes[1], axes[2])).abs() > 1e-10,
            "singular caster basis"
        );
        result.push(Caster {
            center: xyz(records[0]),
            axes,
            half,
        });
    }
    Ok(result)
}

fn turn(a: V2, b: V2, c: V2) -> f64 {
    (b[0] - a[0]) * (c[1] - a[1]) - (b[1] - a[1]) * (c[0] - a[0])
}

fn hull(mut points: Vec<V2>) -> Vec<V2> {
    points.sort_by(|a, b| a[0].total_cmp(&b[0]).then(a[1].total_cmp(&b[1])));
    points.dedup();
    let mut out = Vec::new();
    for p in &points {
        while out.len() >= 2 && turn(out[out.len() - 2], out[out.len() - 1], *p) <= 0.0 {
            out.pop();
        }
        out.push(*p);
    }
    let lower = out.len();
    for p in points.iter().rev().skip(1) {
        while out.len() > lower && turn(out[out.len() - 2], out[out.len() - 1], *p) <= 0.0 {
            out.pop();
        }
        out.push(*p);
    }
    out.pop();
    out
}

pub struct Grid {
    pub records: Vec<[f32; 4]>,
    pub references: usize,
    pub max_candidates: usize,
}

pub fn build(casters: &[Caster], sun: [f32; 3]) -> Result<Grid> {
    let direction = sun_direction(sun)?;
    let sun = direction.map(f64::from);
    // A vertical sun is valid too. Round the projection basis to its stored
    // precision before binning, so the CPU and shader use the same planes.
    let right = if sun[0].hypot(sun[2]) > 1e-8 {
        normalized([sun[2], 0.0, -sun[0]])
    } else {
        [1.0, 0.0, 0.0]
    }
    .map(|x| (x as f32) as f64);
    let up = normalized(cross(sun, right)).map(|x| (x as f32) as f64);
    let mut min = [-30.0f64; 2];
    let mut max = [30.0f64; 2];
    let mut footprints = Vec::with_capacity(casters.len());
    let mut boxes = Vec::with_capacity(casters.len() * 4);
    for caster in casters {
        let mut vertices = Vec::with_capacity(8);
        for corner in 0..8 {
            let mut p = caster.center;
            for (axis, half) in caster.axes.iter().zip(caster.half).enumerate() {
                let (basis, half) = half;
                let sign = if corner & (1 << axis) == 0 { -1.0 } else { 1.0 };
                for j in 0..3 {
                    p[j] += basis[j] * half * sign;
                }
            }
            vertices.push([dot(p, right), dot(p, up)]);
        }
        for p in &vertices {
            for j in 0..2 {
                min[j] = min[j].min(p[j] - 0.01);
                max[j] = max[j].max(p[j] + 0.01);
            }
        }
        footprints.push(hull(vertices));
        boxes.push([
            caster.center[0] as f32,
            caster.center[1] as f32,
            caster.center[2] as f32,
            0.0,
        ]);
        let axes = caster.axes;
        let determinant = dot(axes[0], cross(axes[1], axes[2]));
        for i in 0..3 {
            // Dual basis: do not assume the GPU's rounded axes are orthogonal.
            let dual = scale(
                cross(axes[(i + 1) % 3], axes[(i + 2) % 3]),
                1.0 / determinant,
            );
            let direction = dot(dual, sun);
            let (coeff, extent) = if direction.abs() < 1e-8 {
                (dual, -caster.half[i])
            } else {
                (
                    scale(dual, 1.0 / direction),
                    caster.half[i] / direction.abs(),
                )
            };
            boxes.push([
                coeff[0] as f32,
                coeff[1] as f32,
                coeff[2] as f32,
                extent as f32,
            ]);
        }
    }
    let span = (max[0] - min[0]).max(max[1] - min[1]);
    let inverse_cell = (SIDE as f64 / span) as f32;
    let cell = 1.0 / inverse_cell as f64;
    let min = min.map(|x| (x as f32) as f64);
    let mut bins: Vec<Vec<u32>> = vec![Vec::new(); SIDE * SIDE];
    for (id, polygon) in footprints.iter().enumerate() {
        let mut lo = [f64::INFINITY; 2];
        let mut hi = [f64::NEG_INFINITY; 2];
        for p in polygon {
            for j in 0..2 {
                lo[j] = lo[j].min(p[j]);
                hi[j] = hi[j].max(p[j]);
            }
        }
        // Conservatism covers f32 projection rounding at shared bin boundaries.
        let padding = 1e-4 + span * 2e-7;
        let lower = [0, 1].map(|j| {
            (((lo[j] - min[j] - padding) / cell).floor() as i64).clamp(0, SIDE as i64 - 1) as usize
        });
        let upper = [0, 1].map(|j| {
            (((hi[j] - min[j] + padding) / cell).floor() as i64).clamp(0, SIDE as i64 - 1) as usize
        });
        for y in lower[1]..=upper[1] {
            for x in lower[0]..=upper[0] {
                let center = [
                    min[0] + (x as f64 + 0.5) * cell,
                    min[1] + (y as f64 + 0.5) * cell,
                ];
                let radius = cell * 0.5 + padding;
                let overlaps = polygon
                    .iter()
                    .zip(polygon.iter().cycle().skip(1))
                    .all(|(a, b)| {
                        turn(*a, *b, center) + radius * ((b[0] - a[0]).abs() + (b[1] - a[1]).abs())
                            >= 0.0
                    });
                if overlaps {
                    bins[y * SIDE + x].push(id as u32);
                }
            }
        }
    }
    let references = bins.iter().map(Vec::len).sum::<usize>();
    let max_candidates = bins.iter().map(Vec::len).max().unwrap_or(0);
    let id_base = 4 + bins.len();
    let box_base = (id_base + references.div_ceil(4)).div_ceil(4) * 4;
    // Integer indices encoded as floats must remain exactly representable.
    ensure!(
        box_base + boxes.len() < 1 << 24 && references < 1 << 24,
        "shadow grid exceeds packed index range"
    );
    let mut records = vec![[0.0; 4]; box_base];
    records[0] = [id_base as f32, box_base as f32, SIDE as f32, inverse_cell];
    records[1] = [
        right[0] as f32,
        right[1] as f32,
        right[2] as f32,
        min[0] as f32,
    ];
    records[2] = [up[0] as f32, up[1] as f32, up[2] as f32, min[1] as f32];
    records[3] = [direction[0], direction[1], direction[2], 0.0];
    let mut offset = 0;
    for (i, bin) in bins.iter().enumerate() {
        records[4 + i] = [offset as f32, bin.len() as f32, 0.0, 0.0];
        for id in bin {
            records[id_base + offset / 4][offset % 4] = *id as f32;
            offset += 1;
        }
    }
    records.extend(boxes);
    Ok(Grid {
        records,
        references,
        max_candidates,
    })
}

/// Brute-force f64 reference for GPU tests: transform the ray into each box and
/// clip its interval against all six faces, independently of the grid/packing.
#[cfg(test)]
pub fn reference_occluded(casters: &[Caster], direction: [f32; 3], point: [f32; 3]) -> bool {
    let direction = direction.map(f64::from);
    let point = point.map(f64::from);
    casters.iter().any(|b| {
        let origin = [0, 1, 2].map(|i| point[i] - b.center[i]);
        // A conservative enclosing sphere rejects distant boxes cheaply.
        let radius: f64 = (0..3)
            .map(|i| b.half[i] * dot(b.axes[i], b.axes[i]).sqrt())
            .sum();
        let projection = dot(origin, direction);
        if dot(origin, origin) - projection * projection / dot(direction, direction)
            > radius * radius + 1e-6
        {
            return false;
        }
        let determinant = dot(b.axes[0], cross(b.axes[1], b.axes[2]));
        let mut enter = 0.006f64;
        let mut exit = f64::INFINITY;
        for i in 0..3 {
            let dual = scale(
                cross(b.axes[(i + 1) % 3], b.axes[(i + 2) % 3]),
                1.0 / determinant,
            );
            let o = dot(origin, dual);
            let d = dot(direction, dual);
            if d.abs() < 1e-12 {
                if o.abs() > b.half[i] {
                    return false;
                }
            } else {
                let t0 = (-b.half[i] - o) / d;
                let t1 = (b.half[i] - o) / d;
                enter = enter.max(t0.min(t1));
                exit = exit.min(t0.max(t1));
                if exit < enter {
                    return false;
                }
            }
        }
        exit >= enter
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_grid_and_invalid_sun() {
        let grid = build(&[], DEFAULT_SUN).unwrap();
        assert_eq!(grid.references, 0);
        assert!(grid.records[4..4 + SIDE * SIDE].iter().all(|h| h[1] == 0.0));
        assert!(sun_direction([0.0; 3]).is_err());
        assert!(sun_direction([f32::NAN, 1.0, 0.0]).is_err());
    }

    #[test]
    fn expanded_bounds_and_dense_cells_keep_every_caster() {
        let boxes: Vec<_> = (0..160)
            .map(|i| Caster {
                center: [100.0, i as f64, 0.0],
                axes: [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
                half: [0.5; 3],
            })
            .collect();
        let grid = build(&boxes, [0.0, 1.0, 0.0]).unwrap();
        assert_eq!(grid.max_candidates, 160);
        let x = ((100.0 - grid.records[1][3]) * grid.records[0][3]).floor() as usize;
        let y = ((0.0 - grid.records[2][3]) * grid.records[0][3]).floor() as usize;
        assert!(x < SIDE && y < SIDE);
        let header = grid.records[4 + y * SIDE + x];
        assert_eq!(header[1], 160.0);
        let base = grid.records[0][0] as usize;
        for id in 0..160 {
            let index = header[0] as usize + id;
            assert_eq!(grid.records[base + index / 4][index % 4], id as f32);
        }
    }
}
