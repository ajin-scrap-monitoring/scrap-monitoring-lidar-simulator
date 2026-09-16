use scrap_monitoring_lidar_simulator::{
    configuration::{SensorConfig, SimulatorInputs, load_simulator_inputs},
    geometry::{Polygon2, Triangle, Vec2, Vec3},
    measurement::{
        CollectionOcclusionEvent, EnvironmentScene, FallingMaterialEvent, HitKind,
        ReferenceScanner, ScheduledScan, SensorFrame, SnapshotEventCoordinator,
        SpatialDistortionResolver, TimedReferenceScan, VoidEvent,
        resolve_collection_occlusion_distances, resolve_falling_material_distances,
        resolve_void_distances,
    },
    scenario::HeightField,
};
use serde_json::Value;

const COORDINATES: &str = include_str!("fixtures/model-v1/coordinates.json");
const DISTORTIONS: &str = include_str!("fixtures/model-v1/distortion-events.json");
const HEIGHT_FIELD: &str = include_str!("fixtures/model-v1/height-field-operations.json");
const TOLERANCE: f64 = 1e-10;

fn inputs() -> SimulatorInputs {
    load_simulator_inputs("examples/simulator.v2.json").unwrap()
}

fn polygon(inputs: &SimulatorInputs) -> Polygon2 {
    Polygon2::new(
        inputs
            .environment
            .boundary_xy_m
            .iter()
            .map(|point| Vec2::new(point[0], point[1]).unwrap())
            .collect(),
    )
    .unwrap()
}

fn sensor_frame(sensor: &SensorConfig) -> SensorFrame {
    SensorFrame::new(
        Vec3::try_from(sensor.p0_m).unwrap(),
        Vec3::try_from(sensor.u0).unwrap(),
        Vec3::try_from(sensor.u90).unwrap(),
    )
    .unwrap()
}

fn deposited_surface(inputs: &SimulatorInputs) -> HeightField {
    let scenario = &inputs.simulator.scenario;
    let mut surface = HeightField::new(
        polygon(inputs),
        inputs.environment.floor_z_m,
        inputs.environment.top_z_m,
        scenario.surface.cell_size_m,
    )
    .unwrap();
    surface
        .add_volume(
            surface.capacity_m3() * 0.3,
            Vec2::try_from(scenario.inlet_positions_xy_m[0]).unwrap(),
            scenario.surface.pile_spread_radius_m,
        )
        .unwrap();
    surface.relax_slopes().unwrap();
    surface
}

fn assert_close(actual: f64, expected: f64, path: &str) {
    let tolerance = TOLERANCE.max(expected.abs() * 1e-12);
    assert!(
        (actual - expected).abs() <= tolerance,
        "{path}: actual={actual:?} expected={expected:?} diff={:?}",
        (actual - expected).abs()
    );
}

#[test]
fn sensor_rays_and_scene_hits_match_every_coordinate_fixture() {
    let inputs = inputs();
    let boundary = polygon(&inputs);
    let scene = EnvironmentScene::new(
        boundary,
        inputs.environment.floor_z_m,
        inputs.environment.top_z_m,
        Vec::new(),
    )
    .unwrap();
    let surface = deposited_surface(&inputs).surface_snapshot().unwrap();
    let fixture: Value = serde_json::from_str(COORDINATES).unwrap();
    for (index, expected) in fixture["measurements"]
        .as_array()
        .unwrap()
        .iter()
        .enumerate()
    {
        let sensor = inputs
            .environment
            .sensors
            .iter()
            .find(|sensor| sensor.sensor_id == expected["sensor_id"].as_str().unwrap())
            .unwrap();
        let frame = sensor_frame(sensor);
        let angle = expected["angle_deg"].as_f64().unwrap();
        let ray = frame.ray_at(angle).unwrap();
        let direction = &expected["direction"];
        assert_close(
            ray.direction().x(),
            direction["x"].as_f64().unwrap(),
            &format!("measurements[{index}].direction.x"),
        );
        assert_close(
            ray.direction().y(),
            direction["y"].as_f64().unwrap(),
            &format!("measurements[{index}].direction.y"),
        );
        assert_close(
            ray.direction().z(),
            direction["z"].as_f64().unwrap(),
            &format!("measurements[{index}].direction.z"),
        );
        let dynamic = (expected["scene"] == "deposited").then_some(&surface);
        let actual = scene.first_hit(ray, dynamic, 0.05, 30.0).unwrap();
        if expected["hit"].is_null() {
            assert!(actual.is_none(), "unexpected hit at measurements[{index}]");
            continue;
        }
        let actual = actual.unwrap();
        let hit = &expected["hit"];
        let kind = match hit["kind"].as_str().unwrap() {
            "floor" => HitKind::Floor,
            "wall" => HitKind::Wall,
            "surface" => HitKind::Surface,
            other => panic!("unknown hit kind {other}"),
        };
        assert_eq!(actual.kind, kind, "kind at measurements[{index}]");
        assert_close(
            actual.distance_m,
            hit["distance_m"].as_f64().unwrap(),
            &format!("measurements[{index}].distance_m"),
        );
        for (name, value) in [
            ("x", actual.position_m.x()),
            ("y", actual.position_m.y()),
            ("z", actual.position_m.z()),
        ] {
            assert_close(
                value,
                hit["position_m"][name].as_f64().unwrap(),
                &format!("measurements[{index}].position_m.{name}"),
            );
        }
    }
}

