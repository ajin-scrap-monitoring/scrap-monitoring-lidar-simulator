use scrap_monitoring_lidar_simulator::{
    configuration::{SimulatorInputs, load_simulator_inputs},
    geometry::{Polygon2, Vec2},
    measurement::{
        CollectionOcclusionSettings, DistortionInterval, DistortionPhase, DropoutSettings,
        EnvironmentScene, FallingMaterialSettings, HitKind, MeasurementGenerator,
        MeasurementSettings, QualityDistribution, ReferencePoint, ReferenceScan, ScheduledScan,
        SensorDropoutScheduler, SpatialDistortionTimeline, TimedReferenceScan, VoidSettings,
    },
    rate_profile::SmoothRateProfile,
    scenario::{HeightField, build_scenario_simulator},
};

fn inputs() -> SimulatorInputs {
    load_simulator_inputs("examples/simulator.v2.json").unwrap()
}

fn polygon(inputs: &SimulatorInputs) -> Polygon2 {
    Polygon2::new(
        inputs
            .environment
            .boundary_xy_m
            .iter()
            .map(|value| Vec2::try_from(*value).unwrap())
            .collect(),
    )
    .unwrap()
}

fn fixed_distribution(value: usize) -> QualityDistribution {
    let mut frequencies = [0_u64; 256];
    frequencies[value] = 1;
    QualityDistribution::new(frequencies).unwrap()
}

fn reference(times: Vec<f64>, distances: Vec<f64>) -> TimedReferenceScan {
    let angles: Vec<_> = (0..times.len()).map(|index| index as f64 * 10.0).collect();
    let completed_at_s = times.last().copied().unwrap() + 0.1;
    let schedule =
        ScheduledScan::new("lidar_1", 1, 0.0, completed_at_s, angles.clone(), times).unwrap();
    let points = angles
        .into_iter()
        .zip(distances)
        .map(|(angle, distance)| {
            ReferencePoint::new(
                angle,
                distance,
                (distance > 0.0).then_some(HitKind::Surface),
            )
            .unwrap()
        })
        .collect();
    TimedReferenceScan::new(schedule, ReferenceScan::new("lidar_1", points).unwrap()).unwrap()
}

fn settings(dropout: Option<DropoutSettings>) -> MeasurementSettings {
    MeasurementSettings {
        sensor_id: "lidar_1".to_owned(),
        min_distance_m: 0.05,
        max_distance_m: 30.0,
        noise_enabled: false,
        noise_standard_deviation_m: 0.01,
        noise_limit_m: 0.03,
        reflection_error_enabled: false,
        reflection_error_probability: 0.0,
        reflection_error_reduction_range_m: [0.1, 0.2],
        valid_quality: fixed_distribution(48),
        invalid_quality: fixed_distribution(24),
        dropout,
    }
}

#[test]
fn quality_totals_use_u128_and_singleton_profiles_are_exact() {
    let distribution = QualityDistribution::new([i64::MAX as u64; 256]).unwrap();
    assert_eq!(distribution.total(), u128::from(i64::MAX as u64) * 256);
    let reference = reference(vec![0.0, 0.1], vec![1.0, 0.0]);
    let mut generator = MeasurementGenerator::new(settings(None), 7).unwrap();
    let result = generator.generate(reference, None).unwrap();
    assert_eq!(result.measured().distances_m(), [1.0, 0.0]);
    assert_eq!(result.measured().qualities(), [48, 24]);
}

#[test]
fn fixed_dropout_schedule_matches_the_half_open_fixture() {
    let mut scheduler = SensorDropoutScheduler::new(
        "lidar_1",
        DropoutSettings {
            event_interval_s_range: [0.5, 0.5],
            duration_s_range: [0.5, 0.5],
        },
        123_456_789,
    )
    .unwrap();
    assert_eq!(
        scheduler.active_mask(&[0.0, 0.5, 1.0, 1.5, 2.0]).unwrap(),
        [false, true, false, true, false]
    );
}

