use std::collections::VecDeque;
use std::sync::{Arc, RwLock};

use chrono::Utc;
use serde::Serialize;

use crate::dsp::{DetectedBurst, ModulationType, SpectrumFrame};
use crate::error::SpectraError;
use crate::ids::rules::{
    AlertCategory, AlertEvidence, AlertSeverity, DeviceBaselineProfile, WidsAlert, WidsRuleConfig,
};

/// Thread-safe bounded telemetry state exposed to API and WebSocket consumers.
#[derive(Debug, Clone, Serialize)]
pub struct WidsState {
    pub alerts: VecDeque<WidsAlert>,
    pub bursts: VecDeque<DetectedBurst>,
    pub latest_spectrum: Option<SpectrumFrame>,
    pub baseline_noise_floor_dbfs: f32,
    pub total_alerts_emitted: u64,
    pub total_bursts_detected: u64,
}

impl Default for WidsState {
    fn default() -> Self {
        Self {
            alerts: VecDeque::with_capacity(200),
            bursts: VecDeque::with_capacity(100),
            latest_spectrum: None,
            baseline_noise_floor_dbfs: -92.0,
            total_alerts_emitted: 0,
            total_bursts_detected: 0,
        }
    }
}

/// Shared concurrent handle to bounded WIDS alerts and burst telemetry.
pub type SharedWidsState = Arc<RwLock<WidsState>>;

struct NarrowbandTracker {
    alert_id: u64,
    center_offset_hz: f32,
    last_seen_ns: u64,
}

struct RogueTracker {
    alert_id: u64,
    center_offset_hz: f32,
    last_seen_ns: u64,
}

/// Stateful physical-layer Wireless Intrusion Detection System engine.
pub struct WidsEngine {
    config: WidsRuleConfig,
    profiles: Vec<DeviceBaselineProfile>,
    next_alert_id: u64,
    baseline_noise_floor_dbfs: Option<f32>,
    broadband_onset_ns: Option<u64>,
    active_broadband_alert_id: Option<u64>,
    last_broadband_seen_ns: u64,
    active_narrowband: Vec<NarrowbandTracker>,
    active_rogues: Vec<RogueTracker>,
    active_duty_cycle_alert_id: Option<u64>,
    last_duty_cycle_seen_ns: u64,
    replay_window: VecDeque<DetectedBurst>,
    duty_cycle_window: VecDeque<(u64, u64)>,
    shared_state: SharedWidsState,
}

impl WidsEngine {
    pub fn new(
        config: WidsRuleConfig,
        profiles: Vec<DeviceBaselineProfile>,
    ) -> Result<Self, SpectraError> {
        if !(0.0..=1.0).contains(&config.baseline_ema_alpha) || config.baseline_ema_alpha == 0.0 {
            return Err(SpectraError::InvalidDspConfig(
                "baseline_ema_alpha must be in (0.0, 1.0]".to_string(),
            ));
        }
        if config.replay_history_capacity == 0
            || config.max_stored_alerts == 0
            || config.max_stored_bursts == 0
        {
            return Err(SpectraError::InvalidDspConfig(
                "ring-buffer capacities must be greater than zero".to_string(),
            ));
        }

        let shared_state = Arc::new(RwLock::new(WidsState {
            alerts: VecDeque::with_capacity(config.max_stored_alerts),
            bursts: VecDeque::with_capacity(config.max_stored_bursts),
            latest_spectrum: None,
            baseline_noise_floor_dbfs: -92.0,
            total_alerts_emitted: 0,
            total_bursts_detected: 0,
        }));

        Ok(Self {
            config,
            profiles,
            next_alert_id: 1,
            baseline_noise_floor_dbfs: None,
            broadband_onset_ns: None,
            active_broadband_alert_id: None,
            last_broadband_seen_ns: 0,
            active_narrowband: Vec::new(),
            active_rogues: Vec::new(),
            active_duty_cycle_alert_id: None,
            last_duty_cycle_seen_ns: 0,
            replay_window: VecDeque::with_capacity(64),
            duty_cycle_window: VecDeque::with_capacity(128),
            shared_state,
        })
    }

    pub fn shared_state(&self) -> SharedWidsState {
        Arc::clone(&self.shared_state)
    }

