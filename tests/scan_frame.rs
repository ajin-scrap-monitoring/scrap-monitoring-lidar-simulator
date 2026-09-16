use std::{
    cell::Cell,
    collections::BTreeMap,
    rc::Rc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use prost::Message;
use scrap_monitoring_lidar_simulator::{
    configuration::load_simulator_inputs,
    measurement::{
        HitKind, MeasuredScan, MeasurementResult, ReferencePoint, ReferenceScan, ScanFrameFactory,
        ScheduledScan, SystemScanFrameFactory, TimedReferenceScan, sdk,
    },
    wire::ScanFrame,
};
use serde_json::Value;

const FIXTURE: &str = include_str!("fixtures/model-v1/scan-frames.json");

fn timed_reference(value: &Value) -> TimedReferenceScan {
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
    let schedule = ScheduledScan::new(
        sensor_id,
        value["scan_id"].as_u64().unwrap(),
        value["rotation_started_at_s"].as_f64().unwrap(),
        value["completed_at_s"].as_f64().unwrap(),
        angles.clone(),
        times,
    )
    .unwrap();
    let points = angles
        .into_iter()
        .zip(value["distances_m"].as_array().unwrap())
        .zip(value["hit_kinds"].as_array().unwrap())
        .map(|((angle, distance), kind)| {
            let kind = match kind.as_str() {
                Some("floor") => Some(HitKind::Floor),
                Some("wall") => Some(HitKind::Wall),
                Some("surface") => Some(HitKind::Surface),
                None => None,
                Some(other) => panic!("unknown hit kind {other}"),
            };
            ReferencePoint::new(angle, distance.as_f64().unwrap(), kind).unwrap()
        })
        .collect();
    TimedReferenceScan::new(schedule, ReferenceScan::new(sensor_id, points).unwrap()).unwrap()
}

fn fixed_result(reference: TimedReferenceScan) -> MeasurementResult {
    let inputs = load_simulator_inputs("examples/simulator.v2.json").unwrap();
    let profile = inputs
        .quality_profile
        .sensors
        .iter()
        .find(|profile| profile.sensor_id == reference.sensor_id())
        .unwrap();
    let valid = profile
        .valid_distance_frequencies
        .iter()
        .position(|frequency| *frequency > 0)
        .unwrap() as u8;
    let invalid = profile
        .invalid_distance_frequencies
        .iter()
        .position(|frequency| *frequency > 0)
        .unwrap() as u8;
    let distances = reference
        .scan()
        .points()
        .iter()
        .map(|point| sdk::quantize_hq_distance_m(point.distance_m).unwrap())
        .collect::<Vec<_>>();
    let qualities = distances
        .iter()
        .map(|distance| if *distance > 0.0 { valid } else { invalid })
        .collect();
    let measured = MeasuredScan::new(
        reference.sensor_id(),
        reference.schedule().angles_deg().to_vec(),
        distances,
        qualities,
    )
    .unwrap();
    MeasurementResult::new(reference, measured).unwrap()
}

fn decode_hex(value: &str) -> Vec<u8> {
    value
        .as_bytes()
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| {
            let text = std::str::from_utf8(pair).unwrap();
            u8::from_str_radix(text, 16).unwrap()
        })
        .collect()
}

fn assert_frame(actual: &ScanFrame, expected: &Value) {
    assert_eq!(actual.schema_version, expected["schema_version"]);
    assert_eq!(actual.edge_id, expected["edge_id"]);
    assert_eq!(actual.sensor_id, expected["sensor_id"]);
    assert_eq!(actual.sequence, expected["sequence"].as_u64().unwrap());
    assert_eq!(
        actual.acquired_at_unix_ms,
        expected["acquired_at_unix_ms"].as_i64().unwrap()
    );
    assert_eq!(
        actual.acquired_monotonic_ns,
        expected["acquired_monotonic_ns"].as_u64().unwrap()
    );
    assert_eq!(actual.sdk_status, expected["sdk_status"]);
    assert_eq!(actual.scan_hz, expected["scan_hz"].as_f64().unwrap());
    assert_eq!(actual.instance_id, expected["instance_id"]);
    assert_eq!(actual.config_revision, expected["config_revision"]);
    for (actual, expected) in actual
        .samples
        .iter()
        .zip(expected["samples"].as_array().unwrap())
    {
        assert_eq!(
            u64::from(actual.angle_mdeg),
            expected["angle_mdeg"].as_u64().unwrap()
        );
        assert_eq!(
            u64::from(actual.distance_mm),
            expected["distance_mm"].as_u64().unwrap()
        );
        assert_eq!(
            u64::from(actual.quality),
            expected["quality"].as_u64().unwrap()
        );
    }
}

