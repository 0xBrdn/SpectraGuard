use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::dsp::{DetectedBurst, ModulationType};

/// Severity classification for passive Wireless Intrusion Detection alerts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum AlertSeverity {
    Info,
    Warning,
    Critical,
}

/// Physical-layer RF anomaly taxonomy monitored by SpectraGuard.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum AlertCategory {
    BroadbandJamming,
    NarrowbandJamming,
    ReplayAttack,
    RogueTransmitter,
    DutyCycleAnomaly,
}

/// Structured physical-layer forensic metrics attached to a `WidsAlert`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AlertEvidence {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub noise_floor_delta_db: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub channel_occupancy_pct: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub correlation_score: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub inter_arrival_ms: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_us: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub freq_offset_hz: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub estimated_baud_rate: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reference_burst_id: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub offending_burst_id: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reference_burst_envelope: Option<Vec<f32>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub offending_burst_envelope: Option<Vec<f32>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bitstream_hex: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deviation_details: Option<String>,
}

/// Passive WIDS security alert generated from physical-layer RF telemetry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WidsAlert {
    pub id: u64,
    pub timestamp_utc: DateTime<Utc>,
    pub last_updated_utc: DateTime<Utc>,
    pub occurrence_count: u32,
    pub severity: AlertSeverity,
    pub category: AlertCategory,
    pub title: String,
    pub center_freq_hz: u64,
    pub snr_db: f32,
    pub confidence_pct: f32,
    pub evidence: AlertEvidence,
}

/// Authorized industrial OT/IoT transmitter PHY/MAC envelope profile.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceBaselineProfile {
    pub name: String,
    pub node_id: u8,
    pub expected_offset_hz: f32,
    pub offset_tolerance_hz: f32,
    pub modulation: ModulationType,
    pub expected_baud_rate: u32,
    pub baud_tolerance_pct: f32,
    pub min_duration_us: u64,
    pub max_duration_us: u64,
    pub expected_period_ms: u64,
}

impl DeviceBaselineProfile {
    pub fn default_industrial_profiles() -> Vec<Self> {
        vec![
            Self {
                name: "OT-TempPressure-Node-01".to_string(),
                node_id: 1,
                expected_offset_hz: -350_000.0,
                offset_tolerance_hz: 15_000.0,
                modulation: ModulationType::Ook,
                expected_baud_rate: 4_800,
                baud_tolerance_pct: 35.0,
                min_duration_us: 3_000,
                max_duration_us: 30_000,
                expected_period_ms: 430,
            },
            Self {
                name: "OT-ValveActuator-Node-02".to_string(),
                node_id: 2,
                expected_offset_hz: 120_000.0,
                offset_tolerance_hz: 18_000.0,
                modulation: ModulationType::Fsk2,
                expected_baud_rate: 9_600,
                baud_tolerance_pct: 35.0,
                min_duration_us: 18_000,
                max_duration_us: 42_000,
                expected_period_ms: 610,
            },
            Self {
                name: "OT-FlowMeter-Node-03".to_string(),
                node_id: 3,
                expected_offset_hz: 480_000.0,
                offset_tolerance_hz: 15_000.0,
                modulation: ModulationType::Ook,
                expected_baud_rate: 3_200,
                baud_tolerance_pct: 35.0,
                min_duration_us: 3_000,
                max_duration_us: 52_000,
                expected_period_ms: 790,
            },
        ]
    }

    pub fn matches_burst(&self, burst: &DetectedBurst) -> Result<(), String> {
        let offset_delta = (burst.center_offset_hz - self.expected_offset_hz).abs();
        if offset_delta > self.offset_tolerance_hz {
            return Err(format!(
                "offset {:.1} kHz outside {:.1} +/- {:.1} kHz",
                burst.center_offset_hz / 1_000.0,
                self.expected_offset_hz / 1_000.0,
                self.offset_tolerance_hz / 1_000.0
            ));
        }

        if burst.modulation_hint != self.modulation {
            return Err(format!(
                "modulation {:?} != expected {:?}",
                burst.modulation_hint, self.modulation
            ));
        }

        if burst.duration_us < self.min_duration_us || burst.duration_us > self.max_duration_us {
            return Err(format!(
                "duration {} us outside [{}..{}] us",
                burst.duration_us, self.min_duration_us, self.max_duration_us
            ));
        }

        if burst.estimated_baud_rate > 0 && self.expected_baud_rate > 0 {
            let baud_diff_pct = ((burst.estimated_baud_rate as f32
                - self.expected_baud_rate as f32)
                .abs()
                / self.expected_baud_rate as f32)
                * 100.0;
            if baud_diff_pct > self.baud_tolerance_pct {
                return Err(format!(
                    "baud {} Bd deviates {:.1}% from {} Bd (max {:.1}%)",
                    burst.estimated_baud_rate,
                    baud_diff_pct,
                    self.expected_baud_rate,
                    self.baud_tolerance_pct
                ));
            }
        }

        Ok(())
    }
}

/// Detection thresholds and sliding-window bounds for `WidsEngine`.
#[derive(Debug, Clone)]
pub struct WidsRuleConfig {
    pub baseline_ema_alpha: f32,
    pub baseline_max_occupancy_pct: f32,
    pub broadband_delta_db: f32,
    pub broadband_min_occupancy_pct: f32,
    pub broadband_min_duration_ms: u64,
    pub narrowband_min_duration_us: u64,
    pub narrowband_min_snr_db: f32,
    pub alert_cooldown_ms: u64,
    pub replay_history_capacity: usize,
    pub replay_max_age_ns: u64,
    pub replay_freq_tolerance_hz: f32,
    pub replay_duration_tolerance_ratio: f32,
    pub replay_min_correlation: f32,
    pub replay_max_inter_arrival_ms: f64,
    pub duty_cycle_window_ns: u64,
    pub duty_cycle_max_pct: f32,
    pub max_stored_alerts: usize,
    pub max_stored_bursts: usize,
}

impl Default for WidsRuleConfig {
    fn default() -> Self {
        Self {
            baseline_ema_alpha: 0.02,
            baseline_max_occupancy_pct: 30.0,
            broadband_delta_db: 10.0,
            broadband_min_occupancy_pct: 65.0,
            broadband_min_duration_ms: 250,
            narrowband_min_duration_us: 250_000,
            narrowband_min_snr_db: 18.0,
            alert_cooldown_ms: 2_000,
            replay_history_capacity: 64,
            replay_max_age_ns: 60_000_000_000,
            replay_freq_tolerance_hz: 15_000.0,
            replay_duration_tolerance_ratio: 0.18,
            replay_min_correlation: 0.93,
            replay_max_inter_arrival_ms: 5_000.0,
            duty_cycle_window_ns: 10_000_000_000,
            duty_cycle_max_pct: 22.0,
            max_stored_alerts: 200,
            max_stored_bursts: 100,
        }
    }
}