    pub fn process_frame(
        &mut self,
        spectrum: &SpectrumFrame,
        bursts: &[DetectedBurst],
    ) -> Vec<WidsAlert> {
        let mut emitted = Vec::new();

        if let Some(alert) = self.evaluate_broadband_jamming(spectrum) {
            emitted.push(alert);
        }

        for burst in bursts {
            self.record_burst(burst);

            let is_narrowband_jammer =
                self.evaluate_narrowband_jamming(spectrum, burst, &mut emitted);
            let is_replay = self.evaluate_replay_attack(spectrum, burst, &mut emitted);

            if !is_narrowband_jammer && !is_replay {
                self.evaluate_rogue_transmitter(spectrum, burst, &mut emitted);
            }

            if let Some(dc_alert) = self.evaluate_duty_cycle(spectrum, burst) {
                emitted.push(dc_alert);
            }

            self.push_replay_history(burst.clone());
        }

        if let Ok(mut guard) = self.shared_state.write() {
            guard.latest_spectrum = Some(spectrum.clone());
            if let Some(baseline) = self.baseline_noise_floor_dbfs {
                guard.baseline_noise_floor_dbfs = baseline;
            }
        }

        emitted
    }

    fn record_burst(&mut self, burst: &DetectedBurst) {
        if let Ok(mut guard) = self.shared_state.write() {
            if guard.bursts.len() >= self.config.max_stored_bursts {
                guard.bursts.pop_front();
            }
            guard.bursts.push_back(burst.clone());
            guard.total_bursts_detected = guard.total_bursts_detected.saturating_add(1);
        }
    }

    fn commit_new_alert(&mut self, alert: WidsAlert) -> WidsAlert {
        if let Ok(mut guard) = self.shared_state.write() {
            if guard.alerts.len() >= self.config.max_stored_alerts {
                guard.alerts.pop_front();
            }
            guard.alerts.push_back(alert.clone());
            guard.total_alerts_emitted = guard.total_alerts_emitted.saturating_add(1);
        }
        alert
    }

    fn update_existing_alert<F>(&mut self, alert_id: u64, mut updater: F) -> Option<WidsAlert>
    where
        F: FnMut(&mut WidsAlert),
    {
        let mut guard = self.shared_state.write().ok()?;
        let existing = guard.alerts.iter_mut().find(|a| a.id == alert_id)?;
        updater(existing);
        existing.last_updated_utc = Utc::now();
        existing.occurrence_count = existing.occurrence_count.saturating_add(1);
        Some(existing.clone())
    }

