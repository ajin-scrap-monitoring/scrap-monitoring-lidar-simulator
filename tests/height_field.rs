use std::path::Path;

use scrap_monitoring_lidar_simulator::{
    configuration::load_simulator_inputs,
    geometry::{GeometryError, MAX_POLYGON_VERTICES, Polygon2, Ray, Triangle, Vec2, Vec3},
    scenario::{CellCoverage, HeightField, MAX_GRID_NODES, MAX_SLOPE_RELAXATION_ITERATIONS},
};
use serde_json::Value;

fn point(x: f64, y: f64) -> Vec2 {
    Vec2::new(x, y).unwrap()
}
fn polygon(points: &[[f64; 2]]) -> Polygon2 {
    Polygon2::new(
        points
            .iter()
            .map(|value| Vec2::try_from(*value).unwrap())
            .collect(),
    )
    .unwrap()
}
fn rectangle() -> Polygon2 {
    polygon(&[[0.0, 0.0], [2.0, 0.0], [2.0, 1.0], [0.0, 1.0]])
}
fn number(value: &Value) -> f64 {
    value.as_f64().unwrap()
}
fn close(actual: f64, expected: f64, absolute: f64) {
    let tolerance = absolute.max(1e-12 * actual.abs().max(expected.abs()));
    assert!(
        actual.is_finite() && expected.is_finite() && (actual - expected).abs() <= tolerance,
        "actual={actual:.17e} expected={expected:.17e} tolerance={tolerance:.17e}"
    );
}
fn vector(actual: &[f64], expected: &Value) {
    let expected = expected.as_array().unwrap();
    assert_eq!(actual.len(), expected.len());
    for (actual, expected) in actual.iter().zip(expected) {
        close(*actual, number(expected), 1e-10);
    }
}
fn matrix(actual: &[f64], shape: (usize, usize), expected: &Value) {
    let rows = expected.as_array().unwrap();
    assert_eq!(shape.0, rows.len());
    assert_eq!(actual.len(), shape.0 * shape.1);
    for (actual, expected) in actual.chunks_exact(shape.1).zip(rows) {
        vector(actual, expected);
    }
}
fn bounded(surface: &HeightField) {
    assert!(surface.heights_m().iter().all(|height| height.is_finite()
        && *height >= surface.floor_z_m()
        && *height <= surface.top_z_m()));
    assert!((0.0..=1.0).contains(&surface.fill_ratio()));
}

