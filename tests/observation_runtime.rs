use std::{path::Path, time::Duration};

use scrap_monitoring_lidar_simulator::{
    configuration::{SimulatorInputs, load_simulator_inputs},
    observation::{
        DEFAULT_OBSERVATION_PORT, ObservationError, ObservationPublisherConfig, ObservationScene,
        ObservationSensor, ObservationStreamHeader, TcpObservationPublisher,
        encode_observation_header_frame,
    },
    scenario::{ScenarioModelSnapshot, build_scenario_simulator},
};
use serde_json::Value;
use tokio::{
    io::{AsyncBufReadExt, BufReader},
    net::TcpListener,
    time::timeout,
};

fn inputs() -> SimulatorInputs {
    load_simulator_inputs(Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/simulator.v2.json"))
        .unwrap()
}

fn snapshot(elapsed_s: f64) -> ScenarioModelSnapshot {
    let mut simulator = build_scenario_simulator(&inputs()).unwrap();
    simulator.advance_to(elapsed_s).unwrap();
    simulator.observation_snapshot().unwrap()
}

fn header() -> ObservationStreamHeader {
    let inputs = inputs();
    ObservationStreamHeader::new(
        inputs.environment.environment_id.clone(),
        "run-a",
        "0".repeat(64),
        inputs.simulator.seed,
        ObservationScene::from_inputs(&inputs).unwrap(),
    )
    .unwrap()
}

fn publisher_config(port: u16) -> ObservationPublisherConfig {
    ObservationPublisherConfig::new("127.0.0.1", port, 1.0, 0.2, 0.2, 0.01, 0.02).unwrap()
}

#[test]
fn public_default_port_and_header_fixture_match_the_v1_contract() {
    assert_eq!(DEFAULT_OBSERVATION_PORT, 17_000);
    let sensor = ObservationSensor::new(
        "sensor-a",
        [0.5, 0.5, 1.0],
        [0.0, 0.0, -1.0],
        [1.0, 0.0, 0.0],
    )
    .unwrap();
    let scene = ObservationScene::new(
        vec![[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]],
        0.0,
        1.0,
        vec![[0.5, 0.5]],
        vec![sensor],
    )
    .unwrap();
    let header = ObservationStreamHeader::new(
        "contract-fixture-v1",
        "fixture-run-a",
        "0".repeat(64),
        42,
        scene,
    )
    .unwrap();
    let actual = String::from_utf8(encode_observation_header_frame(&header).unwrap()).unwrap();
    let expected = include_str!("../contracts/observation/v1/fixtures/observation.v1.jsonl")
        .lines()
        .next()
        .unwrap();
    assert_eq!(actual, format!("{expected}\n"));
}

#[test]
fn oversized_header_is_rejected_before_a_publisher_can_start() {
    let sensor = ObservationSensor::new(
        "x".repeat(1_048_576),
        [0.0, 0.0, 1.0],
        [0.0, 0.0, -1.0],
        [1.0, 0.0, 0.0],
    )
    .unwrap();
    let scene = ObservationScene::new(
        vec![[0.0, 0.0], [1.0, 0.0], [0.0, 1.0]],
        0.0,
        1.0,
        vec![[0.5, 0.5]],
        vec![sensor],
    )
    .unwrap();
    let oversized = ObservationStreamHeader::new("env", "run", "0".repeat(64), 1, scene).unwrap();
    let error = TcpObservationPublisher::new(publisher_config(17_000), oversized)
        .err()
        .expect("oversized header must fail before publisher construction");
    assert!(matches!(error, ObservationError::LineTooLarge { .. }));
}

#[test]
fn simulation_time_cadence_uses_absolute_boundaries_without_catch_up() {
    let publisher = TcpObservationPublisher::new(publisher_config(17_000), header()).unwrap();

    assert!(publisher.is_due(0.1));
    assert!(!publisher.is_due(0.2));
    assert!(publisher.is_due(3.4));
    assert!(!publisher.is_due(3.5));
    assert!(publisher.is_due(4.0));
    assert!(!publisher.is_due(f64::NAN));
    assert!(!publisher.is_due(3.0));
}

#[tokio::test(flavor = "current_thread")]
async fn publisher_keeps_only_the_latest_pending_record_and_streams_header_first() {
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let receiver = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut lines = BufReader::new(stream).lines();
        let header = lines.next_line().await.unwrap().unwrap();
        let observation = lines.next_line().await.unwrap().unwrap();
        (header, observation)
    });

    let mut publisher = TcpObservationPublisher::new(publisher_config(port), header()).unwrap();
    assert!(publisher.publish(snapshot(0.1)).unwrap());
    assert!(publisher.publish(snapshot(1.0)).unwrap());
    publisher.start().unwrap();
    let (header_line, observation_line) = timeout(Duration::from_secs(1), receiver)
        .await
        .unwrap()
        .unwrap();
    publisher.close().await;

    let header: Value = serde_json::from_str(&header_line).unwrap();
    let observation: Value = serde_json::from_str(&observation_line).unwrap();
    assert_eq!(header["type"], "load_model_stream_header");
    assert_eq!(observation["type"], "load_model_observation");
    assert_eq!(observation["sequence"], 2);
    assert_eq!(observation["scenario"]["elapsed_s"], 1.0);
    assert_eq!(
        observation["surface"]["heights_m"]
            .as_array()
            .unwrap()
            .len(),
        23
    );
    assert_eq!(
        publisher.stats(),
        scrap_monitoring_lidar_simulator::observation::ObservationPublisherStats {
            accepted_records: 2,
            sent_records: 1,
            dropped_records: 1,
            connection_failures: 0,
        }
    );
}

#[tokio::test(flavor = "current_thread")]
async fn connection_failure_preserves_latest_until_prompt_close_discards_it() {
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    let mut publisher = TcpObservationPublisher::new(publisher_config(port), header()).unwrap();
    publisher.start().unwrap();
    assert!(publisher.publish(snapshot(0.1)).unwrap());

    timeout(Duration::from_secs(1), async {
        while publisher.stats().connection_failures == 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    timeout(Duration::from_millis(100), publisher.close())
        .await
        .unwrap();

    assert_eq!(publisher.stats().sent_records, 0);
    assert_eq!(publisher.stats().dropped_records, 1);
    assert_eq!(
        publisher.stats().accepted_records,
        publisher.stats().sent_records + publisher.stats().dropped_records
    );
}

#[tokio::test(flavor = "current_thread")]
async fn invalid_snapshot_is_accepted_then_dropped_inside_the_isolated_worker() {
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let (header_seen_sender, header_seen) = tokio::sync::oneshot::channel();
    let (release_sender, release) = tokio::sync::oneshot::channel();
    let receiver = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut lines = BufReader::new(stream).lines();
        assert!(lines.next_line().await.unwrap().is_some());
        let _ = header_seen_sender.send(());
        let _ = release.await;
    });

    let mut publisher = TcpObservationPublisher::new(publisher_config(port), header()).unwrap();
    publisher.start().unwrap();
    let mut invalid = snapshot(0.1);
    invalid.state.surface_fill_ratio = f64::NAN;
    assert!(publisher.publish(invalid).unwrap());
    timeout(Duration::from_secs(1), header_seen)
        .await
        .unwrap()
        .unwrap();
    timeout(Duration::from_secs(1), async {
        while publisher.stats().dropped_records == 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(publisher.stats().accepted_records, 1);
    assert_eq!(publisher.stats().sent_records, 0);
    assert_eq!(publisher.stats().connection_failures, 0);

    publisher.close().await;
    let _ = release_sender.send(());
    receiver.await.unwrap();
}