#[test]
fn an_empty_surface_at_the_floor_does_not_replace_the_earlier_floor_hit() {
    let inputs = inputs();
    let scene = EnvironmentScene::new(
        polygon(&inputs),
        inputs.environment.floor_z_m,
        inputs.environment.top_z_m,
        Vec::new(),
    )
    .unwrap();
    let surface = HeightField::new(
        polygon(&inputs),
        inputs.environment.floor_z_m,
        inputs.environment.top_z_m,
        inputs.simulator.scenario.surface.cell_size_m,
    )
    .unwrap()
    .surface_snapshot()
    .unwrap();
    let sensor = &inputs.environment.sensors[0];
    let hit = scene
        .first_hit(
            sensor_frame(sensor).ray_at(0.0).unwrap(),
            Some(&surface),
            0.05,
            30.0,
        )
        .unwrap()
        .unwrap();
    assert_eq!(hit.kind, HitKind::Floor);
}

fn schedule(value: &Value) -> ScheduledScan {
    let sensor_id = value["sensor_id"].as_str().unwrap();
    let angles: Vec<_> = value["angles_deg"]
        .as_array()
        .unwrap()
        .iter()
        .map(|value| value.as_f64().unwrap())
        .collect();
    let times: Vec<_> = value["point_elapsed_times_s"]
        .as_array()
        .unwrap()
        .iter()
        .map(|value| value.as_f64().unwrap())
        .collect();
    ScheduledScan::new(
        sensor_id,
        value["scan_id"].as_u64().unwrap(),
        value["rotation_started_at_s"].as_f64().unwrap(),
        value["completed_at_s"].as_f64().unwrap(),
        angles.clone(),
        times,
    )
    .unwrap()
}

fn assert_reference_matches_fixture(actual: &TimedReferenceScan, expected: &Value) {
    for (index, ((point, distance), kind)) in actual
        .scan()
        .points()
        .iter()
        .zip(expected["distances_m"].as_array().unwrap())
        .zip(expected["hit_kinds"].as_array().unwrap())
        .enumerate()
    {
        assert_close(
            point.distance_m,
            distance.as_f64().unwrap(),
            &format!("reference.distance_m[{index}]"),
        );
        assert_eq!(
            point.hit_kind,
            match kind.as_str() {
                Some("floor") => Some(HitKind::Floor),
                Some("wall") => Some(HitKind::Wall),
                Some("surface") => Some(HitKind::Surface),
                None => None,
                Some(other) => panic!("unknown hit kind {other}"),
            }
        );
    }
}

fn point(value: &Value) -> Vec2 {
    Vec2::new(value["x"].as_f64().unwrap(), value["y"].as_f64().unwrap()).unwrap()
}