#[test]
fn every_height_field_value_matches_the_model_v1_fixture() {
    let fixture: Value = serde_json::from_str(include_str!(
        "fixtures/model-v1/height-field-operations.json"
    ))
    .unwrap();
    assert_eq!(fixture["comparison_class"], "rng-independent");
    assert_eq!(fixture["grid_order"], "y-major");
    assert_eq!(fixture["source"], "examples/simulator.v2.json");
    let inputs = load_simulator_inputs(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/simulator.v2.json"),
    )
    .unwrap();
    let environment = &inputs.environment;
    let config = &inputs.simulator.scenario;
    let mut surface = HeightField::new(
        polygon(&environment.boundary_xy_m),
        environment.floor_z_m,
        environment.top_z_m,
        config.surface.cell_size_m,
    )
    .unwrap();
    let center = Vec2::try_from(
        config.inlet_positions_xy_m[fixture["inlet_index"].as_u64().unwrap() as usize],
    )
    .unwrap();
    let volume_tolerance = surface.capacity_m3().max(1.0) * 1e-10;
    vector(surface.x_coordinates_m(), &fixture["x_coordinates_m"]);
    vector(surface.y_coordinates_m(), &fixture["y_coordinates_m"]);
    matrix(
        surface.node_area_m2(),
        surface.shape(),
        &fixture["node_area_m2"],
    );
    close(
        surface.surface_area_m2(),
        number(&fixture["surface_area_m2"]),
        1e-10,
    );
    close(
        surface.capacity_m3(),
        number(&fixture["capacity_m3"]),
        volume_tolerance,
    );
    let operations = fixture["operations"].as_array().unwrap();
    assert_eq!(operations.len(), 5);
    for operation in operations {
        match operation["operation"].as_str().unwrap() {
            "add_capacity_fraction" | "remove_capacity_fraction" => {
                let amount = surface.capacity_m3() * number(&operation["fraction"]);
                let result = if operation["operation"] == "add_capacity_fraction" {
                    surface
                        .add_volume(amount, center, config.surface.pile_spread_radius_m)
                        .unwrap()
                } else {
                    surface.remove_volume_uniformly(amount).unwrap()
                };
                close(
                    result.requested_m3,
                    number(&operation["change"]["requested_m3"]),
                    volume_tolerance,
                );
                close(
                    result.applied_m3,
                    number(&operation["change"]["applied_m3"]),
                    volume_tolerance,
                );
            }
            "roughness" => {
                let result = surface
                    .apply_local_roughness(
                        center,
                        number(&operation["radius_m"]),
                        number(&operation["peak_delta_m"]),
                    )
                    .unwrap();
                close(
                    result.requested_peak_delta_m,
                    number(&operation["result"]["requested_peak_delta_m"]),
                    1e-10,
                );
                close(
                    result.applied_peak_delta_m,
                    number(&operation["result"]["applied_peak_delta_m"]),
                    1e-10,
                );
            }
            "relax_slopes" => {
                let result = surface.relax_slopes().unwrap();
                assert_eq!(
                    result.iterations as u64,
                    operation["result"]["iterations"].as_u64().unwrap()
                );
                close(
                    result.moved_volume_m3,
                    number(&operation["result"]["moved_volume_m3"]),
                    volume_tolerance,
                );
                close(
                    result.remaining_excess_height_m,
                    number(&operation["result"]["remaining_excess_height_m"]),
                    1e-10,
                );
            }
            unexpected => panic!("unsupported fixture operation {unexpected}"),
        }
        matrix(
            surface.heights_m(),
            surface.shape(),
            &operation["state"]["heights_m"],
        );
        close(
            surface.volume_m3(),
            number(&operation["state"]["volume_m3"]),
            volume_tolerance,
        );
        close(
            surface.fill_ratio(),
            number(&operation["state"]["fill_ratio"]),
            1e-10,
        );
        bounded(&surface);
    }
}

#[test]
fn finite_primitives_preserve_world_vector_semantics() {
    let left = Vec3::new(1.0, 2.0, 3.0).unwrap();
    let right = Vec3::new(-2.0, 4.0, 1.0).unwrap();
    assert_eq!(
        left.checked_add(right).unwrap().coordinates(),
        [-1.0, 6.0, 4.0]
    );
    assert_eq!(
        left.checked_sub(right).unwrap().coordinates(),
        [3.0, -2.0, 2.0]
    );
    assert_eq!(
        left.checked_mul(2.0).unwrap().coordinates(),
        [2.0, 4.0, 6.0]
    );
    assert_eq!(left.dot(right).unwrap(), 9.0);
    assert_eq!(left.cross(right).unwrap().coordinates(), [-10.0, -7.0, 8.0]);
    let large = Vec3::new(1e308, 1e308, 0.0).unwrap().normalized().unwrap();
    close(large.length(), 1.0, 1e-15);
    assert!(Vec3::new(0.0, 0.0, 0.0).unwrap().normalized().is_err());
    for value in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        assert!(Vec2::new(value, 0.0).is_err());
        assert!(Vec3::new(0.0, value, 0.0).is_err());
        assert!(left.checked_mul(value).is_err());
    }
    assert!(
        Vec3::new(f64::MAX, 0.0, 0.0)
            .unwrap()
            .checked_mul(2.0)
            .is_err()
    );
    let huge = Vec3::new(f64::MAX, f64::MAX, f64::MAX).unwrap();
    assert!(huge.dot(huge).is_err());
    assert!(
        Vec2::new(f64::MAX, f64::MAX)
            .unwrap()
            .cross(Vec2::new(f64::MAX, -f64::MAX).unwrap())
            .is_err()
    );
}

#[test]
fn polygon_vertex_limit_bounds_quadratic_geometry_work() {
    let vertices = vec![point(0.0, 0.0); MAX_POLYGON_VERTICES + 1];
    assert_eq!(
        Polygon2::new(vertices).unwrap_err(),
        GeometryError::Allocation("polygon exceeds MAX_POLYGON_VERTICES")
    );
}

