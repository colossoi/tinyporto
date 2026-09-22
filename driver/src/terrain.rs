//! One straight shoreline per one-metre square. The row-major GPU buffer is
//! [outward normal x, outward normal z, local line offset, kind]. Land satisfies
//! dot(normal, position - cell_center) <= offset. No cell can contain two banks.

pub const SIDE: usize = 40;
pub const CELL_SIZE: f32 = 1.0;
pub const HALF: f32 = SIDE as f32 * CELL_SIZE * 0.5;
pub const WATER: [f32; 4] = [0.0, 0.0, 0.0, 0.0];
pub const LAND: [f32; 4] = [0.0, 0.0, 0.0, 1.0];

fn half_plane(normal: [f32; 2], offset: f32) -> [f32; 4] {
    let reach = (normal[0].abs() + normal[1].abs()) * CELL_SIZE * 0.5;
    // An exactly grid-aligned shore is owned by the land-side square, keeping
    // one bank segment without duplicating it in the adjacent water square.
    if offset > reach {
        LAND
    } else if offset <= -reach {
        WATER
    } else {
        [normal[0], normal[1], offset, 2.0]
    }
}

/// Initial canal, slightly oblique to the grid. Both banks continue through the
/// world bounds. Its width exceeds a cell diagonal, so no square contains both.
pub fn demo() -> Vec<[f32; 4]> {
    let slope = 0.12f32;
    let scale = (1.0 + slope * slope).sqrt();
    let normal = [1.0 / scale, -slope / scale];
    let half_width = 1.15;
    (0..SIDE * SIDE)
        .map(|i| {
            let x = (i % SIDE) as f32 + 0.5 - HALF;
            let z = (i / SIDE) as f32 + 0.5 - HALF;
            let center = normal[0] * x + normal[1] * z;
            if center < 0.0 {
                half_plane(normal, -half_width - center)
            } else {
                half_plane([-normal[0], -normal[1]], center - half_width)
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn is_land(cells: &[[f32; 4]], cell: usize, x: f32, z: f32) -> bool {
        let c = cells[cell];
        let cx = (cell % SIDE) as f32 + 0.5 - HALF;
        let cz = (cell / SIDE) as f32 + 0.5 - HALF;
        c[3] == 1.0 || (c[3] == 2.0 && c[0] * (x - cx) + c[1] * (z - cz) <= c[2])
    }

    #[test]
    fn demo_has_one_boundary_per_cell_and_continuous_shared_edges() {
        let cells = demo();
        assert_eq!(cells.len() * std::mem::size_of::<[f32; 4]>(), 25_600);
        for kind in [0.0, 1.0, 2.0] {
            assert!(cells.iter().any(|c| c[3] == kind));
        }
        let slope = 0.12f32;
        let scale = (1.0 + slope * slope).sqrt();
        for cell in 0..cells.len() {
            let x = (cell % SIDE) as f32 - HALF;
            let z = (cell / SIDE) as f32 - HALF;
            // Interior samples detect missing second banks as well as wrong signs.
            for iz in 0..=10 {
                for ix in 0..=10 {
                    let px = x + ix as f32 * 0.1;
                    let pz = z + iz as f32 * 0.1;
                    let shore = ((px - slope * pz) / scale).abs() - 1.15;
                    if shore.abs() > 0.00001 {
                        assert_eq!(is_land(&cells, cell, px, pz), shore >= 0.0);
                    }
                }
            }
            for k in 0..=16 {
                let t = k as f32 / 16.0;
                if cell % SIDE + 1 < SIDE {
                    assert_eq!(
                        is_land(&cells, cell, x + 1.0, z + t),
                        is_land(&cells, cell + 1, x + 1.0, z + t)
                    );
                }
                if cell / SIDE + 1 < SIDE {
                    assert_eq!(
                        is_land(&cells, cell, x + t, z + 1.0),
                        is_land(&cells, cell + SIDE, x + t, z + 1.0)
                    );
                }
            }
        }
    }

    #[test]
    fn grid_aligned_shore_has_exactly_one_owner() {
        for normal in [[1.0, 0.0], [-1.0, 0.0], [0.0, 1.0], [0.0, -1.0]] {
            assert_eq!(half_plane(normal, 0.5)[3], 2.0);
            assert_eq!(half_plane(normal, -0.5), WATER);
            assert_eq!(half_plane(normal, 0.5001), LAND);
        }
    }
}
