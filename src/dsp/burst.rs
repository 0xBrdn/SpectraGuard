use std::f32::consts::TAU;

use num_complex::Complex;
use serde::{Deserialize, Serialize};

use crate::error::SpectraError;
use crate::sdr::IqChunk;

/// Physical-layer modulation classification hint derived from envelope and phase dynamics.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ModulationType {
    Ook,
    Fsk2,
    ContinuousCarrier,
    Unknown,
}

/// Extracted physical telemetry and envelope signature for a completed RF transmission burst.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DetectedBurst {
    pub id: u64,
    pub timestamp_ns: u64,
    pub duration_us: u64,
    pub center_offset_hz: f32,
    pub peak_snr_db: f32,
    pub mean_rssi_dbfs: f32,
    pub modulation_hint: ModulationType,
    pub estimated_baud_rate: u32,
    pub envelope_signature: Vec<f32>,
    pub bitstream_hex: String,
}

/// Configuration for hysteresis squelch burst detection and hangover bridging.
#[derive(Debug, Clone, Copy)]
pub struct BurstDetectorConfig {
    pub fft_size: usize,
    pub snr_on_db: f32,
    pub snr_off_db: f32,
    pub hangover_ms: f32,
    pub min_burst_duration_us: u64,
    pub max_burst_duration_ms: u64,
}

impl Default for BurstDetectorConfig {
    fn default() -> Self {
        Self {
            fft_size: 2048,
            snr_on_db: 10.0,
            snr_off_db: 6.0,
            hangover_ms: 2.0,
            min_burst_duration_us: 250,
            max_burst_duration_ms: 2_000,
        }
    }
}

/// Stateful time-domain hysteresis power squelch detector with OOK hangover bridging.
pub struct BurstDetector {
    config: BurstDetectorConfig,
    next_burst_id: u64,
    smoothed_wideband_noise_dbfs: Option<f32>,
    env_power_linear: f32,
    in_burst: bool,
    burst_start_ns: u64,
    burst_sample_rate_hz: u32,
    burst_ref_noise_dbfs: f32,
    hangover_counter: usize,
    active_samples: Vec<Complex<f32>>,
    active_powers: Vec<f32>,
}

impl BurstDetector {
    pub fn new(config: BurstDetectorConfig) -> Result<Self, SpectraError> {
        if config.fft_size == 0 {
            return Err(SpectraError::InvalidDspConfig(
                "fft_size must be greater than zero".to_string(),
            ));
        }
        if config.snr_on_db <= config.snr_off_db {
            return Err(SpectraError::InvalidDspConfig(
                "snr_on_db must exceed snr_off_db for hysteresis".to_string(),
            ));
        }
        if config.hangover_ms < 0.0 {
            return Err(SpectraError::InvalidDspConfig(
                "hangover_ms must be non-negative".to_string(),
            ));
        }

        Ok(Self {
            config,
            next_burst_id: 1,
            smoothed_wideband_noise_dbfs: None,
            env_power_linear: 1e-12,
            in_burst: false,
            burst_start_ns: 0,
            burst_sample_rate_hz: 0,
            burst_ref_noise_dbfs: -92.0,
            hangover_counter: 0,
            active_samples: Vec::with_capacity(65_536),
            active_powers: Vec::with_capacity(65_536),
        })
    }