#[test]
fn polygon_winding_boundary_and_invalid_shapes_match_the_reference() {
    let boundary = polygon(&[
        [0.0, 0.0],
        [2.0, 0.0],
        [2.0, 1.0],
        [1.0, 1.0],
        [1.0, 2.0],
        [0.0, 2.0],
    ]);
    assert_eq!(boundary.signed_area(), 3.0);
    assert!(boundary.contains(point(0.5, 1.5)).unwrap());
    assert!(!boundary.contains(point(1.5, 1.5)).unwrap());
    assert!(boundary.contains(point(1.0, 1.0)).unwrap());
    assert!(
        !boundary
            .contains_with_boundary(point(1.0, 1.0), false)
            .unwrap()
    );
    let reversed = Polygon2::new(boundary.vertices().iter().copied().rev().collect()).unwrap();
    assert_eq!(reversed.signed_area(), -3.0);
    for points in [
        vec![[0.0, 0.0], [1.0, 0.0]],
        vec![[0.0, 0.0], [1.0, 0.0], [0.0, 0.0]],
        vec![[0.0, 0.0], [1.0, 0.0], [2.0, 0.0]],
        vec![[0.0, 0.0], [3.0, 0.0], [3.0, 3.0], [1.5, 0.0], [0.0, 3.0]],
        vec![[0.0, 0.0], [1e308, 0.0], [0.0, 1e308]],
    ] {
        assert!(
            Polygon2::new(
                points
                    .into_iter()
                    .map(|p| Vec2::try_from(p).unwrap())
                    .collect()
            )
            .is_err()
        );
    }
}

#[test]
fn rays_use_physical_distance_and_triangles_reject_degenerate_area() {
    let origin = Vec3::new(1.0, 2.0, 3.0).unwrap();
    let direction = Vec3::new(0.0, 0.0, -1.0).unwrap();
    let ray = Ray::new(origin, direction).unwrap();
    assert_eq!(ray.point_at(2.0).unwrap().coordinates(), [1.0, 2.0, 1.0]);
    assert_eq!(ray.origin(), origin);
    assert_eq!(ray.direction(), direction);
    for value in [-1.0, f64::NAN, f64::INFINITY] {
        assert!(ray.point_at(value).is_err());
    }
    assert!(Ray::new(origin, Vec3::new(0.0, 0.0, 2.0).unwrap()).is_err());
    let a = Vec3::new(0.0, 0.0, 0.0).unwrap();
    let b = Vec3::new(1.0, 0.0, 0.0).unwrap();
    let c = Vec3::new(0.0, 1.0, 0.0).unwrap();
    assert_eq!(Triangle::new(a, b, c).unwrap().vertices(), [a, b, c]);
    assert!(Triangle::new(a, b, Vec3::new(2.0, 0.0, 0.0).unwrap()).is_err());
}

#[test]
fn rectangle_grid_uses_bilinear_node_areas_and_interpolation() {
    let mut surface = HeightField::new(rectangle(), -1.0, 3.0, 0.5).unwrap();
    assert_eq!(surface.shape(), (3, 5));
    assert_eq!(surface.capacity_m3(), 8.0);
    assert_eq!(surface.volume_m3(), 0.0);
    assert_eq!(surface.height_at(point(0.7, 0.3)).unwrap(), -1.0);
    assert!(
        surface
            .cell_coverage()
            .iter()
            .all(|cell| *cell == CellCoverage::Full)
    );
    assert_eq!(surface.node_area_m2()[0], 0.0625);
    assert_eq!(surface.node_area_m2()[6], 0.25);
    surface.add_volume(1.0, point(0.0, 0.0), 0.4).unwrap();
    let heights = surface.heights_m();
    close(
        surface.height_at(point(0.25, 0.25)).unwrap(),
        (heights[0] + heights[1] + heights[5] + heights[6]) / 4.0,
        1e-12,
    );
    assert!(surface.height_at(point(3.0, 0.0)).is_err());
    close(
        surface.mean_height_within(point(0.23, 0.27), 1e-5).unwrap(),
        surface.height_at(point(0.23, 0.27)).unwrap(),
        1e-12,
    );
}

