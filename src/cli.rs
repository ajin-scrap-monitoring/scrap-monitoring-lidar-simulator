//! Deployment setting resolution and command-line execution boundaries.

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

#[cfg(feature = "edge-validation")]
use clap::ValueEnum;
use clap::{Args, Parser, Subcommand};

#[cfg(feature = "edge-validation")]
use crate::runtime::{
    DEFAULT_MEASUREMENT_DURATION_S, DEFAULT_SAMPLE_CAPACITY, DEFAULT_WARMUP_DURATION_S,
    MAX_SAMPLE_CAPACITY,
};

use crate::{
    configuration::{SimulatorInputs, load_simulator_inputs},
    edge_integration::{build_synthetic_processing_config, write_synthetic_processing_config},
    error::{ConfigurationError, ErrorKind, Result},
};

pub const CONFIG_ENV: &str = "SCRAP_LIDAR_SIMULATOR_CONFIG";
pub const MEAN_FILL_DURATION_ENV: &str = "SCRAP_LIDAR_SIMULATOR_MEAN_FILL_DURATION_S";
pub const COLLECTION_THRESHOLD_ENV: &str =
    "SCRAP_LIDAR_SIMULATOR_COLLECTION_THRESHOLD_CENTER_RATIO";
pub const SOCKET_DIR_ENV: &str = "SCRAP_LIDAR_SIMULATOR_GRPC_SOCKET_DIR";
pub const STATUS_DIR_ENV: &str = "SCRAP_LIDAR_SIMULATOR_STATUS_DIR";
pub const OBSERVATION_HOST_ENV: &str = "SCRAP_LIDAR_SIMULATOR_OBSERVATION_HOST";
pub const OBSERVATION_PORT_ENV: &str = "SCRAP_LIDAR_SIMULATOR_OBSERVATION_PORT";
pub const OBSERVATION_INTERVAL_ENV: &str = "SCRAP_LIDAR_SIMULATOR_OBSERVATION_INTERVAL_S";
pub const DIAGNOSTICS_ENABLED_ENV: &str = "SCRAP_LIDAR_SIMULATOR_DIAGNOSTICS_ENABLED";
pub const DIAGNOSTICS_OUTPUT_ENV: &str = "SCRAP_LIDAR_SIMULATOR_DIAGNOSTICS_OUTPUT_PATH";

pub type Environment = BTreeMap<String, String>;

#[derive(Debug, Parser)]
#[command(
    name = "scrap-monitoring-lidar-simulator",
    version,
    about = "Run and validate the deterministic synthetic LiDAR simulator."
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Validate referenced public JSON inputs without starting generation.
    #[command(args_override_self = true, infer_long_args = true)]
    Check {
        /// Also require and validate every deployment setting.
        #[arg(long)]
        runtime: bool,
        #[command(flatten)]
        overrides: Box<RuntimeSettingOverrides>,
    },
    /// Run paced synthetic generation and serve both LiDAR scan lanes.
    #[command(args_override_self = true, infer_long_args = true)]
    Run {
        #[command(flatten)]
        overrides: Box<RuntimeSettingOverrides>,
    },
    /// Run bounded Raspberry Pi frame latency telemetry.
    #[cfg(feature = "edge-validation")]
    #[command(
        name = "edge-validation",
        args_override_self = true,
        infer_long_args = true
    )]
    EdgeValidation(EdgeValidationArgs),
    /// Export the public synthetic environment for pinned lidar-processing.
    #[command(name = "export-synthetic-processing-config")]
    ExportSyntheticProcessingConfig(ExportSyntheticProcessingConfigArgs),
}

#[cfg(feature = "edge-validation")]
#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub enum EdgeValidationObservationArg {
    Actual,
    NoOp,
}