#[test]
fn dropout_schedule_rejects_an_unbounded_event_batch() {
    let mut scheduler = SensorDropoutScheduler::new(
        "lidar_1",
        DropoutSettings {
            event_interval_s_range: [f64::MIN_POSITIVE, f64::MIN_POSITIVE],
            duration_s_range: [f64::MIN_POSITIVE, f64::MIN_POSITIVE],
        },
        1,
    )
    .unwrap();
    assert!(scheduler.active_mask(&[1.0]).is_err());
}

#[test]
fn dropout_failure_does_not_consume_scheduler_state() {
    let settings = DropoutSettings {
        event_interval_s_range: [0.000_001, 0.000_001],
        duration_s_range: [0.000_001, 0.000_001],
    };
    let mut attempted = SensorDropoutScheduler::new("lidar_1", settings, 11).unwrap();
    let mut pristine = SensorDropoutScheduler::new("lidar_1", settings, 11).unwrap();
    assert!(attempted.active_mask(&[1.0]).is_err());
    assert_eq!(
        attempted.active_mask(&[0.000_001_5]).unwrap(),
        pristine.active_mask(&[0.000_001_5]).unwrap()
    );
}

#[test]
fn dropout_overwrites_prior_distance_and_selects_invalid_quality() {
    let reference = reference(vec![0.0, 0.5, 1.0], vec![1.0001, 2.0, 3.0]);
    let mut generator = MeasurementGenerator::new(
        settings(Some(DropoutSettings {
            event_interval_s_range: [0.5, 0.5],
            duration_s_range: [0.5, 0.5],
        })),
        5,
    )
    .unwrap();
    let result = generator.generate(reference, None).unwrap();
    assert_eq!(result.measured().distances_m(), [1.0, 0.0, 3.0]);
    assert_eq!(result.measured().qualities(), [48, 24, 48]);
}

#[test]
fn independent_model_streams_make_enabled_generation_replay_exact() {
    let mut noisy = settings(None);
    noisy.noise_enabled = true;
    noisy.reflection_error_enabled = true;
    noisy.reflection_error_probability = 0.5;
    let mut first = MeasurementGenerator::new(noisy.clone(), 99).unwrap();
    let mut second = MeasurementGenerator::new(noisy, 99).unwrap();
    let input = reference(vec![0.0, 0.1, 0.2, 0.3], vec![4.0; 4]);
    let left = first.generate(input.clone(), None).unwrap();
    let right = second.generate(input, None).unwrap();
    assert_eq!(left.measured(), right.measured());
}