#[test]
fn concave_partial_cells_and_clockwise_collinear_boundaries_preserve_volume() {
    for boundary in [
        polygon(&[
            [0.0, 0.0],
            [2.0, 0.0],
            [2.0, 1.0],
            [1.0, 1.0],
            [1.0, 2.0],
            [0.0, 2.0],
        ]),
        polygon(&[[0.0, 0.0], [0.0, 1.0], [1.0, 1.0], [2.0, 1.0], [2.0, 0.0]]),
    ] {
        let mut surface = HeightField::new(boundary, -2.0, 2.0, 0.3).unwrap();
        assert!(surface.cell_coverage().contains(&CellCoverage::Partial));
        assert!(surface.node_area_m2().iter().all(|weight| *weight >= 0.0));
        close(
            surface.node_area_m2().iter().sum(),
            surface.surface_area_m2(),
            1e-12,
        );
        surface.add_volume(4.5, point(0.5, 0.5), 0.4).unwrap();
        surface.relax_slopes().unwrap();
        surface.remove_volume(1.75, point(0.5, 0.5), 0.5).unwrap();
        close(surface.volume_m3(), 2.75, 1e-10);
        bounded(&surface);
    }
}

#[test]
fn capacity_overflow_emptying_and_tiny_kernels_keep_all_available_volume() {
    let mut surface = HeightField::new(rectangle(), 0.0, 1.0, 0.25).unwrap();
    surface.add_volume(1.0, point(0.0, 0.0), 1e-100).unwrap();
    close(surface.volume_m3(), 1.0, 1e-10);
    let added = surface.add_volume(3.0, point(0.0, 0.0), 0.25).unwrap();
    close(added.applied_m3, 1.0, 1e-10);
    close(added.unapplied_m3(), 2.0, 1e-10);
    assert!(surface.heights_m().iter().all(|height| *height == 1.0));
    let removed = surface.remove_volume_uniformly(5.0).unwrap();
    close(removed.applied_m3, 2.0, 1e-10);
    close(removed.unapplied_m3(), 3.0, 1e-10);
    assert!(surface.heights_m().iter().all(|height| *height == 0.0));
}

#[test]
fn roughness_is_signed_local_bounded_and_volume_preserving() {
    let mut surface = HeightField::new(rectangle(), 0.0, 2.0, 0.1).unwrap();
    assert_eq!(
        surface
            .apply_local_roughness(point(0.5, 0.5), 0.3, 0.1)
            .unwrap()
            .applied_peak_delta_m,
        0.0
    );
    surface.add_volume(1.0, point(0.5, 0.5), 0.4).unwrap();
    let before = surface.volume_m3();
    for peak in [0.1, -0.1, 100.0, -100.0] {
        let result = surface
            .apply_local_roughness(point(0.5, 0.5), 0.3, peak)
            .unwrap();
        assert!(result.applied_peak_delta_m.abs() <= peak.abs());
        close(surface.volume_m3(), before, 1e-10);
        bounded(&surface);
    }
    assert_eq!(
        surface
            .apply_local_roughness(point(0.5, 0.5), 1e-6, 0.1)
            .unwrap()
            .applied_peak_delta_m,
        0.0
    );
    assert_eq!(
        surface
            .apply_local_roughness(point(0.5, 0.5), 0.3, 0.0)
            .unwrap()
            .applied_peak_delta_m,
        0.0
    );
}

#[test]
fn slope_relaxation_is_simultaneous_bounded_symmetric_and_deterministic() {
    let mut surface = HeightField::new(
        polygon(&[[0.0, 0.0], [2.0, 0.0], [2.0, 2.0], [0.0, 2.0]]),
        0.0,
        2.0,
        0.1,
    )
    .unwrap();
    surface.add_volume(0.5, point(1.0, 1.0), 0.05).unwrap();
    let mut copy = surface.clone();
    let first = surface.relax_slopes_with(35.0, 64).unwrap();
    assert_eq!(first, copy.relax_slopes_with(35.0, 64).unwrap());
    assert_eq!(surface.heights_m(), copy.heights_m());
    assert!((1..=64).contains(&first.iterations));
    close(surface.volume_m3(), 0.5, 1e-10);
    let columns = surface.shape().1;
    for row in surface.heights_m().chunks_exact(columns) {
        for (left, right) in row.iter().zip(row.iter().rev()) {
            close(*left, *right, 1e-10);
        }
    }
    assert!(first.remaining_excess_height_m < 0.01);
    bounded(&surface);
}