#[cfg(feature = "edge-validation")]
#[derive(Clone, Debug, Args)]
pub struct EdgeValidationArgs {
    #[command(flatten)]
    pub overrides: Box<RuntimeSettingOverrides>,
    /// Use the production observation publisher or the benchmark no-op boundary.
    #[arg(long, value_enum, default_value = "actual")]
    pub observation_mode: EdgeValidationObservationArg,
    /// Exclude this many whole seconds before latency collection.
    #[arg(
        long,
        default_value_t = DEFAULT_WARMUP_DURATION_S,
        value_parser = cli_positive_u64
    )]
    pub warmup_duration_s: u64,
    /// Collect every scheduled frame completion in this many whole seconds.
    #[arg(
        long,
        default_value_t = DEFAULT_MEASUREMENT_DURATION_S,
        value_parser = cli_positive_u64
    )]
    pub measurement_duration_s: u64,
    /// Bound each sensor series and the batch maximum series to this many samples.
    #[arg(
        long,
        default_value_t = DEFAULT_SAMPLE_CAPACITY,
        value_parser = cli_sample_capacity
    )]
    pub max_samples: usize,
    /// Start generation at this future host CLOCK_MONOTONIC nanosecond value.
    #[arg(long, value_parser = cli_positive_u64)]
    pub start_at_monotonic_ns: Option<u64>,
    /// Create this absolute JSON result path atomically without replacement.
    #[arg(long, value_parser = cli_absolute_path_buf)]
    pub output: PathBuf,
}

#[derive(Clone, Debug, Args)]
pub struct ExportSyntheticProcessingConfigArgs {
    #[arg(long = "simulator-config", value_parser = cli_path)]
    pub simulator_config: Option<String>,
    #[arg(long, value_parser = cli_path)]
    pub output: String,
    #[arg(long, default_value = "/sockets", value_parser = cli_absolute_path)]
    pub socket_dir: String,
    #[arg(long, value_parser = cli_identity)]
    pub site_id: Option<String>,
    #[arg(long, value_parser = cli_driver_identity)]
    pub edge_id: Option<String>,
    #[arg(long, value_parser = cli_driver_identity)]
    pub config_revision: Option<String>,
}