fn falling_events(value: &Value) -> Vec<FallingMaterialEvent> {
    value
        .as_array()
        .unwrap()
        .iter()
        .map(|event| FallingMaterialEvent {
            cycle_index: event["cycle_index"].as_u64().unwrap(),
            started_at_s: event["started_at_s"].as_f64().unwrap(),
            ends_at_s: event["ends_at_s"].as_f64().unwrap(),
            center: point(&event["center"]),
            radius_m: event["radius_m"].as_f64().unwrap(),
            distance_reduction_m: event["distance_reduction_m"].as_f64().unwrap(),
        })
        .collect()
}

fn collection_events(value: &Value) -> Vec<CollectionOcclusionEvent> {
    value
        .as_array()
        .unwrap()
        .iter()
        .map(|event| CollectionOcclusionEvent {
            cycle_index: event["cycle_index"].as_u64().unwrap(),
            started_at_s: event["started_at_s"].as_f64().unwrap(),
            ends_at_s: event["ends_at_s"].as_f64().unwrap(),
            start_center: point(&event["start_center"]),
            end_center: point(&event["end_center"]),
            radius_m: event["radius_m"].as_f64().unwrap(),
            distance_reduction_m: event["distance_reduction_m"].as_f64().unwrap(),
        })
        .collect()
}

fn void_event(event: &Value) -> VoidEvent {
    VoidEvent {
        cycle_index: event["cycle_index"].as_u64().unwrap(),
        started_at_s: event["started_at_s"].as_f64().unwrap(),
        expires_at_s: event["expires_at_s"].as_f64().unwrap(),
        ends_at_s: event["ends_at_s"].as_f64().unwrap(),
        center: point(&event["center"]),
        surface_height_at_start_m: event["surface_height_at_start_m"].as_f64().unwrap(),
        radius_m: event["radius_m"].as_f64().unwrap(),
        distance_increase_m: event["distance_increase_m"].as_f64().unwrap(),
    }
}

fn assert_vector(actual: &[f64], expected: &Value, path: &str) {
    assert_eq!(actual.len(), expected.as_array().unwrap().len());
    for (index, (actual, expected)) in actual.iter().zip(expected.as_array().unwrap()).enumerate() {
        assert_close(
            *actual,
            expected.as_f64().unwrap(),
            &format!("{path}[{index}]"),
        );
    }
}

#[test]
fn spatial_resolvers_match_half_open_fixture_and_static_protection() {
    let inputs = inputs();
    let fixture: Value = serde_json::from_str(DISTORTIONS).unwrap();
    let static_scene = EnvironmentScene::new(
        polygon(&inputs),
        inputs.environment.floor_z_m,
        inputs.environment.top_z_m,
        Vec::new(),
    )
    .unwrap();
    let surface = deposited_surface(&inputs).surface_snapshot().unwrap();
    for sensor_fixture in fixture["sensors"].as_array().unwrap() {
        let sensor = inputs
            .environment
            .sensors
            .iter()
            .find(|sensor| {
                sensor.sensor_id == sensor_fixture["reference"]["sensor_id"].as_str().unwrap()
            })
            .unwrap();
        let frame = sensor_frame(sensor);
        let mut scanner = ReferenceScanner::new(&sensor.sensor_id, frame, 0.05, 30.0).unwrap();
        let reference = scanner
            .generate(
                &static_scene,
                Some(&surface),
                schedule(&sensor_fixture["reference"]),
            )
            .unwrap();
        assert_reference_matches_fixture(&reference, &sensor_fixture["reference"]);
        let falling = falling_events(&sensor_fixture["falling"]["events"]);
        assert_vector(
            &resolve_falling_material_distances(&reference, &falling, 0.05).unwrap(),
            &sensor_fixture["falling"]["distances_m"],
            "falling",
        );
        let collection = collection_events(&sensor_fixture["collection"]["events"]);
        assert_vector(
            &resolve_collection_occlusion_distances(&reference, &collection, 0.05).unwrap(),
            &sensor_fixture["collection"]["distances_m"],
            "collection",
        );
        let voids: Vec<_> = sensor_fixture["voids"]["events"]
            .as_array()
            .unwrap()
            .iter()
            .map(void_event)
            .collect();
        assert_vector(
            &resolve_void_distances(&reference, &voids, 0.05, 30.0).unwrap(),
            &sensor_fixture["voids"]["distances_m"],
            "voids",
        );
        assert_vector(
            &resolve_void_distances(
                &reference,
                &[void_event(&sensor_fixture["voids"]["blocked_event"])],
                0.05,
                30.0,
            )
            .unwrap(),
            &sensor_fixture["voids"]["blocked_distances_m"],
            "blocked void",
        );
    }
}

