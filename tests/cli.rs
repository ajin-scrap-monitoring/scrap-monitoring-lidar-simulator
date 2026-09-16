use std::{path::Path, process::Command};

use scrap_monitoring_lidar_simulator::cli::{
    self, Environment, RuntimeSettingOverrides, resolve_model_overrides, resolve_runtime_settings,
};

fn environment() -> Environment {
    [
        (
            cli::CONFIG_ENV,
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("examples/simulator.v2.json")
                .display()
                .to_string(),
        ),
        (cli::SOCKET_DIR_ENV, "/run/lidar".into()),
        (cli::STATUS_DIR_ENV, "/status".into()),
        ("SITE_ID", "synthetic-site".into()),
        ("EDGE_ID", "synthetic-edge".into()),
        ("CONFIG_REVISION", "synthetic-r1".into()),
        ("DEPLOYMENT_REVISION", "synthetic-deployment-r1".into()),
        (cli::OBSERVATION_HOST_ENV, "visualizer.example".into()),
        (cli::OBSERVATION_PORT_ENV, "17000".into()),
    ]
    .into_iter()
    .map(|(key, value)| (key.to_owned(), value))
    .collect()
}

#[test]
fn runtime_requires_deployment_identity_and_observation_endpoint() {
    let environment = environment();
    let settings =
        resolve_runtime_settings(&environment, &RuntimeSettingOverrides::default()).unwrap();
    assert_eq!(settings.observation_interval_s, 1.0);
    assert_eq!(settings.observation_port, 17000);
    for name in environment.keys() {
        let mut missing = environment.clone();
        missing.remove(name);
        let error =
            resolve_runtime_settings(&missing, &RuntimeSettingOverrides::default()).unwrap_err();
        assert_eq!(&error.path, name);
    }
}

#[test]
fn cli_overrides_invalid_environment_before_parsing() {
    let mut environment = environment();
    environment.insert(cli::OBSERVATION_PORT_ENV.into(), "invalid".into());
    environment.insert(cli::MEAN_FILL_DURATION_ENV.into(), "invalid".into());
    let overrides = RuntimeSettingOverrides {
        observation_port: Some("18000".into()),
        mean_fill_duration_s: Some("600".into()),
        ..Default::default()
    };
    let settings = resolve_runtime_settings(&environment, &overrides).unwrap();
    assert_eq!(settings.observation_port, 18000);
    assert_eq!(settings.model.mean_fill_duration_s, Some(600.0));
}

#[test]
fn model_override_precedence_and_relative_diagnostics_match_the_contract() {
    let mut environment = environment();
    let baseline = cli::load_overridden_inputs(
        &resolve_model_overrides(&environment, &RuntimeSettingOverrides::default()).unwrap(),
    )
    .unwrap();
    environment.insert(cli::MEAN_FILL_DURATION_ENV.into(), "1200".into());
    environment.insert(cli::COLLECTION_THRESHOLD_ENV.into(), "0.8".into());
    environment.insert(cli::DIAGNOSTICS_ENABLED_ENV.into(), "true".into());
    let overrides = RuntimeSettingOverrides {
        mean_fill_duration_s: Some("600".into()),
        diagnostics_enabled: Some("false".into()),
        diagnostics_output_path: Some("relative".into()),
        ..Default::default()
    };
    let settings = resolve_model_overrides(&environment, &overrides).unwrap();
    let inputs = cli::load_overridden_inputs(&settings).unwrap();
    assert_eq!(inputs.simulator.scenario.mean_fill_duration_s, 600.0);
    assert_eq!(
        inputs.simulator.scenario.collection_threshold_range,
        [0.75, 0.8500000000000001]
    );
    assert!(!inputs.simulator.diagnostics.enabled);
    assert_eq!(
        inputs.simulator.diagnostics.output_path,
        settings.config_path.parent().unwrap().join("relative")
    );
    assert_eq!(inputs.environment, baseline.environment);
    assert_eq!(inputs.quality_profile, baseline.quality_profile);
    assert_eq!(inputs.simulator.measurement, baseline.simulator.measurement);
}