#[test]
fn frame_factory_reproduces_all_fixture_records_and_protobuf_bytes() {
    let fixture: Value = serde_json::from_str(FIXTURE).unwrap();
    let sensor_ids = vec!["lidar_1".to_owned(), "lidar_2".to_owned()];
    let instances = BTreeMap::from([
        ("lidar_1".to_owned(), "parity-lidar_1".to_owned()),
        ("lidar_2".to_owned(), "parity-lidar_2".to_owned()),
    ]);
    let mut monotonic = (0_u64..4).flat_map(|rotation| {
        (0_u64..2).map(move |sensor| 1_000_000_000 + rotation * 250_000_000 + sensor)
    });
    let mut factory = ScanFrameFactory::new(
        sensor_ids,
        "synthetic-edge",
        "parity-v1",
        instances,
        move || Ok(monotonic.next().unwrap()),
        || Ok(1_800_000_000_000),
    )
    .unwrap();
    for record in fixture["records"].as_array().unwrap() {
        let result = fixed_result(timed_reference(&record["reference"]));
        let actual = factory.build(result).unwrap();
        if record["frame"].is_null() {
            assert!(actual.is_none());
            continue;
        }
        let actual = actual.unwrap();
        assert_frame(&actual, &record["frame"]);
        assert_eq!(
            actual.encode_to_vec(),
            decode_hex(record["protobuf_hex"].as_str().unwrap())
        );
    }
}