    fn evaluate_broadband_jamming(&mut self, spectrum: &SpectrumFrame) -> Option<WidsAlert> {
        let cooldown_ns = self.config.alert_cooldown_ms.saturating_mul(1_000_000);
        if self.active_broadband_alert_id.is_some()
            && spectrum.timestamp_ns.saturating_sub(self.last_broadband_seen_ns) > cooldown_ns
        {
            self.active_broadband_alert_id = None;
        }

        let baseline = match self.baseline_noise_floor_dbfs {
            Some(prev) => {
                if spectrum.channel_occupancy_pct < self.config.baseline_max_occupancy_pct {
                    let alpha = self.config.baseline_ema_alpha;
                    let updated = alpha * spectrum.noise_floor_dbfs + (1.0 - alpha) * prev;
                    self.baseline_noise_floor_dbfs = Some(updated);
                    updated
                } else {
                    prev
                }
            }
            None => {
                self.baseline_noise_floor_dbfs = Some(spectrum.noise_floor_dbfs);
                spectrum.noise_floor_dbfs
            }
        };

        let delta_db = spectrum.noise_floor_dbfs - baseline;
        let condition_met = delta_db >= self.config.broadband_delta_db
            && spectrum.channel_occupancy_pct >= self.config.broadband_min_occupancy_pct;

        if !condition_met {
            self.broadband_onset_ns = None;
            return None;
        }

        let onset_ns = *self.broadband_onset_ns.get_or_insert(spectrum.timestamp_ns);
        let elapsed_ms = spectrum.timestamp_ns.saturating_sub(onset_ns) / 1_000_000;
        if elapsed_ms < self.config.broadband_min_duration_ms {
            return None;
        }

        self.last_broadband_seen_ns = spectrum.timestamp_ns;
        let confidence_pct = (70.0
            + (delta_db - self.config.broadband_delta_db).max(0.0) * 1.5
            + (spectrum.channel_occupancy_pct - self.config.broadband_min_occupancy_pct).max(0.0)
                * 0.4)
            .clamp(70.0, 99.9);

        if let Some(alert_id) = self.active_broadband_alert_id {
            if let Some(updated) = self.update_existing_alert(alert_id, |alert| {
                if delta_db > alert.snr_db {
                    alert.snr_db = delta_db;
                }
                if confidence_pct > alert.confidence_pct {
                    alert.confidence_pct = confidence_pct;
                }
                alert.evidence.noise_floor_delta_db = Some(
                    alert
                        .evidence
                        .noise_floor_delta_db
                        .map_or(delta_db, |prev| prev.max(delta_db)),
                );
                alert.evidence.channel_occupancy_pct = Some(
                    alert
                        .evidence
                        .channel_occupancy_pct
                        .map_or(spectrum.channel_occupancy_pct, |prev| {
                            prev.max(spectrum.channel_occupancy_pct)
                        }),
                );
                alert.evidence.duration_us = Some(elapsed_ms.saturating_mul(1_000));
            }) {
                return Some(updated);
            }
        }

        let id = self.next_alert_id;
        self.next_alert_id += 1;
        self.active_broadband_alert_id = Some(id);

        let now = Utc::now();
        let alert = WidsAlert {
            id,
            timestamp_utc: now,
            last_updated_utc: now,
            occurrence_count: 1,
            severity: AlertSeverity::Critical,
            category: AlertCategory::BroadbandJamming,
            title: format!(
                "Broadband RF Jamming (+{:.1} dB noise floor, {:.1}% occupancy)",
                delta_db, spectrum.channel_occupancy_pct
            ),
            center_freq_hz: spectrum.center_freq_hz,
            snr_db: delta_db,
            confidence_pct,
            evidence: AlertEvidence {
                noise_floor_delta_db: Some(delta_db),
                channel_occupancy_pct: Some(spectrum.channel_occupancy_pct),
                duration_us: Some(elapsed_ms.saturating_mul(1_000)),
                deviation_details: Some(format!(
                    "Noise floor {:.1} dBFS vs baseline {:.1} dBFS",
                    spectrum.noise_floor_dbfs, baseline
                )),
                ..AlertEvidence::default()
            },
        };

        Some(self.commit_new_alert(alert))
    }