#[test]
fn deployment_values_enforce_existing_bounds() {
    for (name, value) in [
        (cli::SOCKET_DIR_ENV, "relative"),
        (cli::STATUS_DIR_ENV, ""),
        ("SITE_ID", "bad/name"),
        ("EDGE_ID", "-edge"),
        (cli::OBSERVATION_HOST_ENV, ""),
        (cli::OBSERVATION_PORT_ENV, "0"),
        (cli::OBSERVATION_PORT_ENV, "65536"),
        (cli::OBSERVATION_INTERVAL_ENV, "86401"),
        (cli::OBSERVATION_INTERVAL_ENV, "NaN"),
        (cli::MEAN_FILL_DURATION_ENV, "inf"),
        (cli::DIAGNOSTICS_ENABLED_ENV, "False"),
        (cli::COLLECTION_THRESHOLD_ENV, "0.05"),
        (cli::COLLECTION_THRESHOLD_ENV, "0.951"),
    ] {
        let mut environment = environment();
        environment.insert(name.into(), value.into());
        let error = resolve_runtime_settings(&environment, &RuntimeSettingOverrides::default())
            .unwrap_err();
        assert_eq!(error.path, name, "{error}");
    }
    for (name, length) in [
        ("EDGE_ID", 65),
        ("CONFIG_REVISION", 65),
        ("SITE_ID", 129),
        ("DEPLOYMENT_REVISION", 129),
    ] {
        let mut environment = environment();
        environment.insert(name.into(), "a".repeat(length));
        assert!(
            resolve_runtime_settings(&environment, &RuntimeSettingOverrides::default()).is_err()
        );
    }
}

#[test]
fn numeric_overrides_accept_digit_separators() {
    let mut environment = environment();
    environment.insert(cli::OBSERVATION_PORT_ENV.into(), " +17_000 ".into());
    environment.insert(
        cli::MEAN_FILL_DURATION_ENV.into(),
        " 6_0_0.0_0e+0_0 ".into(),
    );
    let settings =
        resolve_runtime_settings(&environment, &RuntimeSettingOverrides::default()).unwrap();
    assert_eq!(settings.observation_port, 17000);
    assert_eq!(settings.model.mean_fill_duration_s, Some(600.0));
    environment.insert(
        cli::OBSERVATION_PORT_ENV.into(),
        "\u{ff11}\u{0667}_000".into(),
    );
    environment.insert(
        cli::MEAN_FILL_DURATION_ENV.into(),
        "\u{ff16}\u{0660}_\u{0660}".into(),
    );
    let settings =
        resolve_runtime_settings(&environment, &RuntimeSettingOverrides::default()).unwrap();
    assert_eq!(settings.observation_port, 17000);
    assert_eq!(settings.model.mean_fill_duration_s, Some(600.0));
    for value in ["_600", "600_", "6__00", "6_.0", "6e_2", "6_e2", "\u{00b2}"] {
        environment.insert(cli::MEAN_FILL_DURATION_ENV.into(), value.into());
        assert!(
            resolve_runtime_settings(&environment, &RuntimeSettingOverrides::default()).is_err(),
            "accepted {value}"
        );
    }
}

#[test]
fn check_cli_has_success_error_and_help_exit_codes() {
    let binary = env!("CARGO_BIN_EXE_scrap-monitoring-lidar-simulator");
    let config = Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/simulator.v2.json");
    let result = Command::new(binary)
        .env_clear()
        .args(["check", "--config", "/missing-first-value", "--config"])
        .arg(config)
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(String::from_utf8_lossy(&result.stdout).contains("validation=passed sensors=2"));
    let missing = Command::new(binary)
        .env_clear()
        .arg("check")
        .output()
        .unwrap();
    assert_eq!(missing.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&missing.stderr).contains("configuration error:"));
    let help = Command::new(binary)
        .env_clear()
        .arg("--help")
        .output()
        .unwrap();
    assert!(help.status.success());
    let runtime = Command::new(binary)
        .env_clear()
        .envs(environment())
        .args(["check", "--runtime"])
        .output()
        .unwrap();
    assert!(
        runtime.status.success(),
        "{}",
        String::from_utf8_lossy(&runtime.stderr)
    );
    let run_without_settings = Command::new(binary)
        .env_clear()
        .arg("run")
        .output()
        .unwrap();
    assert_eq!(run_without_settings.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&run_without_settings.stderr).contains("configuration error:"));
}

#[test]
fn repeated_cli_options_validate_every_occurrence_before_using_the_last() {
    let binary = env!("CARGO_BIN_EXE_scrap-monitoring-lidar-simulator");
    let config = Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/simulator.v2.json");
    let result = Command::new(binary)
        .env_clear()
        .arg("check")
        .arg("--config")
        .arg(config)
        .args([
            "--mean-fill-duration-s",
            "not-a-number",
            "--mean-fill-duration-s",
            "10",
        ])
        .output()
        .unwrap();
    assert_eq!(result.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&result.stderr).contains("must be a number"));
}
