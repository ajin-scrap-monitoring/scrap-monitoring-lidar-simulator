use std::path::Path;

use scrap_monitoring_lidar_simulator::{
    configuration::load_simulator_inputs, runtime::GenerationRuntime, scenario::ScenarioPhase,
};

fn inputs() -> scrap_monitoring_lidar_simulator::configuration::SimulatorInputs {
    load_simulator_inputs(Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/simulator.v2.json"))
        .unwrap()
}

#[test]
fn two_workers_share_one_time_ordered_model_and_generate_both_sensors() {
    let mut runtime = GenerationRuntime::from_inputs(&inputs()).unwrap();
    assert_eq!(
        runtime.sensor_ids().collect::<Vec<_>>(),
        ["lidar_1", "lidar_2"]
    );
    assert_eq!(runtime.elapsed_s(), 0.0);
    assert_eq!(runtime.next_completion_elapsed_s(), 0.1);

    let first = runtime.next_completed_scans().unwrap();
    assert_eq!(first.completed_at_s, 0.1);
    assert_eq!(
        first
            .scans
            .iter()
            .map(|scan| scan.sensor_id())
            .collect::<Vec<_>>(),
        ["lidar_1", "lidar_2"]
    );
    assert!(first.scans.iter().all(|scan| scan.scan_id() == 1));
    assert_eq!(runtime.stats().completed_batches, 1);
    assert_eq!(runtime.stats().generated_scans, 2);

    for _ in 1..10 {
        runtime.next_completed_scans().unwrap();
    }
    let snapshot = runtime.model_snapshot().unwrap();
    assert_eq!(snapshot.state.elapsed_s, 1.0);
    assert_eq!(snapshot.state.surface_updated_at_s, 1.0);
    assert!(snapshot.state.surface_volume_m3 > 0.0);
}

#[test]
fn worker_scheduling_is_deterministic_across_independent_runtimes() {
    let inputs = inputs();
    let mut first = GenerationRuntime::from_inputs(&inputs).unwrap();
    let mut second = GenerationRuntime::from_inputs(&inputs).unwrap();

    for _ in 0..12 {
        let left = first.next_completed_scans().unwrap();
        let right = second.next_completed_scans().unwrap();
        assert_eq!(
            left.completed_at_s.to_bits(),
            right.completed_at_s.to_bits()
        );
        assert_eq!(left.scans, right.scans);
        assert_eq!(
            first.model_snapshot().unwrap().state,
            second.model_snapshot().unwrap().state
        );
        assert_eq!(
            first.model_snapshot().unwrap().surface.heights_m(),
            second.model_snapshot().unwrap().surface.heights_m()
        );
    }
}

#[test]
fn generation_batches_expose_every_phase_and_cycle_transition_without_a_surface() {
    let mut inputs = inputs();
    inputs.simulator.scenario.mean_fill_duration_s = 3.0;
    inputs.simulator.measurement.sample_rate_hz = 100.0;
    inputs.simulator.measurement.rotation_rate_hz = 10.0;
    inputs
        .simulator
        .measurement
        .distortions
        .falling_material
        .enabled = false;
    inputs.simulator.measurement.distortions.voids.enabled = false;
    inputs
        .simulator
        .measurement
        .distortions
        .collection_occlusion
        .enabled = false;
    inputs
        .simulator
        .measurement
        .distortions
        .reflection_error
        .enabled = false;
    inputs.simulator.measurement.distortions.dropout.enabled = false;
    let mut runtime = GenerationRuntime::from_inputs(&inputs).unwrap();
    let mut transitions = Vec::new();

    for _ in 0..50 {
        transitions.extend(runtime.next_completed_scans().unwrap().scenario_transitions);
        if transitions.last().is_some_and(|transition| {
            transition.phase == ScenarioPhase::Filling && transition.cycle_index == 1
        }) {
            break;
        }
    }

    assert!(transitions.len() >= 2);
    assert_eq!(transitions[0].phase, ScenarioPhase::Collecting);
    assert_eq!(transitions[0].cycle_index, 0);
    assert_eq!(transitions[1].phase, ScenarioPhase::Filling);
    assert_eq!(transitions[1].cycle_index, 1);
    assert!(transitions[0].elapsed_s < transitions[1].elapsed_s);
}