#[test]
fn falling_timeline_uses_model_rng_and_replays_independently() {
    let inputs = inputs();
    let boundary = polygon(&inputs);
    let static_scene = || {
        EnvironmentScene::new(
            boundary.clone(),
            inputs.environment.floor_z_m,
            inputs.environment.top_z_m,
            Vec::new(),
        )
        .unwrap()
    };
    let falling = Some(FallingMaterialSettings {
        event_rate_per_s: 4.0,
        radius_m_range: [0.2, 0.4],
        duration_s_range: [0.1, 0.3],
        distance_reduction_m_range: [0.2, 0.5],
        inlet_positions: inputs
            .simulator
            .scenario
            .inlet_positions_xy_m
            .iter()
            .map(|value| Vec2::try_from(*value).unwrap())
            .collect(),
        placement_radius_m: inputs.simulator.scenario.surface.pile_spread_radius_m,
    });
    let mut first = SpatialDistortionTimeline::new(
        boundary.clone(),
        static_scene(),
        falling.clone(),
        None,
        None,
        123,
    )
    .unwrap();
    let mut second =
        SpatialDistortionTimeline::new(boundary.clone(), static_scene(), falling, None, None, 123)
            .unwrap();
    let scenario = build_scenario_simulator(&inputs).unwrap();
    let surface = scenario.surface().surface_snapshot().unwrap();
    let phase = DistortionPhase::from(scenario.active_phase().unwrap());
    first
        .advance_to(DistortionInterval {
            started_at_s: 0.0,
            ends_at_s: 2.0,
            phase,
            surface: &surface,
        })
        .unwrap();
    second
        .advance_to(DistortionInterval {
            started_at_s: 0.0,
            ends_at_s: 0.75,
            phase,
            surface: &surface,
        })
        .unwrap();
    second
        .advance_to(DistortionInterval {
            started_at_s: 0.75,
            ends_at_s: 2.0,
            phase,
            surface: &surface,
        })
        .unwrap();
    assert_eq!(
        first.resolver().falling_events(),
        second.resolver().falling_events()
    );
    assert_eq!(
        first
            .resolver()
            .falling_events()
            .iter()
            .map(|event| [
                event.started_at_s.to_bits(),
                event.ends_at_s.to_bits(),
                event.center.x().to_bits(),
                event.center.y().to_bits(),
                event.radius_m.to_bits(),
                event.distance_reduction_m.to_bits(),
            ])
            .collect::<Vec<_>>(),
        [
            [
                0x3fd5_48b9_02c8_0426,
                0x3fe4_15e0_6a87_f998,
                0x3ff8_28ac_89d8_3700,
                0x3ffd_4358_c371_ca8c,
                0x3fd7_d7e2_3883_03cc,
                0x3fd0_aa64_e324_93b6,
            ],
            [
                0x3fd7_f954_2abf_745d,
                0x3fe0_5657_8d1a_5427,
                0x3ff5_4c5c_3ff9_ca0e,
                0x3ffc_aeab_f759_8395,
                0x3fcb_040f_831f_6500,
                0x3fda_1dd1_3719_f600,
            ],
            [
                0x3fea_63f9_8e14_f12b,
                0x3ff0_407d_bd55_69cf,
                0x3ff2_bdf6_eaf2_b033,
                0x3ffb_9513_1138_649d,
                0x3fcd_ef4d_3cd6_62df,
                0x3fce_b3d1_d7ab_bbd0,
            ],
            [
                0x3ff1_41a1_ec63_9798,
                0x3ff4_816a_1f92_fc02,
                0x3ff9_4154_bff5_564a,
                0x3fff_d8ea_100d_5554,
                0x3fd8_0dd2_9635_77d4,
                0x3fcd_0be8_3031_8773,
            ],
            [
                0x3ff1_9830_baee_e463,
                0x3ff3_ef7b_2530_0b14,
                0x3ff2_f34d_4991_6cc8,
                0x3ff8_d128_59b7_de75,
                0x3fcb_206e_737b_4f04,
                0x3fdc_a29f_f07f_34ee,
            ],
            [
                0x3ff2_845a_6376_6983,
                0x3ff5_0230_df96_38e4,
                0x3ff3_318e_e42b_0eeb,
                0x3ff5_a14c_f63f_348e,
                0x3fd7_8534_4444_9c3c,
                0x3fdb_a303_ef1a_8289,
            ],
            [
                0x3ff7_6e45_5ee2_f4b8,
                0x3ff9_a5cd_a14f_6601,
                0x3ffa_029e_f3a8_6337,
                0x3ff4_0062_3b8a_298c,
                0x3fcb_0452_324d_8b08,
                0x3fd9_5d78_140a_b30e,
            ],
            [
                0x3ff7_efc7_c95a_cd21,
                0x3ffc_b6d6_5976_6ab0,
                0x3ffe_d489_76d6_d3c9,
                0x3ffb_7a7d_558f_950d,
                0x3fd7_58a9_8069_66e5,
                0x3fde_8acd_6ba8_0758,
            ],
        ]
    );
    assert!(!first.resolver().falling_events().is_empty());
    assert!(
        first
            .resolver()
            .falling_events()
            .iter()
            .all(|event| event.started_at_s < 2.0 && event.ends_at_s <= 2.0)
    );
}