/// Raw values are resolved before parsing so valid CLI values hide invalid environment values.
#[derive(Clone, Debug, Default, Args)]
pub struct RuntimeSettingOverrides {
    #[arg(long = "config", value_parser = cli_path)]
    pub config_path: Option<String>,
    #[arg(long, value_parser = cli_absolute_path)]
    pub grpc_socket_dir: Option<String>,
    #[arg(long, value_parser = cli_absolute_path)]
    pub status_dir: Option<String>,
    #[arg(long, value_parser = cli_identity)]
    pub site_id: Option<String>,
    #[arg(long, value_parser = cli_driver_identity)]
    pub edge_id: Option<String>,
    #[arg(long, value_parser = cli_driver_identity)]
    pub config_revision: Option<String>,
    #[arg(long, value_parser = cli_identity)]
    pub deployment_revision: Option<String>,
    #[arg(long, value_parser = cli_non_empty)]
    pub observation_host: Option<String>,
    #[arg(long, value_parser = cli_port)]
    pub observation_port: Option<String>,
    #[arg(long, value_parser = cli_observation_interval)]
    pub observation_interval_s: Option<String>,
    #[arg(long, value_parser = cli_boolean)]
    pub diagnostics_enabled: Option<String>,
    #[arg(long, value_parser = cli_path)]
    pub diagnostics_output_path: Option<String>,
    #[arg(long, value_parser = cli_positive)]
    pub mean_fill_duration_s: Option<String>,
    #[arg(long, value_parser = cli_collection_threshold)]
    pub collection_threshold_center_ratio: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ModelOverrides {
    pub config_path: PathBuf,
    pub diagnostics_enabled: Option<bool>,
    pub diagnostics_output_path: Option<PathBuf>,
    pub mean_fill_duration_s: Option<f64>,
    pub collection_threshold_center_ratio: Option<f64>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct RuntimeSettings {
    pub model: ModelOverrides,
    pub grpc_socket_dir: PathBuf,
    pub status_dir: PathBuf,
    pub site_id: String,
    pub edge_id: String,
    pub config_revision: String,
    pub deployment_revision: String,
    pub observation_host: String,
    pub observation_port: u16,
    pub observation_interval_s: f64,
}

pub fn resolve_model_overrides(
    environment: &Environment,
    overrides: &RuntimeSettingOverrides,
) -> Result<ModelOverrides> {
    Ok(ModelOverrides {
        config_path: required(
            resolve(&overrides.config_path, environment, CONFIG_ENV, path)?,
            "--config",
            CONFIG_ENV,
        )?,
        diagnostics_enabled: resolve(
            &overrides.diagnostics_enabled,
            environment,
            DIAGNOSTICS_ENABLED_ENV,
            boolean,
        )?,
        diagnostics_output_path: resolve(
            &overrides.diagnostics_output_path,
            environment,
            DIAGNOSTICS_OUTPUT_ENV,
            path,
        )?,
        mean_fill_duration_s: resolve(
            &overrides.mean_fill_duration_s,
            environment,
            MEAN_FILL_DURATION_ENV,
            positive,
        )?,
        collection_threshold_center_ratio: resolve(
            &overrides.collection_threshold_center_ratio,
            environment,
            COLLECTION_THRESHOLD_ENV,
            collection_threshold,
        )?,
    })
}

pub fn resolve_runtime_settings(
    environment: &Environment,
    overrides: &RuntimeSettingOverrides,
) -> Result<RuntimeSettings> {
    Ok(RuntimeSettings {
        model: resolve_model_overrides(environment, overrides)?,
        grpc_socket_dir: required(
            resolve(
                &overrides.grpc_socket_dir,
                environment,
                SOCKET_DIR_ENV,
                absolute_path,
            )?,
            "--grpc-socket-dir",
            SOCKET_DIR_ENV,
        )?,
        status_dir: required(
            resolve(
                &overrides.status_dir,
                environment,
                STATUS_DIR_ENV,
                absolute_path,
            )?,
            "--status-dir",
            STATUS_DIR_ENV,
        )?,
        site_id: required(
            resolve(&overrides.site_id, environment, "SITE_ID", identity)?,
            "--site-id",
            "SITE_ID",
        )?,
        edge_id: required(
            resolve(&overrides.edge_id, environment, "EDGE_ID", driver_identity)?,
            "--edge-id",
            "EDGE_ID",
        )?,
        config_revision: required(
            resolve(
                &overrides.config_revision,
                environment,
                "CONFIG_REVISION",
                driver_identity,
            )?,
            "--config-revision",
            "CONFIG_REVISION",
        )?,
        deployment_revision: required(
            resolve(
                &overrides.deployment_revision,
                environment,
                "DEPLOYMENT_REVISION",
                identity,
            )?,
            "--deployment-revision",
            "DEPLOYMENT_REVISION",
        )?,
        observation_host: required(
            resolve(
                &overrides.observation_host,
                environment,
                OBSERVATION_HOST_ENV,
                non_empty,
            )?,
            "--observation-host",
            OBSERVATION_HOST_ENV,
        )?,
        observation_port: required(
            resolve(
                &overrides.observation_port,
                environment,
                OBSERVATION_PORT_ENV,
                port,
            )?,
            "--observation-port",
            OBSERVATION_PORT_ENV,
        )?,
        observation_interval_s: resolve(
            &overrides.observation_interval_s,
            environment,
            OBSERVATION_INTERVAL_ENV,
            observation_interval,
        )?
        .unwrap_or(1.0),
    })
}

pub fn load_overridden_inputs(settings: &ModelOverrides) -> Result<SimulatorInputs> {
    let mut inputs = load_simulator_inputs(&settings.config_path)?;
    let simulator = &mut inputs.simulator;
    if let Some(value) = settings.mean_fill_duration_s {
        simulator.scenario.mean_fill_duration_s = value;
    }
    if let Some(center) = settings.collection_threshold_center_ratio {
        simulator.scenario.collection_threshold_range = [center - 0.05, center + 0.05];
    }
    if let Some(enabled) = settings.diagnostics_enabled {
        simulator.diagnostics.enabled = enabled;
    }
    if let Some(path) = &settings.diagnostics_output_path {
        simulator.diagnostics.output_path = if path.is_absolute() {
            path.clone()
        } else {
            settings
                .config_path
                .parent()
                .unwrap_or(Path::new("."))
                .join(path)
        };
    }
    Ok(inputs)
}

pub fn check(command: &Command, environment: &Environment) -> Result<SimulatorInputs> {
    match command {
        Command::Check { runtime, overrides } => {
            let settings = if *runtime {
                resolve_runtime_settings(environment, overrides)?.model
            } else {
                resolve_model_overrides(environment, overrides)?
            };
            load_overridden_inputs(&settings)
        }
        Command::ExportSyntheticProcessingConfig(_) => Err(invalid(
            "command",
            "export command cannot be used as a configuration check",
        )),
        Command::Run { .. } => Err(invalid(
            "command",
            "run command cannot be used as a configuration check",
        )),
        #[cfg(feature = "edge-validation")]
        Command::EdgeValidation(_) => Err(invalid(
            "command",
            "edge validation command cannot be used as a configuration check",
        )),
    }
}

pub fn export_synthetic_processing_config(
    arguments: &ExportSyntheticProcessingConfigArgs,
    environment: &Environment,
) -> std::result::Result<(), crate::edge_integration::ProcessingConfigError> {
    let config_path = required_export_value(
        arguments
            .simulator_config
            .as_deref()
            .or_else(|| environment.get(CONFIG_ENV).map(String::as_str)),
        "--simulator-config or SCRAP_LIDAR_SIMULATOR_CONFIG",
    )?;
    let site_id = required_export_value(
        arguments
            .site_id
            .as_deref()
            .or_else(|| environment.get("SITE_ID").map(String::as_str)),
        "--site-id or SITE_ID",
    )?;
    let edge_id = required_export_value(
        arguments
            .edge_id
            .as_deref()
            .or_else(|| environment.get("EDGE_ID").map(String::as_str)),
        "--edge-id or EDGE_ID",
    )?;
    let config_revision = required_export_value(
        arguments
            .config_revision
            .as_deref()
            .or_else(|| environment.get("CONFIG_REVISION").map(String::as_str)),
        "--config-revision or CONFIG_REVISION",
    )?;
    let inputs = load_simulator_inputs(config_path).map_err(|error| {
        crate::edge_integration::ProcessingConfigError::Invalid(error.to_string())
    })?;
    let output = build_synthetic_processing_config(
        &inputs,
        Path::new(&arguments.socket_dir),
        site_id,
        edge_id,
        config_revision,
    )?;
    write_synthetic_processing_config(Path::new(&arguments.output), &output)
}

fn required_export_value<'a>(
    value: Option<&'a str>,
    source: &str,
) -> std::result::Result<&'a str, crate::edge_integration::ProcessingConfigError> {
    value.filter(|value| !value.is_empty()).ok_or_else(|| {
        crate::edge_integration::ProcessingConfigError::Invalid(format!("set {source}"))
    })
}