#[test]
fn void_candidates_are_independently_blocked_by_static_geometry_and_maximum_range() {
    let boundary = Polygon2::new(vec![
        Vec2::new(-2.0, -2.0).unwrap(),
        Vec2::new(2.0, -2.0).unwrap(),
        Vec2::new(2.0, 2.0).unwrap(),
        Vec2::new(-2.0, 2.0).unwrap(),
    ])
    .unwrap();
    let static_scene = EnvironmentScene::new(boundary.clone(), 0.0, 4.0, Vec::new()).unwrap();
    let mut surface = HeightField::new(boundary.clone(), 0.0, 4.0, 0.25).unwrap();
    surface
        .add_volume(2.0, Vec2::new(0.0, 0.0).unwrap(), 0.75)
        .unwrap();
    let down = SensorFrame::new(
        Vec3::new(0.0, 0.0, 3.0).unwrap(),
        Vec3::new(0.0, 0.0, -1.0).unwrap(),
        Vec3::new(1.0, 0.0, 0.0).unwrap(),
    )
    .unwrap();
    let schedule = ScheduledScan::new("sensor", 1, 0.0, 1.0, vec![0.0], vec![0.5]).unwrap();
    let reference = ReferenceScanner::new("sensor", down, 0.05, 10.0)
        .unwrap()
        .generate(
            &static_scene,
            Some(&surface.surface_snapshot().unwrap()),
            schedule,
        )
        .unwrap();
    let reference_distance = reference.scan().points()[0].distance_m;
    assert_eq!(
        reference.scan().points()[0].hit_kind,
        Some(HitKind::Surface)
    );
    let static_blocked = VoidEvent {
        cycle_index: 0,
        started_at_s: 0.0,
        expires_at_s: 1.0,
        ends_at_s: 1.0,
        center: Vec2::new(0.0, 0.0).unwrap(),
        surface_height_at_start_m: 3.0 - reference_distance,
        radius_m: 0.5,
        distance_increase_m: 3.1 - reference_distance,
    };
    assert_eq!(
        resolve_void_distances(&reference, &[static_blocked], 0.05, 10.0).unwrap(),
        [reference_distance]
    );

    let max_blocked = VoidEvent {
        distance_increase_m: 0.2,
        ..static_blocked
    };
    assert!(
        resolve_void_distances(&reference, &[max_blocked], 0.05, reference_distance + 0.1).is_err()
    );

    let maximum = 2.5;
    let max_reference = ReferenceScanner::new("sensor", down, 0.05, maximum)
        .unwrap()
        .generate(
            &static_scene,
            Some(&surface.surface_snapshot().unwrap()),
            ScheduledScan::new("sensor", 2, 0.0, 1.0, vec![0.0], vec![0.5]).unwrap(),
        )
        .unwrap();
    let max_reference_distance = max_reference.scan().points()[0].distance_m;
    let max_blocked = VoidEvent {
        distance_increase_m: maximum - max_reference_distance + 0.1,
        ..max_blocked
    };
    assert_eq!(
        resolve_void_distances(&max_reference, &[max_blocked], 0.05, maximum).unwrap(),
        [max_reference_distance]
    );

    let fixed = Triangle::new(
        Vec3::new(-0.5, -0.5, 0.5).unwrap(),
        Vec3::new(0.5, -0.5, 0.5).unwrap(),
        Vec3::new(0.0, 0.5, 0.5).unwrap(),
    )
    .unwrap();
    let different_scene = EnvironmentScene::new(boundary, 0.0, 4.0, vec![fixed]).unwrap();
    let mut resolver = SpatialDistortionResolver::new(different_scene);
    resolver.advance_with_events(1.0, [], [], []).unwrap();
    assert!(resolver.resolve_distances(&reference, 0.05, 10.0).is_err());
}