#[test]
fn void_coverage_and_collection_timeline_stay_bounded_to_their_phases() {
    let boundary = Polygon2::new(vec![
        Vec2::new(-1.0, -1.0).unwrap(),
        Vec2::new(1.0, -1.0).unwrap(),
        Vec2::new(1.0, 1.0).unwrap(),
        Vec2::new(-1.0, 1.0).unwrap(),
    ])
    .unwrap();
    let mut surface = HeightField::new(boundary.clone(), 0.0, 3.0, 0.25).unwrap();
    surface
        .add_volume(1.0, Vec2::new(0.0, 0.0).unwrap(), 10.0)
        .unwrap();
    let snapshot = surface.surface_snapshot().unwrap();
    let make_timeline = || {
        SpatialDistortionTimeline::new(
            boundary.clone(),
            EnvironmentScene::new(boundary.clone(), 0.0, 3.0, Vec::new()).unwrap(),
            None,
            Some(VoidSettings {
                surface_area_ratio: 0.001,
                radius_m_range: [0.1, 0.1],
                duration_s_range: [0.4, 0.4],
                cover_height_increase_m: 0.1,
                distance_increase_m_range: [0.2, 0.2],
            }),
            Some(CollectionOcclusionSettings {
                event_interval_s_range: [0.2, 0.2],
                radius_m_range: [0.1, 0.1],
                duration_s_range: [0.1, 0.1],
                distance_reduction_m_range: [0.2, 0.2],
            }),
            47,
        )
        .unwrap()
    };
    let mut timeline = make_timeline();
    let mut chunked = make_timeline();
    let profile = SmoothRateProfile::new(1.0, Vec::new()).unwrap();
    timeline
        .advance_to(DistortionInterval {
            started_at_s: 0.0,
            ends_at_s: 1.0,
            phase: DistortionPhase::Filling {
                cycle_index: 0,
                started_at_s: 0.0,
                ends_at_s: 1.0,
                inlet_index: 0,
                rate_profile: &profile,
            },
            surface: &snapshot,
        })
        .unwrap();
    for (started_at_s, ends_at_s) in [(0.0, 0.35), (0.35, 1.0)] {
        chunked
            .advance_to(DistortionInterval {
                started_at_s,
                ends_at_s,
                phase: DistortionPhase::Filling {
                    cycle_index: 0,
                    started_at_s: 0.0,
                    ends_at_s: 1.0,
                    inlet_index: 0,
                    rate_profile: &profile,
                },
                surface: &snapshot,
            })
            .unwrap();
    }
    assert_eq!(
        timeline.resolver().void_events(),
        chunked.resolver().void_events()
    );
    assert!(!timeline.resolver().void_events().is_empty());
    assert!(
        timeline
            .resolver()
            .void_events()
            .iter()
            .all(|event| event.started_at_s < 1.0 && event.ends_at_s <= 1.0)
    );
    timeline
        .advance_to(DistortionInterval {
            started_at_s: 1.0,
            ends_at_s: 2.0,
            phase: DistortionPhase::Collecting {
                cycle_index: 0,
                started_at_s: 1.0,
                ends_at_s: 2.0,
            },
            surface: &snapshot,
        })
        .unwrap();
    for (started_at_s, ends_at_s) in [(1.0, 1.55), (1.55, 2.0)] {
        chunked
            .advance_to(DistortionInterval {
                started_at_s,
                ends_at_s,
                phase: DistortionPhase::Collecting {
                    cycle_index: 0,
                    started_at_s: 1.0,
                    ends_at_s: 2.0,
                },
                surface: &snapshot,
            })
            .unwrap();
    }
    assert_eq!(
        timeline.resolver().collection_events(),
        chunked.resolver().collection_events()
    );
    assert_eq!(
        timeline
            .resolver()
            .void_events()
            .iter()
            .map(|event| [
                event.started_at_s.to_bits(),
                event.expires_at_s.to_bits(),
                event.ends_at_s.to_bits(),
                event.center.x().to_bits(),
                event.center.y().to_bits(),
                event.surface_height_at_start_m.to_bits(),
                event.radius_m.to_bits(),
                event.distance_increase_m.to_bits(),
            ])
            .collect::<Vec<_>>(),
        [
            [
                0x0000_0000_0000_0000,
                0x3fd9_9999_9999_999a,
                0x3fd9_9999_9999_999a,
                0x3fb4_f2c9_faa9_1c80,
                0xbfd4_bee3_02a0_0fe4,
                0x3fd0_0b3f_5688_556f,
                0x3fb9_9999_9999_999a,
                0x3fc9_9999_9999_999a,
            ],
            [
                0x3fd9_9999_9999_999a,
                0x3fe9_9999_9999_999a,
                0x3fe9_9999_9999_999a,
                0x3fd7_4d4e_0942_4124,
                0xbfea_cded_0bf9_c6f8,
                0x3fcf_f8c2_c185_b61e,
                0x3fb9_9999_9999_999a,
                0x3fc9_9999_9999_999a,
            ],
            [
                0x3fe9_9999_9999_999a,
                0x3ff0_0000_0000_0000,
                0x3ff0_0000_0000_0000,
                0x3faa_affe_cad4_86e0,
                0x3fd0_9709_bb88_8604,
                0x3fd0_0c66_5d37_ca2e,
                0x3fb9_9999_9999_999a,
                0x3fc9_9999_9999_999a,
            ],
        ]
    );
    assert_eq!(
        timeline
            .resolver()
            .collection_events()
            .iter()
            .map(|event| [
                event.started_at_s.to_bits(),
                event.ends_at_s.to_bits(),
                event.start_center.x().to_bits(),
                event.start_center.y().to_bits(),
                event.end_center.x().to_bits(),
                event.end_center.y().to_bits(),
                event.radius_m.to_bits(),
                event.distance_reduction_m.to_bits(),
            ])
            .collect::<Vec<_>>(),
        [
            [
                0x3ff3_3333_3333_3333,
                0x3ff4_cccc_cccc_cccd,
                0x3fe5_3f71_5157_f02a,
                0xbfeb_a2de_1489_0b90,
                0xbfbe_7614_454e_0bb0,
                0x3fe1_b4c9_ed79_76a0,
                0x3fb9_9999_9999_999a,
                0x3fc9_9999_9999_999a,
            ],
            [
                0x3ff6_6666_6666_6666,
                0x3ff8_0000_0000_0000,
                0x3fe5_8692_9ab5_9f44,
                0x3fe4_512b_1b73_d43c,
                0x3fd0_d119_2262_19c4,
                0x3fe3_3eb6_180c_7a8c,
                0x3fb9_9999_9999_999a,
                0x3fc9_9999_9999_999a,
            ],
            [
                0x3ff9_9999_9999_9999,
                0x3ffb_3333_3333_3333,
                0xbfe9_b961_f191_9cb6,
                0x3fa2_7db7_83e1_bf40,
                0x3feb_7fe0_a4ff_52f0,
                0x3fd1_c158_e47b_0dc4,
                0x3fb9_9999_9999_999a,
                0x3fc9_9999_9999_999a,
            ],
            [
                0x3ffc_cccc_cccc_cccc,
                0x3ffe_6666_6666_6666,
                0xbfeb_14b9_350f_be04,
                0xbfe1_1f28_c8c7_c11a,
                0x3fe6_2599_e358_44ee,
                0x3fdd_cc39_8617_8218,
                0x3fb9_9999_9999_999a,
                0x3fc9_9999_9999_999a,
            ],
            [
                0x3fff_ffff_ffff_ffff,
                0x4000_0000_0000_0000,
                0x3fce_1721_9ec8_96a8,
                0x3fdc_e147_c64c_e31c,
                0x3fd9_d781_1201_9190,
                0x3fce_8dad_7d93_9818,
                0x3fb9_9999_9999_999a,
                0x3fc9_9999_9999_999a,
            ],
        ]
    );
    assert!(!timeline.resolver().collection_events().is_empty());
    assert!(
        timeline
            .resolver()
            .collection_events()
            .iter()
            .all(|event| event.started_at_s >= 1.0 && event.ends_at_s <= 2.0)
    );
}