fn resolve<T>(
    cli: &Option<String>,
    environment: &Environment,
    variable: &str,
    parser: fn(&str, &str) -> Result<T>,
) -> Result<Option<T>> {
    cli.as_ref()
        .or_else(|| environment.get(variable))
        .map(|value| parser(value, variable))
        .transpose()
}

fn required<T>(value: Option<T>, option: &str, variable: &str) -> Result<T> {
    value.ok_or_else(|| invalid(variable, &format!("set {option} or {variable}")))
}

fn invalid(name: &str, message: &str) -> ConfigurationError {
    ConfigurationError::new(ErrorKind::Runtime, name, message)
}

fn non_empty(value: &str, name: &str) -> Result<String> {
    if value.is_empty() {
        Err(invalid(name, "must be non-empty"))
    } else {
        Ok(value.to_owned())
    }
}

fn path(value: &str, name: &str) -> Result<PathBuf> {
    non_empty(value, name).map(PathBuf::from)
}

fn absolute_path(value: &str, name: &str) -> Result<PathBuf> {
    let value = path(value, name)?;
    if !value.is_absolute() {
        return Err(invalid(name, "must be an absolute path"));
    }
    Ok(value)
}

fn identity(value: &str, name: &str) -> Result<String> {
    if value.is_empty()
        || value.len() > 128
        || !value.as_bytes()[0].is_ascii_alphanumeric()
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"_.-".contains(&byte))
    {
        return Err(invalid(name, "must be a safe deployment identifier"));
    }
    Ok(value.to_owned())
}