    fn evaluate_narrowband_jamming(
        &mut self,
        spectrum: &SpectrumFrame,
        burst: &DetectedBurst,
        emitted: &mut Vec<WidsAlert>,
    ) -> bool {
        let cooldown_ns = self.config.alert_cooldown_ms.saturating_mul(1_000_000);
        self.active_narrowband
            .retain(|tracker| burst.timestamp_ns.saturating_sub(tracker.last_seen_ns) <= cooldown_ns);

        let is_unmodulated_carrier = matches!(
            burst.modulation_hint,
            ModulationType::ContinuousCarrier | ModulationType::Unknown
        );
        if !is_unmodulated_carrier
            || burst.duration_us < self.config.narrowband_min_duration_us
            || burst.peak_snr_db < self.config.narrowband_min_snr_db
        {
            return false;
        }

        let target_freq_hz = (spectrum.center_freq_hz as i64)
            .saturating_add(burst.center_offset_hz.round() as i64)
            .max(0) as u64;
        let confidence_pct = (75.0
            + (burst.peak_snr_db - self.config.narrowband_min_snr_db).max(0.0) * 0.8
            + ((burst.duration_us as f32 / 1_000.0) - 250.0).max(0.0) * 0.02)
            .clamp(75.0, 99.5);

        if let Some(tracker) = self
            .active_narrowband
            .iter_mut()
            .find(|t| (t.center_offset_hz - burst.center_offset_hz).abs() <= 25_000.0)
        {
            tracker.last_seen_ns = burst.timestamp_ns;
            let alert_id = tracker.alert_id;
            if let Some(updated) = self.update_existing_alert(alert_id, |alert| {
                if burst.peak_snr_db > alert.snr_db {
                    alert.snr_db = burst.peak_snr_db;
                }
                if confidence_pct > alert.confidence_pct {
                    alert.confidence_pct = confidence_pct;
                }
                alert.evidence.duration_us = Some(
                    alert
                        .evidence
                        .duration_us
                        .unwrap_or(0)
                        .saturating_add(burst.duration_us),
                );
                alert.evidence.offending_burst_id = Some(burst.id);
                alert.evidence.offending_burst_envelope = Some(burst.envelope_signature.clone());
            }) {
                emitted.push(updated);
            }
            return true;
        }

        let id = self.next_alert_id;
        self.next_alert_id += 1;
        self.active_narrowband.push(NarrowbandTracker {
            alert_id: id,
            center_offset_hz: burst.center_offset_hz,
            last_seen_ns: burst.timestamp_ns,
        });

        let now = Utc::now();
        let alert = WidsAlert {
            id,
            timestamp_utc: now,
            last_updated_utc: now,
            occurrence_count: 1,
            severity: AlertSeverity::Warning,
            category: AlertCategory::NarrowbandJamming,
            title: format!(
                "Narrowband Channel Jamming at {:+.1} kHz ({:.1} ms, {:.1} dB SNR)",
                burst.center_offset_hz / 1_000.0,
                burst.duration_us as f64 / 1_000.0,
                burst.peak_snr_db
            ),
            center_freq_hz: target_freq_hz,
            snr_db: burst.peak_snr_db,
            confidence_pct,
            evidence: AlertEvidence {
                duration_us: Some(burst.duration_us),
                freq_offset_hz: Some(burst.center_offset_hz),
                offending_burst_id: Some(burst.id),
                offending_burst_envelope: Some(burst.envelope_signature.clone()),
                deviation_details: Some(format!(
                    "Continuous {:?} carrier blocking channel for {:.1} ms",
                    burst.modulation_hint,
                    burst.duration_us as f64 / 1_000.0
                )),
                ..AlertEvidence::default()
            },
        };

        emitted.push(self.commit_new_alert(alert));
        true
    }

