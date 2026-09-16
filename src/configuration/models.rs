//! Validated simulator inputs in world coordinates and SI lengths.

use std::path::PathBuf;

pub type Coordinate2 = [f64; 2];
pub type Coordinate3 = [f64; 3];
pub type FloatRange = [f64; 2];
pub type QualityFrequencies = [u64; 256];

/// A diagnostic sample count constrained by the public resource bound.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SampleScanLimit(u64);

impl SampleScanLimit {
    pub(super) fn from_validated(value: u64) -> Self {
        debug_assert!(value <= crate::MAX_DIAGNOSTIC_SCANS_PER_SENSOR);
        Self(value)
    }

    pub fn allows(&self, recorded_count: u64) -> bool {
        recorded_count < self.0
    }

    pub fn get(&self) -> u64 {
        self.0
    }
}

impl std::fmt::Display for SampleScanLimit {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(formatter)
    }
}

#[derive(Clone, Debug, PartialEq, serde::Serialize)]
pub struct SensorConfig {
    pub sensor_id: String,
    pub p0_m: Coordinate3,
    pub u0: Coordinate3,
    pub u90: Coordinate3,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize)]
pub struct EnvironmentConfig {
    pub environment_id: String,
    pub boundary_xy_m: Vec<Coordinate2>,
    pub floor_z_m: f64,
    pub top_z_m: f64,
    pub sensors: Vec<SensorConfig>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize)]
pub struct SurfaceConfig {
    pub cell_size_m: f64,
    pub update_interval_s: f64,
    pub pile_spread_radius_m: f64,
    pub roughness_height_range_m: FloatRange,
    pub roughness_radius_range_m: FloatRange,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize)]
pub struct ScenarioConfig {
    pub mean_fill_duration_s: f64,
    pub fill_duration_factor_range: FloatRange,
    pub fill_rate_factor_range: FloatRange,
    pub fill_rate_change_duration_s_range: FloatRange,
    pub collection_threshold_range: FloatRange,
    pub collection_duration_factor_range: FloatRange,
    pub collection_rate_factor_range: FloatRange,
    pub collection_rate_change_duration_s_range: FloatRange,
    pub inlet_positions_xy_m: Vec<Coordinate2>,
    pub inlet_switch_activation_ratio: f64,
    pub inlet_switch_height_difference_m: f64,
    pub inlet_comparison_radius_m: f64,
    pub surface: SurfaceConfig,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize)]
pub struct DistanceNoiseConfig {
    pub enabled: bool,
    pub standard_deviation_m: f64,
    pub limit_m: f64,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize)]
pub struct FallingMaterialConfig {
    pub enabled: bool,
    pub event_rate_per_s: f64,
    pub radius_m_range: FloatRange,
    pub duration_s_range: FloatRange,
    pub distance_reduction_m_range: FloatRange,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize)]
pub struct VoidsConfig {
    pub enabled: bool,
    pub surface_area_ratio: f64,
    pub radius_m_range: FloatRange,
    pub duration_s_range: FloatRange,
    pub cover_height_increase_m: f64,
    pub distance_increase_m_range: FloatRange,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize)]
pub struct CollectionOcclusionConfig {
    pub enabled: bool,
    pub event_interval_s_range: FloatRange,
    pub radius_m_range: FloatRange,
    pub duration_s_range: FloatRange,
    pub distance_reduction_m_range: FloatRange,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize)]
pub struct ReflectionErrorConfig {
    pub enabled: bool,
    pub probability: f64,
    pub distance_reduction_m_range: FloatRange,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize)]
pub struct DropoutConfig {
    pub enabled: bool,
    pub event_interval_s_range: FloatRange,
    pub duration_s_range: FloatRange,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize)]
pub struct DistortionConfig {
    pub falling_material: FallingMaterialConfig,
    pub voids: VoidsConfig,
    pub collection_occlusion: CollectionOcclusionConfig,
    pub reflection_error: ReflectionErrorConfig,
    pub dropout: DropoutConfig,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize)]
pub struct MeasurementConfig {
    pub sample_rate_hz: f64,
    pub rotation_rate_hz: f64,
    pub min_distance_m: f64,
    pub max_distance_m: f64,
    pub distance_noise: DistanceNoiseConfig,
    pub distortions: DistortionConfig,
}

#[derive(Clone, Debug, PartialEq)]
pub struct DiagnosticsConfig {
    pub enabled: bool,
    pub output_path: PathBuf,
    pub sample_scan_limit_per_sensor: SampleScanLimit,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ObservationTransportConfig {
    pub connect_timeout_s: f64,
    pub send_timeout_s: f64,
    pub reconnect_initial_delay_s: f64,
    pub reconnect_max_delay_s: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SimulatorConfig {
    pub seed: u64,
    pub environment_path: PathBuf,
    pub quality_profile_path: PathBuf,
    pub scenario: ScenarioConfig,
    pub measurement: MeasurementConfig,
    pub observation_transport: ObservationTransportConfig,
    pub diagnostics: DiagnosticsConfig,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SensorQualityConfig {
    pub sensor_id: String,
    pub valid_distance_frequencies: QualityFrequencies,
    pub invalid_distance_frequencies: QualityFrequencies,
}

#[derive(Clone, Debug, PartialEq)]
pub struct QualityProfileConfig {
    pub sensors: Vec<SensorQualityConfig>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SimulatorInputs {
    pub simulator: SimulatorConfig,
    pub environment: EnvironmentConfig,
    pub quality_profile: QualityProfileConfig,
}