fn driver_identity(value: &str, name: &str) -> Result<String> {
    let value = identity(value, name)?;
    if value.len() > 64 {
        return Err(invalid(name, "must be a driver-compatible identifier"));
    }
    Ok(value)
}

fn positive(value: &str, name: &str) -> Result<f64> {
    let value = numeric_text(value, name)?
        .parse::<f64>()
        .map_err(|_| invalid(name, "must be a number"))?;
    if !value.is_finite() || value <= 0.0 {
        return Err(invalid(name, "must be a finite positive number"));
    }
    Ok(value)
}

fn boolean(value: &str, name: &str) -> Result<bool> {
    match value {
        "true" => Ok(true),
        "false" => Ok(false),
        _ => Err(invalid(name, "must be true or false")),
    }
}

fn port(value: &str, name: &str) -> Result<u16> {
    numeric_text(value, name)?
        .parse::<u16>()
        .ok()
        .filter(|value| *value > 0)
        .ok_or_else(|| invalid(name, "must be an integer in [1, 65535]"))
}

fn numeric_text(value: &str, name: &str) -> Result<String> {
    let value: String = value.trim().chars().map(decimal_character).collect();
    let bytes = value.as_bytes();
    for (index, byte) in bytes.iter().enumerate() {
        if *byte == b'_'
            && (index == 0
                || index + 1 == bytes.len()
                || !bytes[index - 1].is_ascii_digit()
                || !bytes[index + 1].is_ascii_digit())
        {
            return Err(invalid(name, "must be a number"));
        }
    }
    Ok(value.replace('_', ""))
}

fn decimal_character(character: char) -> char {
    // Numeric CLI values accept these Unicode 16.0 decimal digit blocks.
    const ZERO_POINTS: &[u32] = &[
        0x30, 0x660, 0x6f0, 0x7c0, 0x966, 0x9e6, 0xa66, 0xae6, 0xb66, 0xbe6, 0xc66, 0xce6, 0xd66,
        0xde6, 0xe50, 0xed0, 0xf20, 0x1040, 0x1090, 0x17e0, 0x1810, 0x1946, 0x19d0, 0x1a80, 0x1a90,
        0x1b50, 0x1bb0, 0x1c40, 0x1c50, 0xa620, 0xa8d0, 0xa900, 0xa9d0, 0xa9f0, 0xaa50, 0xabf0,
        0xff10, 0x104a0, 0x10d30, 0x10d40, 0x11066, 0x110f0, 0x11136, 0x111d0, 0x112f0, 0x11450,
        0x114d0, 0x11650, 0x116c0, 0x116d0, 0x116da, 0x11730, 0x118e0, 0x11950, 0x11bf0, 0x11c50,
        0x11d50, 0x11da0, 0x11f50, 0x16130, 0x16a60, 0x16ac0, 0x16b50, 0x16d70, 0x1ccf0, 0x1d7ce,
        0x1d7d8, 0x1d7e2, 0x1d7ec, 0x1d7f6, 0x1e140, 0x1e2f0, 0x1e4f0, 0x1e5f1, 0x1e950, 0x1fbf0,
    ];
    let codepoint = u32::from(character);
    let after = ZERO_POINTS.partition_point(|zero| *zero <= codepoint);
    if let Some(index) = after.checked_sub(1) {
        let digit = codepoint - ZERO_POINTS[index];
        if digit < 10 {
            return char::from(b'0' + digit as u8);
        }
    }
    character
}

