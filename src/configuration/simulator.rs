use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
};

use serde_json::Value;

use super::{
    compensated_sum, load_environment, load_quality_profile,
    models::*,
    polygon,
    strict_json::{self as json, Object},
};
use crate::{
    MAX_DIAGNOSTIC_SCANS_PER_SENSOR, MAX_INLET_POSITIONS,
    error::{ConfigurationError, ErrorKind, Result},
    measurement::validate_scan_point_limit,
};

pub fn load_simulator_config(path: impl AsRef<Path>) -> Result<SimulatorConfig> {
    let source = path.as_ref();
    parse_simulator_config(
        &json::read_document(source, "simulator configuration")?,
        source.parent().unwrap_or(Path::new(".")),
    )
}

pub fn parse_simulator_config(
    document: &str,
    base_directory: impl AsRef<Path>,
) -> Result<SimulatorConfig> {
    let value = json::parse_document(document)?;
    let root = Object::new(
        &value,
        "$",
        &[
            "config_version",
            "seed",
            "environment_path",
            "quality_profile_path",
            "scenario",
            "measurement",
            "observation_transport",
            "diagnostics",
        ],
    )?;
    json::version(root.get("config_version"), 2, "$.config_version")?;
    let base = base_directory.as_ref();
    let scenario = scenario(root.get("scenario"))?;
    let measurement = measurement(root.get("measurement"))?;
    let observation_transport = transport(root.get("observation_transport"))?;
    let diagnostics = Object::new(
        root.get("diagnostics"),
        "$.diagnostics",
        &["enabled", "output_path", "sample_scan_limit_per_sensor"],
    )?;
    let diagnostics = DiagnosticsConfig {
        enabled: diagnostics.boolean("enabled")?,
        output_path: resolve_path(&diagnostics, "output_path", base)?,
        sample_scan_limit_per_sensor: SampleScanLimit::from_validated(json::integer(
            diagnostics.get("sample_scan_limit_per_sensor"),
            &diagnostics.at("sample_scan_limit_per_sensor"),
            0,
            MAX_DIAGNOSTIC_SCANS_PER_SENSOR,
        )?),
    };
    Ok(SimulatorConfig {
        seed: json::integer(root.get("seed"), "$.seed", 0, u64::MAX)?,
        environment_path: resolve_path(&root, "environment_path", base)?,
        quality_profile_path: resolve_path(&root, "quality_profile_path", base)?,
        scenario,
        measurement,
        observation_transport,
        diagnostics,
    })
}

pub fn load_simulator_inputs(path: impl AsRef<Path>) -> Result<SimulatorInputs> {
    let simulator = load_simulator_config(path)?;
    let environment = load_environment(&simulator.environment_path)?;
    let quality_profile = load_quality_profile(&simulator.quality_profile_path)?;
    let inputs = SimulatorInputs {
        simulator,
        environment,
        quality_profile,
    };
    validate_inputs(&inputs)?;
    Ok(inputs)
}

pub fn validate_inputs(inputs: &SimulatorInputs) -> Result<()> {
    if inputs.environment.sensors.len() != 2 {
        return Err(ConfigurationError::new(
            ErrorKind::CrossInput,
            "$.sensors",
            "simulator inputs must contain exactly 2 environment sensors",
        ));
    }
    let environment_ids: BTreeSet<_> = inputs
        .environment
        .sensors
        .iter()
        .map(|sensor| &sensor.sensor_id)
        .collect();
    let quality_ids: BTreeSet<_> = inputs
        .quality_profile
        .sensors
        .iter()
        .map(|sensor| &sensor.sensor_id)
        .collect();
    if environment_ids != quality_ids {
        return Err(ConfigurationError::new(
            ErrorKind::CrossInput,
            "$.sensors",
            "quality profile sensor_id values must exactly match environment sensors",
        ));
    }
    for inlet in &inputs.simulator.scenario.inlet_positions_xy_m {
        if !polygon::contains(&inputs.environment.boundary_xy_m, *inlet)? {
            return Err(ConfigurationError::new(
                ErrorKind::CrossInput,
                "$.scenario.inlet_positions_xy_m",
                "scenario inlet positions must lie inside the environment boundary",
            ));
        }
    }
    Ok(())
}

fn resolve_path(object: &Object<'_>, key: &str, base: &Path) -> Result<PathBuf> {
    let path = PathBuf::from(object.string(key)?);
    Ok(if path.is_absolute() {
        path
    } else {
        base.join(path)
    })
}