    fn evaluate_replay_attack(
        &mut self,
        spectrum: &SpectrumFrame,
        burst: &DetectedBurst,
        emitted: &mut Vec<WidsAlert>,
    ) -> bool {
        while let Some(oldest) = self.replay_window.front() {
            if burst.timestamp_ns.saturating_sub(oldest.timestamp_ns) > self.config.replay_max_age_ns
            {
                self.replay_window.pop_front();
            } else {
                break;
            }
        }

        let matched_profile = self
            .profiles
            .iter()
            .find(|p| p.matches_burst(burst).is_ok());

        let mut best_match: Option<(&DetectedBurst, f32, f64)> = None;

        for candidate in self.replay_window.iter().rev() {
            let freq_diff = (burst.center_offset_hz - candidate.center_offset_hz).abs();
            if freq_diff > self.config.replay_freq_tolerance_hz {
                continue;
            }

            let max_dur = burst.duration_us.max(candidate.duration_us).max(1) as f32;
            let dur_diff_ratio =
                (burst.duration_us as f32 - candidate.duration_us as f32).abs() / max_dur;
            if dur_diff_ratio > self.config.replay_duration_tolerance_ratio {
                continue;
            }

            let inter_arrival_ms =
                burst.timestamp_ns.saturating_sub(candidate.timestamp_ns) as f64 / 1_000_000.0;
            if inter_arrival_ms >= self.config.replay_max_inter_arrival_ms {
                continue;
            }

            let cadence_violated = matched_profile.is_none_or(|profile| {
                inter_arrival_ms < (profile.expected_period_ms as f64) * 0.70
            });
            let is_full_frame_clone = burst.modulation_hint == ModulationType::Ook
                && burst.bitstream_hex.len() >= 12
                && burst.duration_us >= 14_000
                && burst.bitstream_hex == candidate.bitstream_hex;
            if !cadence_violated && !is_full_frame_clone {
                continue;
            }

            let env_corr =
                pearson_correlation(&candidate.envelope_signature, &burst.envelope_signature);

            let corr = if burst.modulation_hint == ModulationType::Fsk2 {
                if env_corr < 0.84 {
                    continue;
                }
                let dur_sim = (1.0 - dur_diff_ratio).clamp(0.0, 1.0);
                let freq_sim =
                    (1.0 - (freq_diff / self.config.replay_freq_tolerance_hz)).clamp(0.0, 1.0);
                let max_baud = burst.estimated_baud_rate.max(candidate.estimated_baud_rate).max(1) as f32;
                let baud_sim = (1.0
                    - ((burst.estimated_baud_rate as f32 - candidate.estimated_baud_rate as f32)
                        .abs()
                        / max_baud))
                    .clamp(0.0, 1.0);
                0.35 * env_corr + 0.30 * dur_sim + 0.20 * freq_sim + 0.15 * baud_sim
            } else {
                env_corr
            };

            if corr >= self.config.replay_min_correlation
                && best_match.is_none_or(|(_, best_corr, _)| corr > best_corr)
            {
                best_match = Some((candidate, corr, inter_arrival_ms));
            }
        }

        let Some((reference_burst, correlation_score, inter_arrival_ms)) = best_match else {
            return false;
        };

        let target_freq_hz = (spectrum.center_freq_hz as i64)
            .saturating_add(burst.center_offset_hz.round() as i64)
            .max(0) as u64;
        let confidence_pct = (82.0 + ((correlation_score - 0.96).max(0.0) / 0.04) * 17.9)
            .clamp(82.0, 99.9);

        let id = self.next_alert_id;
        self.next_alert_id += 1;

        let now = Utc::now();
        let alert = WidsAlert {
            id,
            timestamp_utc: now,
            last_updated_utc: now,
            occurrence_count: 1,
            severity: AlertSeverity::Critical,
            category: AlertCategory::ReplayAttack,
            title: format!(
                "RF Replay Attack Detected at {:+.1} kHz (r={:.4}, dt={:.1} ms)",
                burst.center_offset_hz / 1_000.0,
                correlation_score,
                inter_arrival_ms
            ),
            center_freq_hz: target_freq_hz,
            snr_db: burst.peak_snr_db,
            confidence_pct,
            evidence: AlertEvidence {
                correlation_score: Some(correlation_score),
                inter_arrival_ms: Some(inter_arrival_ms),
                duration_us: Some(burst.duration_us),
                freq_offset_hz: Some(burst.center_offset_hz),
                estimated_baud_rate: Some(burst.estimated_baud_rate),
                reference_burst_id: Some(reference_burst.id),
                offending_burst_id: Some(burst.id),
                reference_burst_envelope: Some(reference_burst.envelope_signature.clone()),
                offending_burst_envelope: Some(burst.envelope_signature.clone()),
                bitstream_hex: Some(burst.bitstream_hex.clone()),
                deviation_details: Some(format!(
                    "Burst #{} replays Burst #{} after {:.1} ms with {:.2}% envelope correlation",
                    burst.id,
                    reference_burst.id,
                    inter_arrival_ms,
                    correlation_score * 100.0
                )),
                ..AlertEvidence::default()
            },
        };

        emitted.push(self.commit_new_alert(alert));
        true
    }