#[test]
fn fixed_surface_hits_do_not_receive_dynamic_surface_distortions() {
    let boundary = Polygon2::new(vec![
        Vec2::new(-1.0, -1.0).unwrap(),
        Vec2::new(1.0, -1.0).unwrap(),
        Vec2::new(1.0, 1.0).unwrap(),
        Vec2::new(-1.0, 1.0).unwrap(),
    ])
    .unwrap();
    let fixed = Triangle::new(
        Vec3::new(-0.5, -0.5, 2.0).unwrap(),
        Vec3::new(0.5, -0.5, 2.0).unwrap(),
        Vec3::new(0.0, 0.5, 2.0).unwrap(),
    )
    .unwrap();
    let scene = EnvironmentScene::new(boundary, 0.0, 4.0, vec![fixed]).unwrap();
    let frame = SensorFrame::new(
        Vec3::new(0.0, 0.0, 3.0).unwrap(),
        Vec3::new(0.0, 0.0, -1.0).unwrap(),
        Vec3::new(1.0, 0.0, 0.0).unwrap(),
    )
    .unwrap();
    let reference = ReferenceScanner::new("sensor", frame, 0.05, 10.0)
        .unwrap()
        .generate(
            &scene,
            None,
            ScheduledScan::new("sensor", 1, 0.0, 1.0, vec![0.0], vec![0.5]).unwrap(),
        )
        .unwrap();
    assert_eq!(
        reference.scan().points()[0].hit_kind,
        Some(HitKind::Surface)
    );
    let event = FallingMaterialEvent {
        cycle_index: 0,
        started_at_s: 0.0,
        ends_at_s: 1.0,
        center: Vec2::new(0.0, 0.0).unwrap(),
        radius_m: 1.0,
        distance_reduction_m: 0.2,
    };
    assert_eq!(
        resolve_falling_material_distances(&reference, &[event], 0.05).unwrap(),
        [1.0]
    );
    assert!(
        resolve_falling_material_distances(
            &reference,
            &[FallingMaterialEvent {
                radius_m: -1.0,
                ..event
            }],
            0.05,
        )
        .is_err()
    );
}

#[test]
fn spatial_history_rejects_scans_older_than_the_earliest_retained_sample() {
    let boundary = Polygon2::new(vec![
        Vec2::new(-1.0, -1.0).unwrap(),
        Vec2::new(1.0, -1.0).unwrap(),
        Vec2::new(1.0, 1.0).unwrap(),
        Vec2::new(-1.0, 1.0).unwrap(),
    ])
    .unwrap();
    let scene = EnvironmentScene::new(boundary.clone(), 0.0, 3.0, Vec::new()).unwrap();
    let mut surface = HeightField::new(boundary, 0.0, 3.0, 0.25).unwrap();
    surface
        .add_volume(1.0, Vec2::new(0.0, 0.0).unwrap(), 0.75)
        .unwrap();
    let snapshot = surface.surface_snapshot().unwrap();
    let frame = SensorFrame::new(
        Vec3::new(0.0, 0.0, 2.5).unwrap(),
        Vec3::new(0.0, 0.0, -1.0).unwrap(),
        Vec3::new(1.0, 0.0, 0.0).unwrap(),
    )
    .unwrap();
    let mut scanner = ReferenceScanner::new("sensor", frame, 0.05, 10.0).unwrap();
    let event = FallingMaterialEvent {
        cycle_index: 0,
        started_at_s: 0.25,
        ends_at_s: 0.75,
        center: Vec2::new(0.0, 0.0).unwrap(),
        radius_m: 1.0,
        distance_reduction_m: 0.1,
    };
    let mut resolver = SpatialDistortionResolver::new(scene.clone());
    resolver.advance_with_events(1.0, [event], [], []).unwrap();
    resolver.discard_before(0.5).unwrap();
    assert_eq!(resolver.retained_from_s(), 0.5);

    let old = scanner
        .generate(
            &scene,
            Some(&snapshot),
            ScheduledScan::new("sensor", 1, 0.0, 0.6, vec![0.0], vec![0.4]).unwrap(),
        )
        .unwrap();
    assert!(resolver.resolve_distances(&old, 0.05, 10.0).is_err());

    let retained = scanner
        .generate(
            &scene,
            Some(&snapshot),
            ScheduledScan::new("sensor", 2, 0.4, 0.7, vec![0.0], vec![0.5]).unwrap(),
        )
        .unwrap();
    assert!(resolver.resolve_distances(&retained, 0.05, 10.0).is_ok());
    assert!(resolver.discard_before(0.4).is_err());
}