#[test]
fn void_timeline_rejects_a_radius_with_unrepresentable_area() {
    let boundary = Polygon2::new(vec![
        Vec2::new(-1.0, -1.0).unwrap(),
        Vec2::new(1.0, -1.0).unwrap(),
        Vec2::new(1.0, 1.0).unwrap(),
        Vec2::new(-1.0, 1.0).unwrap(),
    ])
    .unwrap();
    let static_scene = EnvironmentScene::new(boundary.clone(), 0.0, 3.0, Vec::new()).unwrap();
    assert!(
        SpatialDistortionTimeline::new(
            boundary,
            static_scene,
            None,
            Some(VoidSettings {
                surface_area_ratio: 0.1,
                radius_m_range: [1e-200, 1e-200],
                duration_s_range: [0.1, 0.1],
                cover_height_increase_m: 0.1,
                distance_increase_m_range: [0.1, 0.1],
            }),
            None,
            1,
        )
        .is_err()
    );
}

#[test]
fn distortion_timeline_failure_preserves_events_and_rng_state() {
    let boundary = Polygon2::new(vec![
        Vec2::new(-1.0, -1.0).unwrap(),
        Vec2::new(1.0, -1.0).unwrap(),
        Vec2::new(1.0, 1.0).unwrap(),
        Vec2::new(-1.0, 1.0).unwrap(),
    ])
    .unwrap();
    let static_scene = EnvironmentScene::new(boundary.clone(), 0.0, 3.0, Vec::new()).unwrap();
    let mut surface = HeightField::new(boundary.clone(), 0.0, 3.0, 0.25).unwrap();
    surface
        .add_volume(1.0, Vec2::new(0.0, 0.0).unwrap(), 10.0)
        .unwrap();
    let mut timeline = SpatialDistortionTimeline::new(
        boundary,
        static_scene,
        None,
        Some(VoidSettings {
            surface_area_ratio: 0.001,
            radius_m_range: [0.1, 0.1],
            duration_s_range: [0.8, 0.8],
            cover_height_increase_m: 0.01,
            distance_increase_m_range: [0.2, 0.2],
        }),
        None,
        71,
    )
    .unwrap();
    let profile = SmoothRateProfile::new(1.0, Vec::new()).unwrap();
    timeline
        .advance_to(DistortionInterval {
            started_at_s: 0.0,
            ends_at_s: 0.25,
            phase: DistortionPhase::Filling {
                cycle_index: 0,
                started_at_s: 0.0,
                ends_at_s: 1.0,
                inlet_index: 0,
                rate_profile: &profile,
            },
            surface: &surface.surface_snapshot().unwrap(),
        })
        .unwrap();
    let event = timeline.resolver().void_events()[0];
    surface.add_volume(0.5, event.center, 0.5).unwrap();
    let changed = surface.surface_snapshot().unwrap();
    let wrong_profile = SmoothRateProfile::new(2.0, Vec::new()).unwrap();
    assert!(
        timeline
            .advance_to(DistortionInterval {
                started_at_s: 0.25,
                ends_at_s: 0.5,
                phase: DistortionPhase::Filling {
                    cycle_index: 0,
                    started_at_s: 0.0,
                    ends_at_s: 1.0,
                    inlet_index: 0,
                    rate_profile: &wrong_profile,
                },
                surface: &changed,
            })
            .is_err()
    );
    assert_eq!(timeline.advanced_to_s(), 0.25);
    assert_eq!(timeline.resolver().void_events(), [event]);

    timeline
        .advance_to(DistortionInterval {
            started_at_s: 0.25,
            ends_at_s: 0.5,
            phase: DistortionPhase::Filling {
                cycle_index: 0,
                started_at_s: 0.0,
                ends_at_s: 1.0,
                inlet_index: 0,
                rate_profile: &profile,
            },
            surface: &changed,
        })
        .unwrap();
    assert_eq!(timeline.resolver().void_events()[0].ends_at_s, 0.25);
}