fn observation_interval(value: &str, name: &str) -> Result<f64> {
    let value = positive(value, name)?;
    if value > 86_400.0 {
        return Err(invalid(name, "must not exceed 86400 seconds"));
    }
    Ok(value)
}

fn collection_threshold(value: &str, name: &str) -> Result<f64> {
    let value = positive(value, name)?;
    if value <= 0.05 || value > 0.95 {
        return Err(invalid(name, "must be greater than 0.05 and at most 0.95"));
    }
    Ok(value)
}

fn validate_cli<T>(
    value: &str,
    parser: fn(&str, &str) -> Result<T>,
) -> std::result::Result<String, String> {
    parser(value, "CLI value")
        .map(|_| value.to_owned())
        .map_err(|error| error.to_string())
}

fn cli_non_empty(value: &str) -> std::result::Result<String, String> {
    validate_cli(value, non_empty)
}

fn cli_path(value: &str) -> std::result::Result<String, String> {
    validate_cli(value, path)
}

fn cli_absolute_path(value: &str) -> std::result::Result<String, String> {
    validate_cli(value, absolute_path)
}

fn cli_identity(value: &str) -> std::result::Result<String, String> {
    validate_cli(value, identity)
}

fn cli_driver_identity(value: &str) -> std::result::Result<String, String> {
    validate_cli(value, driver_identity)
}

fn cli_positive(value: &str) -> std::result::Result<String, String> {
    validate_cli(value, positive)
}

fn cli_boolean(value: &str) -> std::result::Result<String, String> {
    validate_cli(value, boolean)
}

fn cli_port(value: &str) -> std::result::Result<String, String> {
    validate_cli(value, port)
}

fn cli_observation_interval(value: &str) -> std::result::Result<String, String> {
    validate_cli(value, observation_interval)
}

fn cli_collection_threshold(value: &str) -> std::result::Result<String, String> {
    validate_cli(value, collection_threshold)
}

#[cfg(feature = "edge-validation")]
fn cli_absolute_path_buf(value: &str) -> std::result::Result<PathBuf, String> {
    absolute_path(value, "CLI value").map_err(|error| error.to_string())
}

#[cfg(feature = "edge-validation")]
fn cli_non_negative_u64(value: &str) -> std::result::Result<u64, String> {
    numeric_text(value, "CLI value")
        .map_err(|error| error.to_string())?
        .parse::<u64>()
        .map_err(|_| invalid("CLI value", "must be a non-negative integer").to_string())
}

#[cfg(feature = "edge-validation")]
fn cli_positive_u64(value: &str) -> std::result::Result<u64, String> {
    cli_non_negative_u64(value).and_then(|parsed| {
        (parsed > 0)
            .then_some(parsed)
            .ok_or_else(|| invalid("CLI value", "must be a positive integer").to_string())
    })
}

#[cfg(feature = "edge-validation")]
fn cli_sample_capacity(value: &str) -> std::result::Result<usize, String> {
    let parsed = cli_positive_u64(value)?;
    usize::try_from(parsed)
        .ok()
        .filter(|parsed| *parsed <= MAX_SAMPLE_CAPACITY)
        .ok_or_else(|| invalid("CLI value", "must be an integer from 1 through 100000").to_string())
}