#[test]
fn spatial_event_insertion_is_transactional() {
    let boundary = Polygon2::new(vec![
        Vec2::new(-1.0, -1.0).unwrap(),
        Vec2::new(1.0, -1.0).unwrap(),
        Vec2::new(1.0, 1.0).unwrap(),
        Vec2::new(-1.0, 1.0).unwrap(),
    ])
    .unwrap();
    let scene = EnvironmentScene::new(boundary, 0.0, 3.0, Vec::new()).unwrap();
    let valid = FallingMaterialEvent {
        cycle_index: 0,
        started_at_s: 0.25,
        ends_at_s: 0.75,
        center: Vec2::new(0.0, 0.0).unwrap(),
        radius_m: 0.2,
        distance_reduction_m: 0.1,
    };
    let invalid = VoidEvent {
        cycle_index: 0,
        started_at_s: 0.5,
        expires_at_s: 0.4,
        ends_at_s: 0.75,
        center: Vec2::new(0.0, 0.0).unwrap(),
        surface_height_at_start_m: 1.0,
        radius_m: 0.2,
        distance_increase_m: 0.1,
    };
    let mut resolver = SpatialDistortionResolver::new(scene);
    assert!(
        resolver
            .advance_with_events(1.0, [valid], [invalid], [])
            .is_err()
    );
    assert_eq!(resolver.advanced_to_s(), 0.0);
    assert!(resolver.falling_events().is_empty());
    assert!(resolver.void_events().is_empty());
}

#[test]
fn snapshot_event_time_uses_new_state_and_retention_tracks_earliest_pending_sample() {
    let inputs = inputs();
    let mut surface = HeightField::new(
        polygon(&inputs),
        inputs.environment.floor_z_m,
        inputs.environment.top_z_m,
        inputs.simulator.scenario.surface.cell_size_m,
    )
    .unwrap();
    let initial = surface.surface_snapshot().unwrap();
    surface
        .add_volume(
            1.0,
            Vec2::try_from(inputs.simulator.scenario.inlet_positions_xy_m[0]).unwrap(),
            inputs.simulator.scenario.surface.pile_spread_radius_m,
        )
        .unwrap();
    let updated = surface.surface_snapshot().unwrap();
    let expected_updated_volume = updated.volume_m3();
    let mut coordinator = SnapshotEventCoordinator::new(initial);
    coordinator.push_event(0.5, updated).unwrap();
    assert_eq!(
        coordinator.snapshot_at(0.5).unwrap().volume_m3(),
        expected_updated_volume
    );
    assert_eq!(
        coordinator
            .snapshot_at(0.5_f64.next_down())
            .unwrap()
            .volume_m3(),
        0.0
    );
    let independent = HeightField::new(
        polygon(&inputs),
        inputs.environment.floor_z_m,
        inputs.environment.top_z_m,
        inputs.simulator.scenario.surface.cell_size_m,
    )
    .unwrap()
    .surface_snapshot()
    .unwrap();
    assert!(coordinator.push_event(0.75, independent).is_err());
    assert_eq!(coordinator.retained_snapshot_count(), 2);
    coordinator.discard_before_earliest_pending(0.5).unwrap();
    assert_eq!(coordinator.retained_snapshot_count(), 1);
    assert!(coordinator.snapshot_at(0.5_f64.next_down()).is_err());
}

