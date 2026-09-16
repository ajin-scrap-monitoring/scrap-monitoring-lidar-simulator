//! Long-running simulation composition independent of external delivery.

mod application;
#[cfg(feature = "edge-validation")]
mod edge_validation;
mod generation;

#[cfg(feature = "edge-validation")]
pub use application::run_edge_validation_application;
pub use application::{ApplicationError, ApplicationSummary, run_simulator_application};
#[cfg(feature = "edge-validation")]
pub use edge_validation::{
    DEFAULT_MEASUREMENT_DURATION_S, DEFAULT_SAMPLE_CAPACITY, DEFAULT_WARMUP_DURATION_S,
    EdgeValidationError, EdgeValidationObservationMode, EdgeValidationReport,
    EdgeValidationSettings, MAX_SAMPLE_CAPACITY, MAX_SCENARIO_TRANSITIONS, ScenarioPhaseTelemetry,
    ScenarioStateTelemetry, ScenarioTelemetry, ScenarioTransitionTelemetry,
    write_edge_validation_report_atomic,
};
pub use generation::{
    GenerationBatch, GenerationRuntime, GenerationRuntimeError, GenerationRuntimeStats,
    ScenarioPhaseTransition,
};