    fn evaluate_rogue_transmitter(
        &mut self,
        spectrum: &SpectrumFrame,
        burst: &DetectedBurst,
        emitted: &mut Vec<WidsAlert>,
    ) -> bool {
        if self.profiles.is_empty() {
            return false;
        }

        let mut closest_reason = String::new();
        let mut closest_offset_diff = f32::INFINITY;

        for profile in &self.profiles {
            match profile.matches_burst(burst) {
                Ok(()) => return false,
                Err(reason) => {
                    let diff = (burst.center_offset_hz - profile.expected_offset_hz).abs();
                    if diff < closest_offset_diff {
                        closest_offset_diff = diff;
                        closest_reason = format!("nearest profile {}: {}", profile.name, reason);
                    }
                }
            }
        }

        let cooldown_ns = self.config.alert_cooldown_ms.saturating_mul(1_000_000);
        self.active_rogues
            .retain(|tracker| burst.timestamp_ns.saturating_sub(tracker.last_seen_ns) <= cooldown_ns);

        let target_freq_hz = (spectrum.center_freq_hz as i64)
            .saturating_add(burst.center_offset_hz.round() as i64)
            .max(0) as u64;
        let confidence_pct = (78.0 + (burst.peak_snr_db - 10.0).max(0.0) * 0.5).clamp(78.0, 98.5);

        if let Some(tracker) = self
            .active_rogues
            .iter_mut()
            .find(|t| (t.center_offset_hz - burst.center_offset_hz).abs() <= 20_000.0)
        {
            tracker.last_seen_ns = burst.timestamp_ns;
            let alert_id = tracker.alert_id;
            if let Some(updated) = self.update_existing_alert(alert_id, |alert| {
                if burst.peak_snr_db > alert.snr_db {
                    alert.snr_db = burst.peak_snr_db;
                }
                alert.evidence.offending_burst_id = Some(burst.id);
                alert.evidence.offending_burst_envelope = Some(burst.envelope_signature.clone());
                alert.evidence.bitstream_hex = Some(burst.bitstream_hex.clone());
                alert.evidence.duration_us = Some(burst.duration_us);
            }) {
                emitted.push(updated);
            }
            return true;
        }

        let id = self.next_alert_id;
        self.next_alert_id += 1;
        self.active_rogues.push(RogueTracker {
            alert_id: id,
            center_offset_hz: burst.center_offset_hz,
            last_seen_ns: burst.timestamp_ns,
        });

        let now = Utc::now();
        let alert = WidsAlert {
            id,
            timestamp_utc: now,
            last_updated_utc: now,
            occurrence_count: 1,
            severity: AlertSeverity::Warning,
            category: AlertCategory::RogueTransmitter,
            title: format!(
                "Unauthorized Rogue Transmitter at {:+.1} kHz ({:?}, {} Bd)",
                burst.center_offset_hz / 1_000.0,
                burst.modulation_hint,
                burst.estimated_baud_rate
            ),
            center_freq_hz: target_freq_hz,
            snr_db: burst.peak_snr_db,
            confidence_pct,
            evidence: AlertEvidence {
                duration_us: Some(burst.duration_us),
                freq_offset_hz: Some(burst.center_offset_hz),
                estimated_baud_rate: Some(burst.estimated_baud_rate),
                offending_burst_id: Some(burst.id),
                offending_burst_envelope: Some(burst.envelope_signature.clone()),
                bitstream_hex: Some(burst.bitstream_hex.clone()),
                deviation_details: Some(closest_reason),
                ..AlertEvidence::default()
            },
        };

        emitted.push(self.commit_new_alert(alert));
        true
    }

    fn evaluate_duty_cycle(
        &mut self,
        spectrum: &SpectrumFrame,
        burst: &DetectedBurst,
    ) -> Option<WidsAlert> {
        let burst_end_ns = burst
            .timestamp_ns
            .saturating_add(burst.duration_us.saturating_mul(1_000));
        self.duty_cycle_window
            .push_back((burst.timestamp_ns, burst.duration_us));

        while let Some(&(ts_ns, _)) = self.duty_cycle_window.front() {
            if burst_end_ns.saturating_sub(ts_ns) > self.config.duty_cycle_window_ns {
                self.duty_cycle_window.pop_front();
            } else {
                break;
            }
        }

        if self.active_broadband_alert_id.is_some() {
            return None;
        }

        let total_airtime_us: u64 = self.duty_cycle_window.iter().map(|&(_, dur)| dur).sum();
        let window_us = (self.config.duty_cycle_window_ns / 1_000).max(1);
        let duty_cycle_pct = ((total_airtime_us as f32) / (window_us as f32)) * 100.0;

        if duty_cycle_pct <= self.config.duty_cycle_max_pct {
            return None;
        }

        let cooldown_ns = self.config.alert_cooldown_ms.saturating_mul(1_000_000);
        if let Some(alert_id) = self.active_duty_cycle_alert_id {
            if burst.timestamp_ns.saturating_sub(self.last_duty_cycle_seen_ns) <= cooldown_ns {
                self.last_duty_cycle_seen_ns = burst.timestamp_ns;
                return self.update_existing_alert(alert_id, |alert| {
                    alert.evidence.channel_occupancy_pct = Some(duty_cycle_pct);
                });
            }
        }

        let id = self.next_alert_id;
        self.next_alert_id += 1;
        self.active_duty_cycle_alert_id = Some(id);
        self.last_duty_cycle_seen_ns = burst.timestamp_ns;

        let now = Utc::now();
        let alert = WidsAlert {
            id,
            timestamp_utc: now,
            last_updated_utc: now,
            occurrence_count: 1,
            severity: AlertSeverity::Info,
            category: AlertCategory::DutyCycleAnomaly,
            title: format!(
                "ISM Sub-Band Duty Cycle Anomaly ({:.1}% over {:.0}s window)",
                duty_cycle_pct,
                self.config.duty_cycle_window_ns as f64 / 1_000_000_000.0
            ),
            center_freq_hz: spectrum.center_freq_hz,
            snr_db: burst.peak_snr_db,
            confidence_pct: 85.0,
            evidence: AlertEvidence {
                channel_occupancy_pct: Some(duty_cycle_pct),
                duration_us: Some(total_airtime_us),
                deviation_details: Some(format!(
                    "Aggregate airtime {:.1} ms exceeds {:.1}% threshold",
                    total_airtime_us as f64 / 1_000.0,
                    self.config.duty_cycle_max_pct
                )),
                ..AlertEvidence::default()
            },
        };

        Some(self.commit_new_alert(alert))
    }