#[test]
fn immutable_snapshots_share_grid_but_never_follow_later_height_updates() {
    let mut surface = HeightField::new(rectangle(), 0.0, 2.0, 0.1).unwrap();
    surface.add_volume(1.0, point(0.5, 0.5), 0.3).unwrap();
    let snapshot = surface.surface_snapshot().unwrap();
    let clone = snapshot.clone();
    let before = snapshot.heights_m().to_vec();
    surface.remove_volume_uniformly(1.0).unwrap();
    assert_eq!(snapshot.heights_m(), before);
    assert_eq!(clone.heights_m(), before);
    assert_eq!(
        snapshot.x_coordinates_m().as_ptr(),
        surface.x_coordinates_m().as_ptr()
    );
    assert_eq!(snapshot.heights_m().as_ptr(), clone.heights_m().as_ptr());
    assert_eq!(snapshot.shape(), surface.shape());
    close(snapshot.volume_m3(), 1.0, 1e-10);
    assert_eq!(surface.volume_m3(), 0.0);
}

#[test]
fn repeated_mixed_updates_preserve_volume_and_bounds() {
    let mut surface = HeightField::new(
        polygon(&[
            [0.0, 0.0],
            [2.0, 0.0],
            [2.0, 1.0],
            [1.0, 1.0],
            [1.0, 2.0],
            [0.0, 2.0],
        ]),
        -1.0,
        2.0,
        0.15,
    )
    .unwrap();
    for index in 0..128 {
        let center = if index % 2 == 0 {
            point(0.3, 0.4)
        } else {
            point(0.4, 1.4)
        };
        let radius = [1e-100, 0.05, 0.3, 1.0][index % 4];
        let amount = 0.1 + (index % 7) as f64 * 0.025;
        let before = surface.volume_m3();
        let adding = index % 3 != 0;
        let result = if adding {
            surface.add_volume(amount, center, radius).unwrap()
        } else {
            surface.remove_volume(amount, center, radius).unwrap()
        };
        let expected = before
            + if adding {
                result.applied_m3
            } else {
                -result.applied_m3
            };
        close(surface.volume_m3(), expected, 1e-10);
        surface
            .apply_local_roughness(center, 0.35, if adding { 0.08 } else { -0.08 })
            .unwrap();
        surface.relax_slopes().unwrap();
        close(surface.volume_m3(), expected, 1e-10);
        bounded(&surface);
    }
}

#[test]
fn invalid_bounds_and_oversized_grids_fail_before_allocation() {
    assert_eq!(MAX_GRID_NODES, 262_144);
    for (floor, top, cell) in [
        (0.0, 0.0, 0.1),
        (1.0, 0.0, 0.1),
        (0.0, f64::INFINITY, 0.1),
        (0.0, 1.0, 0.0),
        (0.0, 1.0, f64::NAN),
        (0.0, 1.0, f64::MIN_POSITIVE),
        (-f64::MAX, f64::MAX, 0.1),
    ] {
        assert!(HeightField::new(rectangle(), floor, top, cell).is_err());
    }
    for cell in [1e-9, 1e-100, 1.0 / MAX_GRID_NODES as f64, 0.001] {
        assert!(matches!(
            HeightField::new(rectangle(), 0.0, 1.0, cell),
            Err(GeometryError::Allocation(_))
        ));
    }
}

#[test]
fn invalid_updates_do_not_mutate_the_surface() {
    let mut surface = HeightField::new(rectangle(), 0.0, 2.0, 0.1).unwrap();
    let before = surface.heights_m().to_vec();
    for value in [-1.0, f64::NAN, f64::INFINITY] {
        assert!(surface.add_volume(value, point(0.5, 0.5), 0.2).is_err());
        assert!(surface.remove_volume_uniformly(value).is_err());
    }
    assert!(surface.add_volume(0.0, point(3.0, 0.0), 0.2).is_err());
    assert!(
        surface
            .apply_local_roughness(point(0.5, 0.5), 0.0, 0.0)
            .is_err()
    );
    assert!(
        surface
            .apply_local_roughness(point(0.5, 0.5), 0.2, f64::NAN)
            .is_err()
    );
    for angle in [0.0, 90.0, f64::NAN] {
        assert!(surface.relax_slopes_with(angle, 32).is_err());
    }
    assert!(surface.relax_slopes_with(35.0, 0).is_err());
    assert!(matches!(
        surface.relax_slopes_with(35.0, MAX_SLOPE_RELAXATION_ITERATIONS + 1),
        Err(GeometryError::Allocation(_))
    ));
    assert_eq!(surface.heights_m(), before);
}