#[test]
fn model_v1_measurement_has_an_exact_mixed_vector() {
    let mut valid_frequencies = [0_u64; 256];
    valid_frequencies[10] = 1;
    valid_frequencies[20] = 2;
    valid_frequencies[200] = 3;
    let mut invalid_frequencies = [0_u64; 256];
    invalid_frequencies[1] = 2;
    invalid_frequencies[8] = 1;
    let settings = MeasurementSettings {
        sensor_id: "lidar_1".to_owned(),
        min_distance_m: 0.05,
        max_distance_m: 30.0,
        noise_enabled: true,
        noise_standard_deviation_m: 0.05,
        noise_limit_m: 0.08,
        reflection_error_enabled: true,
        reflection_error_probability: 0.45,
        reflection_error_reduction_range_m: [0.1, 0.35],
        valid_quality: QualityDistribution::new(valid_frequencies).unwrap(),
        invalid_quality: QualityDistribution::new(invalid_frequencies).unwrap(),
        dropout: Some(DropoutSettings {
            event_interval_s_range: [0.18, 0.31],
            duration_s_range: [0.08, 0.19],
        }),
    };
    let times = (0..12).map(|index| index as f64 * 0.1).collect();
    let input = reference(
        times,
        vec![
            1.0, 2.0, 0.0, 0.06, 29.95, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0, 9.0,
        ],
    );
    let result = MeasurementGenerator::new(settings, 0x1234_5678_9abc_def0)
        .unwrap()
        .generate(input, None)
        .unwrap();
    assert_eq!(
        result
            .measured()
            .distances_m()
            .iter()
            .map(|value| value.to_bits())
            .collect::<Vec<_>>(),
        [
            0x3fe8_5c28_f5c2_8f5c,
            0x3fff_d3f7_ced9_1687,
            0,
            0,
            0x403d_b2b0_20c4_9ba6,
            0x4005_29fb_e76c_8b44,
            0x400f_bef9_db22_d0e5,
            0,
            0x4018_1c6a_7ef9_db23,
            0x401a_ea7e_f9db_22d1,
            0,
            0x4022_2395_8106_24dd,
        ]
    );
    assert_eq!(
        result.measured().qualities(),
        [200, 20, 1, 1, 10, 20, 200, 1, 200, 20, 8, 20]
    );
}

