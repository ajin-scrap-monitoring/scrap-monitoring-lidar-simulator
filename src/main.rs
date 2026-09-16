use clap::Parser;
use scrap_monitoring_lidar_simulator::{
    cli::{self, Cli, Command},
    runtime::run_simulator_application,
};

#[cfg(feature = "edge-validation")]
use scrap_monitoring_lidar_simulator::{
    cli::EdgeValidationObservationArg,
    runtime::{
        EdgeValidationObservationMode, EdgeValidationSettings, run_edge_validation_application,
    },
};

#[tokio::main(flavor = "multi_thread")]
async fn main() -> std::process::ExitCode {
    let cli = Cli::parse();
    let environment = std::env::vars_os()
        .filter_map(|(key, value)| Some((key.into_string().ok()?, value.into_string().ok()?)))
        .collect();
    match &cli.command {
        command @ Command::Check { .. } => match cli::check(command, &environment) {
            Ok(inputs) => {
                println!(
                    "validation=passed sensors={} seed={}",
                    inputs.environment.sensors.len(),
                    inputs.simulator.seed
                );
                std::process::ExitCode::SUCCESS
            }
            Err(error) => {
                eprintln!("configuration error: {error}");
                std::process::ExitCode::from(2)
            }
        },
        Command::ExportSyntheticProcessingConfig(arguments) => {
            match cli::export_synthetic_processing_config(arguments, &environment) {
                Ok(()) => std::process::ExitCode::SUCCESS,
                Err(error) => {
                    eprintln!("configuration error: {error}");
                    std::process::ExitCode::from(2)
                }
            }
        }
        Command::Run { overrides } => {
            let settings = match cli::resolve_runtime_settings(&environment, overrides) {
                Ok(settings) => settings,
                Err(error) => {
                    eprintln!("configuration error: {error}");
                    return std::process::ExitCode::from(2);
                }
            };
            let inputs = match cli::load_overridden_inputs(&settings.model) {
                Ok(inputs) => inputs,
                Err(error) => {
                    eprintln!("configuration error: {error}");
                    return std::process::ExitCode::from(2);
                }
            };
            match run_simulator_application(inputs, settings).await {
                Ok(summary) => {
                    let published = summary
                        .scan_stats
                        .sensors
                        .iter()
                        .map(|sensor| sensor.published_frames)
                        .sum::<u64>();
                    let frame_loss = summary
                        .scan_stats
                        .sensors
                        .iter()
                        .map(|sensor| sensor.frame_loss)
                        .sum::<u64>();
                    let subscribers = summary
                        .scan_stats
                        .sensors
                        .iter()
                        .map(|sensor| sensor.subscribers)
                        .sum::<usize>();
                    println!(
                        "run_id={} generated={} published={}",
                        summary.run_id, summary.generated_scans, published
                    );
                    println!(
                        "scan_stream published={published} frame_loss={frame_loss} subscribers={subscribers}"
                    );
                    println!(
                        "observation=active sent={} dropped={} connection_failures={}",
                        summary.observation_stats.sent_records,
                        summary.observation_stats.dropped_records,
                        summary.observation_stats.connection_failures
                    );
                    std::process::ExitCode::SUCCESS
                }
                Err(error) => {
                    eprintln!("runtime error: {error}");
                    std::process::ExitCode::from(1)
                }
            }
        }
        #[cfg(feature = "edge-validation")]
        Command::EdgeValidation(arguments) => {
            let settings = match cli::resolve_runtime_settings(&environment, &arguments.overrides) {
                Ok(settings) => settings,
                Err(error) => {
                    eprintln!("configuration error: {error}");
                    return std::process::ExitCode::from(2);
                }
            };
            let inputs = match cli::load_overridden_inputs(&settings.model) {
                Ok(inputs) => inputs,
                Err(error) => {
                    eprintln!("configuration error: {error}");
                    return std::process::ExitCode::from(2);
                }
            };
            let observation_mode = match arguments.observation_mode {
                EdgeValidationObservationArg::Actual => EdgeValidationObservationMode::Actual,
                EdgeValidationObservationArg::NoOp => EdgeValidationObservationMode::NoOp,
            };
            let validation = match EdgeValidationSettings::new(
                observation_mode,
                std::time::Duration::from_secs(arguments.warmup_duration_s),
                std::time::Duration::from_secs(arguments.measurement_duration_s),
                arguments.max_samples,
                arguments.output.clone(),
            )
            .and_then(|settings| {
                settings.with_start_at_monotonic_ns(arguments.start_at_monotonic_ns)
            }) {
                Ok(validation) => validation,
                Err(error) => {
                    eprintln!("configuration error: {error}");
                    return std::process::ExitCode::from(2);
                }
            };
            match run_edge_validation_application(inputs, settings, validation).await {
                Ok(summary) => {
                    println!(
                        "edge_validation=passed run_id={} output={}",
                        summary.run_id,
                        arguments.output.display()
                    );
                    std::process::ExitCode::SUCCESS
                }
                Err(error) => {
                    eprintln!("runtime error: {error}");
                    std::process::ExitCode::from(1)
                }
            }
        }
    }
}