fn scenario(value: &Value) -> Result<ScenarioConfig> {
    let config = Object::new(
        value,
        "$.scenario",
        &[
            "mean_fill_duration_s",
            "fill_duration_factor_range",
            "fill_rate_factor_range",
            "fill_rate_change_duration_s_range",
            "collection_threshold_range",
            "collection_duration_factor_range",
            "collection_rate_factor_range",
            "collection_rate_change_duration_s_range",
            "inlet_positions_xy_m",
            "inlet_switch_activation_ratio",
            "inlet_switch_height_difference_m",
            "inlet_comparison_radius_m",
            "surface",
        ],
    )?;
    let inlet_values = config.array("inlet_positions_xy_m")?;
    if inlet_values.len() > MAX_INLET_POSITIONS {
        return Err(ConfigurationError::new(
            ErrorKind::Range,
            config.at("inlet_positions_xy_m"),
            format!("must contain at most {MAX_INLET_POSITIONS} coordinates"),
        ));
    }
    let inlets = inlet_values
        .iter()
        .enumerate()
        .map(|(index, value)| {
            json::coordinate(value, &format!("$.scenario.inlet_positions_xy_m[{index}]"))
        })
        .collect::<Result<Vec<Coordinate2>>>()?;
    if inlets.is_empty() {
        return Err(ConfigurationError::new(
            ErrorKind::Range,
            config.at("inlet_positions_xy_m"),
            "must contain at least 1 coordinate",
        ));
    }
    for (index, inlet) in inlets.iter().enumerate() {
        if inlets[..index].contains(inlet) {
            return Err(ConfigurationError::new(
                ErrorKind::Range,
                config.at("inlet_positions_xy_m"),
                "must contain unique coordinates",
            ));
        }
    }
    let fill_duration_factor_range = config.range("fill_duration_factor_range", json::positive)?;
    if (compensated_sum(fill_duration_factor_range) - 2.0).abs() > 1e-12 {
        return Err(ConfigurationError::new(
            ErrorKind::Range,
            config.at("fill_duration_factor_range"),
            "must be centered on 1",
        ));
    }
    let fill_rate_factor_range = average_range(&config, "fill_rate_factor_range")?;
    let collection_rate_factor_range = average_range(&config, "collection_rate_factor_range")?;
    Ok(ScenarioConfig {
        mean_fill_duration_s: config.positive("mean_fill_duration_s")?,
        fill_duration_factor_range,
        fill_rate_factor_range,
        fill_rate_change_duration_s_range: config
            .range("fill_rate_change_duration_s_range", json::positive)?,
        collection_threshold_range: config
            .range("collection_threshold_range", json::positive_ratio)?,
        collection_duration_factor_range: config
            .range("collection_duration_factor_range", json::positive)?,
        collection_rate_factor_range,
        collection_rate_change_duration_s_range: config
            .range("collection_rate_change_duration_s_range", json::positive)?,
        inlet_positions_xy_m: inlets,
        inlet_switch_activation_ratio: config.ratio("inlet_switch_activation_ratio")?,
        inlet_switch_height_difference_m: config
            .non_negative("inlet_switch_height_difference_m")?,
        inlet_comparison_radius_m: config.positive("inlet_comparison_radius_m")?,
        surface: surface(config.get("surface"))?,
    })
}

fn average_range(config: &Object<'_>, key: &str) -> Result<FloatRange> {
    let range = config.range(key, json::positive)?;
    if range[0] > 1.0 || range[1] < 1.0 {
        return Err(ConfigurationError::new(
            ErrorKind::Range,
            config.at(key),
            "must include the cycle average factor 1",
        ));
    }
    Ok(range)
}

fn surface(value: &Value) -> Result<SurfaceConfig> {
    let config = Object::new(
        value,
        "$.scenario.surface",
        &[
            "cell_size_m",
            "update_interval_s",
            "pile_spread_radius_m",
            "roughness_height_range_m",
            "roughness_radius_range_m",
        ],
    )?;
    Ok(SurfaceConfig {
        cell_size_m: config.positive("cell_size_m")?,
        update_interval_s: config.positive("update_interval_s")?,
        pile_spread_radius_m: config.positive("pile_spread_radius_m")?,
        roughness_height_range_m: config.range("roughness_height_range_m", json::number)?,
        roughness_radius_range_m: config.range("roughness_radius_range_m", json::positive)?,
    })
}

fn measurement(value: &Value) -> Result<MeasurementConfig> {
    let config = Object::new(
        value,
        "$.measurement",
        &[
            "sample_rate_hz",
            "rotation_rate_hz",
            "min_distance_m",
            "max_distance_m",
            "distance_noise",
            "distortions",
        ],
    )?;
    let sample_rate_hz = config.positive("sample_rate_hz")?;
    let rotation_rate_hz = config.positive("rotation_rate_hz")?;
    if sample_rate_hz < rotation_rate_hz {
        return Err(ConfigurationError::new(
            ErrorKind::Range,
            config.at("sample_rate_hz"),
            "must be at least rotation_rate_hz",
        ));
    }
    if validate_scan_point_limit(sample_rate_hz, rotation_rate_hz).is_err() {
        return Err(ConfigurationError::new(
            ErrorKind::Range,
            config.at("sample_rate_hz"),
            "must produce at most 32768 points per rotation",
        ));
    }
    let min_distance_m = contract_distance(&config, "min_distance_m")?;
    let max_distance_m = contract_distance(&config, "max_distance_m")?;
    if max_distance_m <= min_distance_m {
        return Err(ConfigurationError::new(
            ErrorKind::Range,
            config.at("max_distance_m"),
            "must be greater than min_distance_m",
        ));
    }
    let noise = Object::new(
        config.get("distance_noise"),
        config.at("distance_noise"),
        &["enabled", "standard_deviation_m", "limit_m"],
    )?;
    Ok(MeasurementConfig {
        sample_rate_hz,
        rotation_rate_hz,
        min_distance_m,
        max_distance_m,
        distance_noise: DistanceNoiseConfig {
            enabled: noise.boolean("enabled")?,
            standard_deviation_m: noise.non_negative("standard_deviation_m")?,
            limit_m: noise.non_negative("limit_m")?,
        },
        distortions: distortions(config.get("distortions"))?,
    })
}