    pub fn process_chunk(
        &mut self,
        chunk: &IqChunk,
        per_bin_noise_floor_dbfs: f32,
    ) -> Result<Vec<DetectedBurst>, SpectraError> {
        if chunk.sample_rate_hz == 0 {
            return Err(SpectraError::InvalidDspConfig(
                "chunk sample_rate_hz must be greater than zero".to_string(),
            ));
        }

        let processing_gain_db = 10.0 * (self.config.fft_size as f32).log10();
        let wideband_noise_dbfs = per_bin_noise_floor_dbfs + processing_gain_db;
        let smoothed_noise_dbfs = match self.smoothed_wideband_noise_dbfs {
            Some(prev) => 0.85 * prev + 0.15 * wideband_noise_dbfs,
            None => wideband_noise_dbfs,
        };
        self.smoothed_wideband_noise_dbfs = Some(smoothed_noise_dbfs);

        let threshold_on_dbfs = smoothed_noise_dbfs + self.config.snr_on_db;
        let threshold_off_dbfs = smoothed_noise_dbfs + self.config.snr_off_db;

        let fs = chunk.sample_rate_hz as f32;
        let env_tau_sec = 15e-6_f32;
        let env_alpha = (1.0 - (-1.0 / (fs * env_tau_sec)).exp()).clamp(0.005, 1.0);
        let hangover_samples = ((self.config.hangover_ms / 1000.0) * fs).round() as usize;
        let max_burst_samples =
            ((self.config.max_burst_duration_ms * chunk.sample_rate_hz as u64) / 1000) as usize;

        let mut completed = Vec::new();

        for (idx, &sample) in chunk.samples.iter().enumerate() {
            let inst_power = sample.re * sample.re + sample.im * sample.im;
            self.env_power_linear += env_alpha * (inst_power - self.env_power_linear);
            let env_dbfs = 10.0 * self.env_power_linear.max(1e-15).log10();

            if !self.in_burst {
                if env_dbfs >= threshold_on_dbfs {
                    self.in_burst = true;
                    self.burst_start_ns = chunk.timestamp_ns
                        + ((idx as u128 * 1_000_000_000u128) / chunk.sample_rate_hz as u128)
                            as u64;
                    self.burst_sample_rate_hz = chunk.sample_rate_hz;
                    self.burst_ref_noise_dbfs = smoothed_noise_dbfs;
                    self.hangover_counter = 0;
                    self.active_samples.clear();
                    self.active_powers.clear();
                    self.active_samples.push(sample);
                    self.active_powers.push(inst_power);
                }
            } else {
                self.active_samples.push(sample);
                self.active_powers.push(inst_power);

                if env_dbfs >= threshold_off_dbfs {
                    self.hangover_counter = 0;
                } else {
                    self.hangover_counter += 1;
                    if self.hangover_counter >= hangover_samples {
                        let trim_len = self
                            .active_samples
                            .len()
                            .saturating_sub(self.hangover_counter);
                        if let Some(burst) = self.finalize_burst(trim_len, false) {
                            completed.push(burst);
                        }
                        self.in_burst = false;
                        self.hangover_counter = 0;
                        self.active_samples.clear();
                        self.active_powers.clear();
                        continue;
                    }
                }

                if self.active_samples.len() >= max_burst_samples {
                    let len = self.active_samples.len();
                    if let Some(burst) = self.finalize_burst(len, true) {
                        completed.push(burst);
                    }
                    let next_sample_idx = idx + 1;
                    self.burst_start_ns = chunk.timestamp_ns
                        + ((next_sample_idx as u128 * 1_000_000_000u128)
                            / chunk.sample_rate_hz as u128) as u64;
                    self.burst_ref_noise_dbfs = smoothed_noise_dbfs;
                    self.active_samples.clear();
                    self.active_powers.clear();
                }
            }
        }

        Ok(completed)
    }