#[test]
fn normalized_samples_use_wide_integer_math_and_stable_angle_sort() {
    let fixture: Value = serde_json::from_str(FIXTURE).unwrap();
    let case = &fixture["stable_sort_case"];
    let angles: Vec<_> = case["input_angles_deg"]
        .as_array()
        .unwrap()
        .iter()
        .map(|value| value.as_f64().unwrap())
        .collect();
    let distances: Vec<_> = case["input_distances_m"]
        .as_array()
        .unwrap()
        .iter()
        .map(|value| value.as_f64().unwrap())
        .collect();
    let qualities: Vec<_> = case["input_quality_bytes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|value| value.as_u64().unwrap() as u8)
        .collect();
    let schedule = ScheduledScan::new(
        "lidar_1",
        1,
        0.0,
        0.1,
        angles.clone(),
        vec![0.0, 0.02, 0.04, 0.06, 0.08],
    )
    .unwrap();
    let points = angles
        .iter()
        .zip(&distances)
        .map(|(&angle, &distance)| {
            ReferencePoint::new(angle, distance, (distance > 0.0).then_some(HitKind::Wall)).unwrap()
        })
        .collect();
    let reference =
        TimedReferenceScan::new(schedule, ReferenceScan::new("lidar_1", points).unwrap()).unwrap();
    let measured = MeasuredScan::new("lidar_1", angles, distances, qualities).unwrap();
    let result = MeasurementResult::new(reference, measured).unwrap();
    let times = Rc::new(Cell::new(1_000_000_000_u64));
    let times_for_clock = Rc::clone(&times);
    let mut factory = ScanFrameFactory::new(
        ["lidar_1".to_owned()],
        "synthetic-edge",
        "parity-v1",
        BTreeMap::from([("lidar_1".to_owned(), "parity-lidar_1".to_owned())]),
        move || {
            let value = times_for_clock.get();
            times_for_clock.set(value + 100_000_000);
            Ok(value)
        },
        || Ok(1_800_000_000_000),
    )
    .unwrap();
    assert!(factory.build(result.clone()).unwrap().is_none());
    let actual = factory.build(result).unwrap().unwrap();
    assert_frame(&actual, &case["frame"]);
    assert_eq!(
        actual.encode_to_vec(),
        decode_hex(case["protobuf_hex"].as_str().unwrap())
    );
}

#[test]
fn normalization_finishes_before_clocks_are_read() {
    let schedule = ScheduledScan::new("lidar_1", 1, 0.0, 1.0, vec![0.0], vec![0.0]).unwrap();
    let reference = TimedReferenceScan::new(
        schedule,
        ReferenceScan::new(
            "lidar_1",
            vec![ReferencePoint::new(0.0, 1.0, Some(HitKind::Floor)).unwrap()],
        )
        .unwrap(),
    )
    .unwrap();
    let measured = MeasuredScan::from_hq_samples(
        "lidar_1",
        vec![scrap_monitoring_lidar_simulator::measurement::HqSample {
            angle_z_q14: 0,
            dist_mm_q2: u64::MAX,
            quality: 0,
        }],
    )
    .unwrap();
    let result = MeasurementResult::new(reference, measured).unwrap();
    let calls = Rc::new(Cell::new(0));
    let monotonic_calls = Rc::clone(&calls);
    let unix_calls = Rc::clone(&calls);
    let mut factory = ScanFrameFactory::new(
        ["lidar_1".to_owned()],
        "edge",
        "r1",
        BTreeMap::from([("lidar_1".to_owned(), "instance".to_owned())]),
        move || {
            monotonic_calls.set(monotonic_calls.get() + 1);
            Ok(1)
        },
        move || {
            unix_calls.set(unix_calls.get() + 1);
            Ok(1)
        },
    )
    .unwrap();
    assert!(factory.build(result).is_err());
    assert_eq!(calls.get(), 0);
}

#[test]
fn frame_factory_rejects_duplicate_sensor_identifiers() {
    let result = ScanFrameFactory::new(
        ["lidar_1".to_owned(), "lidar_1".to_owned()],
        "edge",
        "r1",
        BTreeMap::from([("lidar_1".to_owned(), "instance".to_owned())]),
        || Ok(1),
        || Ok(1),
    );
    assert!(result.is_err());
}

#[test]
fn a_clock_regression_does_not_poison_the_previous_completion() {
    let fixture: Value = serde_json::from_str(FIXTURE).unwrap();
    let result = fixed_result(timed_reference(&fixture["records"][0]["reference"]));
    let mut times = [100_u64, 90, 110].into_iter();
    let mut factory = ScanFrameFactory::new(
        ["lidar_1".to_owned()],
        "edge",
        "r1",
        BTreeMap::from([("lidar_1".to_owned(), "instance".to_owned())]),
        move || Ok(times.next().unwrap()),
        || Ok(1),
    )
    .unwrap();
    assert!(factory.build(result.clone()).unwrap().is_none());
    assert!(factory.build(result.clone()).is_err());
    let frame = factory.build(result).unwrap().unwrap();
    assert_eq!(frame.sequence, 1);
    assert_eq!(frame.scan_hz, 100_000_000.0);
}

#[test]
fn system_factory_uses_host_monotonic_epoch_and_system_wall_clock() {
    fn monotonic_ns() -> u64 {
        let value = rustix::time::clock_gettime(rustix::time::ClockId::Monotonic);
        u64::try_from(value.tv_sec).unwrap() * 1_000_000_000 + u64::try_from(value.tv_nsec).unwrap()
    }

    let fixture: Value = serde_json::from_str(FIXTURE).unwrap();
    let result = fixed_result(timed_reference(&fixture["records"][0]["reference"]));
    let mut factory = SystemScanFrameFactory::with_system_clocks(
        ["lidar_1".to_owned()],
        "edge",
        "r1",
        BTreeMap::from([("lidar_1".to_owned(), "instance".to_owned())]),
    )
    .unwrap();
    let before_monotonic = monotonic_ns();
    assert!(factory.build(result.clone()).unwrap().is_none());
    std::thread::sleep(Duration::from_millis(1));
    let frame = factory.build(result).unwrap().unwrap();
    let after_monotonic = monotonic_ns();
    let wall_ms = i64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis(),
    )
    .unwrap();
    assert!((before_monotonic..=after_monotonic).contains(&frame.acquired_monotonic_ns));
    assert!((wall_ms - 1_000..=wall_ms).contains(&frame.acquired_at_unix_ms));
}