fn contract_distance(config: &Object<'_>, key: &str) -> Result<f64> {
    json::bound(
        config.number(key)?,
        &config.at(key),
        |value| (0.05..=30.0).contains(&value),
        "must be between 0.05 and 30.0",
    )
}

fn distortions(value: &Value) -> Result<DistortionConfig> {
    let config = Object::new(
        value,
        "$.measurement.distortions",
        &[
            "falling_material",
            "voids",
            "collection_occlusion",
            "reflection_error",
            "dropout",
        ],
    )?;
    let falling = Object::new(
        config.get("falling_material"),
        config.at("falling_material"),
        &[
            "enabled",
            "event_rate_per_s",
            "radius_m_range",
            "duration_s_range",
            "distance_reduction_m_range",
        ],
    )?;
    let voids = Object::new(
        config.get("voids"),
        config.at("voids"),
        &[
            "enabled",
            "surface_area_ratio",
            "radius_m_range",
            "duration_s_range",
            "cover_height_increase_m",
            "distance_increase_m_range",
        ],
    )?;
    let occlusion = Object::new(
        config.get("collection_occlusion"),
        config.at("collection_occlusion"),
        &[
            "enabled",
            "event_interval_s_range",
            "radius_m_range",
            "duration_s_range",
            "distance_reduction_m_range",
        ],
    )?;
    let reflection = Object::new(
        config.get("reflection_error"),
        config.at("reflection_error"),
        &["enabled", "probability", "distance_reduction_m_range"],
    )?;
    let dropout = Object::new(
        config.get("dropout"),
        config.at("dropout"),
        &["enabled", "event_interval_s_range", "duration_s_range"],
    )?;
    Ok(DistortionConfig {
        falling_material: FallingMaterialConfig {
            enabled: falling.boolean("enabled")?,
            event_rate_per_s: falling.non_negative("event_rate_per_s")?,
            radius_m_range: falling.range("radius_m_range", json::positive)?,
            duration_s_range: falling.range("duration_s_range", json::positive)?,
            distance_reduction_m_range: falling
                .range("distance_reduction_m_range", json::positive)?,
        },
        voids: VoidsConfig {
            enabled: voids.boolean("enabled")?,
            surface_area_ratio: voids.ratio("surface_area_ratio")?,
            radius_m_range: voids.range("radius_m_range", json::positive)?,
            duration_s_range: voids.range("duration_s_range", json::positive)?,
            cover_height_increase_m: voids.non_negative("cover_height_increase_m")?,
            distance_increase_m_range: voids.range("distance_increase_m_range", json::positive)?,
        },
        collection_occlusion: CollectionOcclusionConfig {
            enabled: occlusion.boolean("enabled")?,
            event_interval_s_range: occlusion.range("event_interval_s_range", json::positive)?,
            radius_m_range: occlusion.range("radius_m_range", json::positive)?,
            duration_s_range: occlusion.range("duration_s_range", json::positive)?,
            distance_reduction_m_range: occlusion
                .range("distance_reduction_m_range", json::positive)?,
        },
        reflection_error: ReflectionErrorConfig {
            enabled: reflection.boolean("enabled")?,
            probability: reflection.ratio("probability")?,
            distance_reduction_m_range: reflection
                .range("distance_reduction_m_range", json::positive)?,
        },
        dropout: DropoutConfig {
            enabled: dropout.boolean("enabled")?,
            event_interval_s_range: dropout.range("event_interval_s_range", json::positive)?,
            duration_s_range: dropout.range("duration_s_range", json::positive)?,
        },
    })
}

fn transport(value: &Value) -> Result<ObservationTransportConfig> {
    let config = Object::new(
        value,
        "$.observation_transport",
        &[
            "connect_timeout_s",
            "send_timeout_s",
            "reconnect_initial_delay_s",
            "reconnect_max_delay_s",
        ],
    )?;
    let initial = config.positive("reconnect_initial_delay_s")?;
    let maximum = config.positive("reconnect_max_delay_s")?;
    if initial > maximum {
        return Err(ConfigurationError::new(
            ErrorKind::Range,
            config.at("reconnect_initial_delay_s"),
            "must not exceed reconnect_max_delay_s",
        ));
    }
    Ok(ObservationTransportConfig {
        connect_timeout_s: config.positive("connect_timeout_s")?,
        send_timeout_s: config.positive("send_timeout_s")?,
        reconnect_initial_delay_s: initial,
        reconnect_max_delay_s: maximum,
    })
}