    fn finalize_burst(
        &mut self,
        valid_len: usize,
        forced_continuous: bool,
    ) -> Option<DetectedBurst> {
        if valid_len < 16 {
            return None;
        }

        let sample_rate_hz = self.burst_sample_rate_hz;
        let duration_us =
            ((valid_len as u128 * 1_000_000u128) / sample_rate_hz as u128) as u64;
        if duration_us < self.config.min_burst_duration_us {
            return None;
        }

        let samples = &self.active_samples[..valid_len];
        let powers = &self.active_powers[..valid_len];

        let mut peak_linear = 0.0_f32;
        let mut sum_linear = 0.0_f64;
        for &p in powers {
            if p > peak_linear {
                peak_linear = p;
            }
            sum_linear += p as f64;
        }

        let mean_linear = (sum_linear / valid_len as f64) as f32;
        let peak_dbfs = 10.0 * peak_linear.max(1e-15).log10();
        let peak_snr_db = (peak_dbfs - self.burst_ref_noise_dbfs).max(0.0);
        let mean_rssi_dbfs = 10.0 * mean_linear.max(1e-15).log10();

        let (center_offset_hz, phase_coherence) =
            Self::estimate_carrier_offset_hz(samples, sample_rate_hz);

        let smooth_win = ((sample_rate_hz / 128_000) as usize).clamp(4, 32);
        let smooth_env = Self::smooth_amplitude(powers, smooth_win);
        let (norm_env, env_min, env_max) = Self::normalize_envelope(&smooth_env);

        let envelope_signature = Self::downsample_signature(&norm_env, 128);
        let (ook_transitions, ook_runs) = Self::extract_level_runs(&norm_env, 0.55, 0.45);

        let env_mean = smooth_env.iter().sum::<f32>() / (smooth_env.len() as f32);
        let env_var = smooth_env
            .iter()
            .map(|&v| {
                let d = v - env_mean;
                d * d
            })
            .sum::<f32>()
            / (smooth_env.len() as f32);
        let env_cv = if env_mean > 1e-9 {
            env_var.sqrt() / env_mean
        } else {
            0.0
        };
        let modulation_depth = if env_max > 1e-9 {
            (env_max - env_min) / env_max
        } else {
            0.0
        };

        let (freq_dev_rms_hz, fsk_transitions, fsk_runs) = Self::analyze_instantaneous_frequency(
            samples,
            sample_rate_hz,
            center_offset_hz,
            smooth_win,
        );

        let modulation_hint = if forced_continuous {
            if phase_coherence < 0.15 {
                ModulationType::Unknown
            } else {
                ModulationType::ContinuousCarrier
            }
        } else if phase_coherence < 0.15 && env_cv < 0.35 {
            ModulationType::Unknown
        } else if modulation_depth > 0.50 && env_cv > 0.28 && ook_transitions >= 4 {
            ModulationType::Ook
        } else if freq_dev_rms_hz > 8_000.0 && fsk_transitions >= 4 {
            ModulationType::Fsk2
        } else if freq_dev_rms_hz < 6_000.0 && env_cv < 0.28 {
            ModulationType::ContinuousCarrier
        } else {
            ModulationType::Unknown
        };

        let active_runs = match modulation_hint {
            ModulationType::Fsk2 => &fsk_runs,
            _ => &ook_runs,
        };
        let estimated_baud_rate = Self::estimate_baud_from_runs(active_runs, sample_rate_hz);
        let bitstream_hex = Self::pack_bitstream_hex(&norm_env, sample_rate_hz, estimated_baud_rate);

        let id = self.next_burst_id;
        self.next_burst_id += 1;

        Some(DetectedBurst {
            id,
            timestamp_ns: self.burst_start_ns,
            duration_us,
            center_offset_hz,
            peak_snr_db,
            mean_rssi_dbfs,
            modulation_hint,
            estimated_baud_rate,
            envelope_signature,
            bitstream_hex,
        })
    }

    fn estimate_carrier_offset_hz(samples: &[Complex<f32>], sample_rate_hz: u32) -> (f32, f32) {
        if samples.len() < 2 {
            return (0.0, 0.0);
        }

        let mut corr_sum = Complex::new(0.0_f64, 0.0_f64);
        let mut energy_sum = 0.0_f64;

        for pair in samples.windows(2) {
            let s0 = pair[0];
            let s1 = pair[1];
            let prod_re = (s1.re as f64) * (s0.re as f64) + (s1.im as f64) * (s0.im as f64);
            let prod_im = (s1.im as f64) * (s0.re as f64) - (s1.re as f64) * (s0.im as f64);
            corr_sum.re += prod_re;
            corr_sum.im += prod_im;
            energy_sum += (s1.re as f64) * (s1.re as f64) + (s1.im as f64) * (s1.im as f64);
        }

        let coherence = if energy_sum > 1e-18 {
            (corr_sum.norm() / energy_sum).clamp(0.0, 1.0) as f32
        } else {
            0.0
        };

        let angle_rad = corr_sum.im.atan2(corr_sum.re) as f32;
        let offset_hz = (angle_rad / TAU) * (sample_rate_hz as f32);
        (offset_hz, coherence)
    }