#[test]
fn sensor_feature_toggles_do_not_shift_other_random_streams() {
    let times = vec![0.0, 0.1, 0.2, 0.3];
    let angles: Vec<_> = (0..times.len()).map(|index| index as f64 * 10.0).collect();
    let schedule =
        ScheduledScan::new("lidar_1", 1, 0.0, 0.4, angles.clone(), times.clone()).unwrap();
    let points = angles
        .into_iter()
        .map(|angle| ReferencePoint::new(angle, 4.0, Some(HitKind::Wall)).unwrap())
        .collect();
    let wall_reference =
        TimedReferenceScan::new(schedule, ReferenceScan::new("lidar_1", points).unwrap()).unwrap();

    let mut without_reflection = settings(None);
    without_reflection.noise_enabled = true;
    let mut with_reflection = without_reflection.clone();
    with_reflection.reflection_error_enabled = true;
    with_reflection.reflection_error_probability = 1.0;
    let without = MeasurementGenerator::new(without_reflection, 91)
        .unwrap()
        .generate(wall_reference.clone(), None)
        .unwrap();
    let with = MeasurementGenerator::new(with_reflection, 91)
        .unwrap()
        .generate(wall_reference, None)
        .unwrap();
    assert_eq!(without.measured(), with.measured());

    let mut valid_frequencies = [0_u64; 256];
    valid_frequencies[10] = 1;
    valid_frequencies[200] = 1;
    let mut noiseless = settings(None);
    noiseless.valid_quality = QualityDistribution::new(valid_frequencies).unwrap();
    let mut noisy = noiseless.clone();
    noisy.noise_enabled = true;
    let input = reference(times, vec![4.0; 4]);
    let noiseless = MeasurementGenerator::new(noiseless, 92)
        .unwrap()
        .generate(input.clone(), None)
        .unwrap();
    let noisy = MeasurementGenerator::new(noisy, 92)
        .unwrap()
        .generate(input, None)
        .unwrap();
    assert_eq!(
        noiseless.measured().qualities(),
        noisy.measured().qualities()
    );
    assert_ne!(
        noiseless.measured().distances_m(),
        noisy.measured().distances_m()
    );
}

#[test]
fn failed_measurement_does_not_consume_sensor_random_streams() {
    let mut generator_settings = settings(None);
    generator_settings.max_distance_m = f64::MAX;
    generator_settings.noise_enabled = true;
    generator_settings.reflection_error_enabled = true;
    generator_settings.reflection_error_probability = 1.0;
    let mut attempted = MeasurementGenerator::new(generator_settings.clone(), 93).unwrap();
    let mut pristine = MeasurementGenerator::new(generator_settings, 93).unwrap();

    assert!(
        attempted
            .generate(reference(vec![0.0], vec![f64::MAX]), None)
            .is_err()
    );
    let input = reference(vec![0.1, 0.2], vec![4.0, 5.0]);
    assert_eq!(
        attempted.generate(input.clone(), None).unwrap().measured(),
        pristine.generate(input, None).unwrap().measured()
    );
}