    fn push_replay_history(&mut self, burst: DetectedBurst) {
        if self.replay_window.len() >= self.config.replay_history_capacity {
            self.replay_window.pop_front();
        }
        self.replay_window.push_back(burst);
    }
}

/// Computes normalized Pearson cross-correlation coefficient in `[-1.0, 1.0]`.
pub fn pearson_correlation(a: &[f32], b: &[f32]) -> f32 {
    let n = a.len().min(b.len());
    if n < 2 {
        return 0.0;
    }

    let inv_n = 1.0 / (n as f64);
    let mean_a: f64 = a[..n].iter().map(|&v| v as f64).sum::<f64>() * inv_n;
    let mean_b: f64 = b[..n].iter().map(|&v| v as f64).sum::<f64>() * inv_n;

    let mut num = 0.0_f64;
    let mut den_a = 0.0_f64;
    let mut den_b = 0.0_f64;

    for i in 0..n {
        let da = (a[i] as f64) - mean_a;
        let db = (b[i] as f64) - mean_b;
        num += da * db;
        den_a += da * da;
        den_b += db * db;
    }

    let denom = (den_a * den_b).sqrt();
    if denom < 1e-12 {
        0.0
    } else {
        (num / denom).clamp(-1.0, 1.0) as f32
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dsp::{BurstDetector, BurstDetectorConfig, SpectrumAnalyzer, SpectrumConfig};
    use crate::sdr::RfSource;
    use crate::sdr::simulator::{SimulatedRfSource, SimulatorConfig, ThreatCommand};

    #[test]
    fn detects_broadband_and_narrowband_jamming_with_aggregation() -> Result<(), SpectraError> {
        let mut engine = WidsEngine::new(
            WidsRuleConfig::default(),
            DeviceBaselineProfile::default_industrial_profiles(),
        )?;

        let clean_frame = SpectrumFrame {
            timestamp_ns: 0,
            center_freq_hz: 868_300_000,
            sample_rate_hz: 2_048_000,
            bins_dbfs: vec![-92.0; 64],
            max_hold_dbfs: vec![-90.0; 64],
            avg_psd_dbfs: vec![-92.0; 64],
            noise_floor_dbfs: -92.0,
            channel_occupancy_pct: 5.0,
        };
        assert!(engine.process_frame(&clean_frame, &[]).is_empty());

        for step in 1..=12u64 {
            let jammed_frame = SpectrumFrame {
                timestamp_ns: step * 50_000_000,
                noise_floor_dbfs: -70.0,
                channel_occupancy_pct: 84.0,
                ..clean_frame.clone()
            };
            engine.process_frame(&jammed_frame, &[]);
        }

        let nb_burst = DetectedBurst {
            id: 99,
            timestamp_ns: 700_000_000,
            duration_us: 400_000,
            center_offset_hz: 120_000.0,
            peak_snr_db: 26.0,
            mean_rssi_dbfs: -15.0,
            modulation_hint: ModulationType::ContinuousCarrier,
            estimated_baud_rate: 0,
            envelope_signature: vec![0.95; 128],
            bitstream_hex: String::new(),
        };
        engine.process_frame(&clean_frame, &[nb_burst]);

        let shared = engine.shared_state();
        let state = shared.read().map_err(|_| SpectraError::ChannelClosed)?;
        let broadband_alerts: Vec<_> = state
            .alerts
            .iter()
            .filter(|a| a.category == AlertCategory::BroadbandJamming)
            .collect();
        assert_eq!(broadband_alerts.len(), 1);
        assert_eq!(broadband_alerts[0].severity, AlertSeverity::Critical);
        assert!(broadband_alerts[0].occurrence_count >= 5);

        let narrowband_alerts: Vec<_> = state
            .alerts
            .iter()
            .filter(|a| a.category == AlertCategory::NarrowbandJamming)
            .collect();
        assert_eq!(narrowband_alerts.len(), 1);
        assert_eq!(narrowband_alerts[0].severity, AlertSeverity::Warning);
        Ok(())
    }

    #[tokio::test]
    async fn detects_replay_and_rogue_without_clean_false_positives() -> Result<(), SpectraError> {
        let sample_rate_hz = 2_048_000;
        let fft_size = 2048;
        let chunk_size = 32_768;

        let (mut sim, threat_tx) = SimulatedRfSource::new(
            868_300_000,
            sample_rate_hz,
            chunk_size,
            SimulatorConfig::default(),
            false,
        )?;
        let mut analyzer = SpectrumAnalyzer::new(SpectrumConfig {
            fft_size,
            ..SpectrumConfig::default()
        })?;
        let mut detector = BurstDetector::new(BurstDetectorConfig {
            fft_size,
            ..BurstDetectorConfig::default()
        })?;
        let mut engine = WidsEngine::new(
            WidsRuleConfig::default(),
            DeviceBaselineProfile::default_industrial_profiles(),
        )?;

        for _ in 0..90 {
            let chunk = sim.next_chunk().await?;
            let spec = analyzer.analyze_chunk(&chunk)?;
            let bursts = detector.process_chunk(&chunk, spec.noise_floor_dbfs)?;
            engine.process_frame(&spec, &bursts);
        }

        let shared = engine.shared_state();
        {
            let state = shared.read().map_err(|_| SpectraError::ChannelClosed)?;
            assert!(
                state.alerts.is_empty(),
                "unexpected alerts during clean traffic: {:?}",
                state.alerts
            );
        }

        let _ = threat_tx.try_send(ThreatCommand::InjectReplayAttack {
            repeat_count: 3,
            interval_ms: 85,
        });

        for _ in 0..30 {
            let chunk = sim.next_chunk().await?;
            let spec = analyzer.analyze_chunk(&chunk)?;
            let bursts = detector.process_chunk(&chunk, spec.noise_floor_dbfs)?;
            engine.process_frame(&spec, &bursts);
        }

        let _ = threat_tx.try_send(ThreatCommand::InjectRogueTransmitter {
            freq_offset_hz: -140_000.0,
            baud_rate: 12_500,
            duration_ms: 120,
        });

        for _ in 0..40 {
            let chunk = sim.next_chunk().await?;
            let spec = analyzer.analyze_chunk(&chunk)?;
            let bursts = detector.process_chunk(&chunk, spec.noise_floor_dbfs)?;
            engine.process_frame(&spec, &bursts);
        }

        let state = shared.read().map_err(|_| SpectraError::ChannelClosed)?;
        let replay = state
            .alerts
            .iter()
            .find(|a| a.category == AlertCategory::ReplayAttack)
            .unwrap_or_else(|| {
                panic!(
                    "missing replay alert; alerts={:?}, bursts={:?}",
                    state.alerts, state.bursts
                )
            });
        assert_eq!(replay.severity, AlertSeverity::Critical);
        assert!(replay.evidence.correlation_score.unwrap_or(0.0) >= 0.93);
        assert!(replay.evidence.reference_burst_envelope.is_some());
        assert!(replay.evidence.offending_burst_envelope.is_some());

        let rogue = state
            .alerts
            .iter()
            .find(|a| a.category == AlertCategory::RogueTransmitter)
            .unwrap_or_else(|| {
                panic!(
                    "missing rogue alert; alerts={:?}, bursts={:?}",
                    state.alerts, state.bursts
                )
            });
        assert_eq!(rogue.severity, AlertSeverity::Warning);
        Ok(())
    }
}