    fn smooth_amplitude(powers: &[f32], window_len: usize) -> Vec<f32> {
        let n = powers.len();
        let w = window_len.max(1).min(n);
        let mut out = Vec::with_capacity(n);
        let mut acc = 0.0_f32;

        for i in 0..n {
            acc += powers[i].max(0.0).sqrt();
            if i >= w {
                acc -= powers[i - w].max(0.0).sqrt();
                out.push(acc / (w as f32));
            } else {
                out.push(acc / ((i + 1) as f32));
            }
        }
        out
    }

    fn normalize_envelope(smooth_env: &[f32]) -> (Vec<f32>, f32, f32) {
        let mut sorted = smooth_env.to_vec();
        sorted.sort_by(|a, b| a.total_cmp(b));
        let len = sorted.len();
        let low = sorted[len / 20];
        let high = sorted[(len * 19) / 20];
        let span = (high - low).max(1e-9);

        let norm = smooth_env
            .iter()
            .map(|&v| ((v - low) / span).clamp(0.0, 1.0))
            .collect();
        (norm, low, high)
    }

    fn downsample_signature(norm_env: &[f32], target_len: usize) -> Vec<f32> {
        let src_len = norm_env.len();
        if src_len == 0 || target_len == 0 {
            return vec![0.0; target_len];
        }

        let mut sig = Vec::with_capacity(target_len);
        if src_len >= target_len {
            for i in 0..target_len {
                let start = (i * src_len) / target_len;
                let end = (((i + 1) * src_len) / target_len).max(start + 1).min(src_len);
                let sum: f32 = norm_env[start..end].iter().sum();
                sig.push(sum / ((end - start) as f32));
            }
        } else {
            for i in 0..target_len {
                let pos = (i as f32) * ((src_len - 1) as f32) / ((target_len - 1).max(1) as f32);
                let idx0 = pos.floor() as usize;
                let idx1 = (idx0 + 1).min(src_len - 1);
                let frac = pos - (idx0 as f32);
                sig.push(norm_env[idx0] * (1.0 - frac) + norm_env[idx1] * frac);
            }
        }

        let min_v = sig.iter().copied().fold(f32::INFINITY, f32::min);
        let max_v = sig.iter().copied().fold(f32::NEG_INFINITY, f32::max);
        let span = (max_v - min_v).max(1e-9);
        for val in &mut sig {
            *val = ((*val - min_v) / span).clamp(0.0, 1.0);
        }
        sig
    }

    fn extract_level_runs(
        signal: &[f32],
        high_thresh: f32,
        low_thresh: f32,
    ) -> (usize, Vec<usize>) {
        if signal.is_empty() {
            return (0, Vec::new());
        }

        let mut state = signal[0] >= high_thresh;
        let mut transitions = 0usize;
        let mut current_run = 0usize;
        let mut runs = Vec::new();

        for &val in signal {
            let next_state = if state {
                val >= low_thresh
            } else {
                val >= high_thresh
            };

            if next_state == state {
                current_run += 1;
            } else {
                if transitions > 0 && current_run >= 4 {
                    runs.push(current_run);
                }
                transitions += 1;
                state = next_state;
                current_run = 1;
            }
        }

        (transitions, runs)
    }

    fn analyze_instantaneous_frequency(
        samples: &[Complex<f32>],
        sample_rate_hz: u32,
        center_offset_hz: f32,
        smooth_win: usize,
    ) -> (f32, usize, Vec<usize>) {
        if samples.len() < 4 {
            return (0.0, 0, Vec::new());
        }

        let fs = sample_rate_hz as f32;
        let scale = fs / TAU;
        let mut inst_dev = Vec::with_capacity(samples.len() - 1);

        for pair in samples.windows(2) {
            let s0 = pair[0];
            let s1 = pair[1];
            let prod_re = s1.re * s0.re + s1.im * s0.im;
            let prod_im = s1.im * s0.re - s1.re * s0.im;
            let freq_hz = prod_im.atan2(prod_re) * scale - center_offset_hz;
            inst_dev.push(freq_hz);
        }

        let w = smooth_win.max(1).min(inst_dev.len());
        let mut smoothed_dev = Vec::with_capacity(inst_dev.len());
        let mut acc = 0.0_f32;
        for i in 0..inst_dev.len() {
            acc += inst_dev[i];
            if i >= w {
                acc -= inst_dev[i - w];
                smoothed_dev.push(acc / (w as f32));
            } else {
                smoothed_dev.push(acc / ((i + 1) as f32));
            }
        }

        let dev_sq_sum: f32 = smoothed_dev.iter().map(|&f| f * f).sum();
        let dev_rms_hz = (dev_sq_sum / (smoothed_dev.len() as f32)).sqrt();

        let (transitions, runs) = Self::extract_level_runs(&smoothed_dev, 3_500.0, -3_500.0);
        (dev_rms_hz, transitions, runs)
    }