#[test]
fn a_scan_crossing_an_event_uses_old_then_new_surface_without_interpolation() {
    let boundary = Polygon2::new(vec![
        Vec2::new(-1.0, -1.0).unwrap(),
        Vec2::new(1.0, -1.0).unwrap(),
        Vec2::new(1.0, 1.0).unwrap(),
        Vec2::new(-1.0, 1.0).unwrap(),
    ])
    .unwrap();
    let scene = EnvironmentScene::new(boundary.clone(), 0.0, 3.0, Vec::new()).unwrap();
    let mut surface = HeightField::new(boundary, 0.0, 3.0, 0.25).unwrap();
    let mut snapshots = SnapshotEventCoordinator::new(surface.surface_snapshot().unwrap());
    surface
        .add_volume(0.2, Vec2::new(0.0, 0.0).unwrap(), 0.5)
        .unwrap();
    snapshots
        .push_event(0.5, surface.surface_snapshot().unwrap())
        .unwrap();
    let frame = SensorFrame::new(
        Vec3::new(0.0, 0.0, 2.0).unwrap(),
        Vec3::new(0.0, 0.0, -1.0).unwrap(),
        Vec3::new(1.0, 0.0, 0.0).unwrap(),
    )
    .unwrap();
    let mut scanner = ReferenceScanner::new("sensor", frame, 0.05, 30.0).unwrap();
    let schedule = ScheduledScan::new(
        "sensor",
        1,
        0.0,
        1.0,
        vec![0.0, 0.0],
        vec![0.5_f64.next_down(), 0.5],
    )
    .unwrap();
    let result = scanner
        .generate_with_snapshots(&scene, &snapshots, schedule)
        .unwrap();
    assert_eq!(result.scan().points()[0].hit_kind, Some(HitKind::Floor));
    assert_eq!(result.scan().points()[0].distance_m, 2.0);
    assert_eq!(result.scan().points()[1].hit_kind, Some(HitKind::Surface));
    assert!(result.scan().points()[1].distance_m < 2.0);
}

#[test]
fn reference_cache_is_keyed_by_the_actual_hq_angle_not_point_position() {
    let boundary = Polygon2::new(vec![
        Vec2::new(-2.0, -2.0).unwrap(),
        Vec2::new(2.0, -2.0).unwrap(),
        Vec2::new(2.0, 2.0).unwrap(),
        Vec2::new(-2.0, 2.0).unwrap(),
    ])
    .unwrap();
    let scene = EnvironmentScene::new(boundary.clone(), 0.0, 4.0, Vec::new()).unwrap();
    let changed_scene = EnvironmentScene::new(boundary, 0.25, 4.0, Vec::new()).unwrap();
    let frame = SensorFrame::new(
        Vec3::new(0.0, 0.0, 3.0).unwrap(),
        Vec3::new(0.0, 0.0, -1.0).unwrap(),
        Vec3::new(1.0, 0.0, 0.0).unwrap(),
    )
    .unwrap();
    let mut scanner = ReferenceScanner::new("sensor", frame, 0.05, 30.0).unwrap();
    let first = scanner.measure(&scene, None, 0.0).unwrap();
    scanner
        .measure(
            &scene,
            None,
            scrap_monitoring_lidar_simulator::measurement::sdk::HQ_ANGLE_STEP_DEG * 0.25,
        )
        .unwrap();
    assert_eq!(scanner.cached_angle_count(), 1);
    assert_eq!(scanner.cached_static_hit_count(), 1);
    scanner.measure(&scene, None, 90.0).unwrap();
    assert_eq!(scanner.cached_angle_count(), 2);
    assert_eq!(scanner.cached_static_hit_count(), 2);

    let changed = scanner.measure(&changed_scene, None, 0.0).unwrap();
    assert_eq!(first.distance_m, 3.0);
    assert_eq!(changed.distance_m, 2.75);
    assert_eq!(scanner.cached_angle_count(), 2);
    assert_eq!(scanner.cached_static_hit_count(), 1);
}

#[test]
fn deposited_surface_matches_the_fixture_used_by_coordinate_and_distortion_cases() {
    let inputs = inputs();
    let actual = deposited_surface(&inputs);
    let fixture: Value = serde_json::from_str(HEIGHT_FIELD).unwrap();
    let expected = &fixture["operations"][1]["state"]["heights_m"];
    let expected_flat: Vec<_> = expected
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|row| row.as_array().unwrap())
        .map(|value| value.as_f64().unwrap())
        .collect();
    assert_vector(
        actual.heights_m(),
        &Value::Array(expected_flat.into_iter().map(Value::from).collect()),
        "height",
    );
}