    fn estimate_baud_from_runs(runs: &[usize], sample_rate_hz: u32) -> u32 {
        if runs.is_empty() {
            return 0;
        }

        let mut sorted = runs.to_vec();
        sorted.sort_unstable();
        let base_idx = (sorted.len() / 6).min(sorted.len() - 1);
        let base_run = sorted[base_idx] as f32;
        let upper_bound = (base_run * 1.35).ceil() as usize;

        let cluster: Vec<usize> = sorted
            .into_iter()
            .filter(|&r| r <= upper_bound)
            .collect();

        let symbol_samples = if cluster.is_empty() {
            base_run
        } else {
            cluster[cluster.len() / 2] as f32
        };

        if symbol_samples <= 0.0 {
            0
        } else {
            ((sample_rate_hz as f32) / symbol_samples).round() as u32
        }
    }

    fn pack_bitstream_hex(norm_env: &[f32], sample_rate_hz: u32, baud_rate: u32) -> String {
        if norm_env.is_empty() {
            return String::new();
        }

        let samples_per_bit = if baud_rate >= 300 {
            ((sample_rate_hz as f32) / (baud_rate as f32))
                .round()
                .max(1.0) as usize
        } else {
            (norm_env.len() / 64).max(1)
        };

        let max_bits = 512usize;
        let num_bits = (norm_env.len() / samples_per_bit).clamp(1, max_bits);
        let mut bits = Vec::with_capacity(num_bits);

        for b in 0..num_bits {
            let start = b * samples_per_bit;
            let end = ((b + 1) * samples_per_bit).min(norm_env.len());
            let quarter = (end - start) / 4;
            let mid_start = start + quarter;
            let mid_end = (end - quarter).max(mid_start + 1);
            let avg: f32 =
                norm_env[mid_start..mid_end].iter().sum::<f32>() / ((mid_end - mid_start) as f32);
            bits.push(u8::from(avg >= 0.5));
        }

        let byte_len = bits.len().div_ceil(8);
        let mut hex = String::with_capacity(byte_len * 2);
        const HEX_CHARS: &[u8; 16] = b"0123456789ABCDEF";

        for byte_chunk in bits.chunks(8) {
            let mut byte_val = 0u8;
            for (bit_pos, &bit) in byte_chunk.iter().enumerate() {
                byte_val |= bit << (7 - bit_pos);
            }
            hex.push(HEX_CHARS[(byte_val >> 4) as usize] as char);
            hex.push(HEX_CHARS[(byte_val & 0x0F) as usize] as char);
        }

        hex
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sdr::RfSource;
    use crate::sdr::simulator::{SimulatedRfSource, SimulatorConfig};

    #[tokio::test]
    async fn detects_synthetic_ook_and_fsk_bursts() -> Result<(), SpectraError> {
        let sample_rate_hz = 2_048_000;
        let fft_size = 2048;
        let chunk_size = 32_768;

        let (mut sim, _tx) = SimulatedRfSource::new(
            868_300_000,
            sample_rate_hz,
            chunk_size,
            SimulatorConfig::default(),
            false,
        )?;

        let mut detector = BurstDetector::new(BurstDetectorConfig {
            fft_size,
            ..BurstDetectorConfig::default()
        })?;

        let mut detected = Vec::new();
        for _ in 0..35 {
            let chunk = sim.next_chunk().await?;
            let bursts = detector.process_chunk(&chunk, -92.0)?;
            detected.extend(bursts);
        }

        assert!(detected.len() >= 2);
        let first = &detected[0];
        assert_eq!(first.envelope_signature.len(), 128);
        assert!(!first.bitstream_hex.is_empty());
        assert!(first.peak_snr_db > 15.0);
        Ok(())
    }
}