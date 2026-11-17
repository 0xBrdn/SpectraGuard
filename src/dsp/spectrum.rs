use std::f32::consts::TAU;
use std::sync::Arc;

use num_complex::Complex;
use rustfft::{Fft, FftPlanner};
use serde::{Deserialize, Serialize};

use crate::error::SpectraError;
use crate::sdr::IqChunk;

/// Calibrated power spectral density frame with DC centered at `bins_dbfs[fft_size / 2]`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpectrumFrame {
    pub timestamp_ns: u64,
    pub center_freq_hz: u64,
    pub sample_rate_hz: u32,
    pub bins_dbfs: Vec<f32>,
    pub max_hold_dbfs: Vec<f32>,
    pub avg_psd_dbfs: Vec<f32>,
    pub noise_floor_dbfs: f32,
    pub channel_occupancy_pct: f32,
}

/// Configuration parameters for real-time Blackman-Harris PSD analysis.
#[derive(Debug, Clone, Copy)]
pub struct SpectrumConfig {
    pub fft_size: usize,
    pub avg_alpha: f32,
    pub max_hold_decay_db_per_frame: f32,
    pub occupancy_threshold_db: f32,
}

impl Default for SpectrumConfig {
    fn default() -> Self {
        Self {
            fft_size: 2048,
            avg_alpha: 0.2,
            max_hold_decay_db_per_frame: 0.35,
            occupancy_threshold_db: 6.0,
        }
    }
}

/// Real-time FFT spectrum analyzer with preallocated scratch buffers and ordered-statistics noise estimation.
pub struct SpectrumAnalyzer {
    config: SpectrumConfig,
    fft: Arc<dyn Fft<f32>>,
    window: Vec<f32>,
    inv_norm_sq: f32,
    fft_buffer: Vec<Complex<f32>>,
    fft_scratch: Vec<Complex<f32>>,
    frame_linear_accum: Vec<f32>,
    frame_peak_dbfs: Vec<f32>,
    sorted_scratch: Vec<f32>,
    avg_psd_dbfs: Vec<f32>,
    max_hold_dbfs: Vec<f32>,
    initialized: bool,
}

impl SpectrumAnalyzer {
    pub fn new(config: SpectrumConfig) -> Result<Self, SpectraError> {
        let fft_size = config.fft_size;
        if fft_size < 64 || !fft_size.is_power_of_two() {
            return Err(SpectraError::InvalidDspConfig(format!(
                "fft_size {} must be a power of two >= 64",
                fft_size
            )));
        }
        if !(0.0..=1.0).contains(&config.avg_alpha) || config.avg_alpha == 0.0 {
            return Err(SpectraError::InvalidDspConfig(
                "avg_alpha must be in (0.0, 1.0]".to_string(),
            ));
        }

        let mut planner = FftPlanner::<f32>::new();
        let fft = planner.plan_fft_forward(fft_size);
        let scratch_len = fft.get_inplace_scratch_len();

        let n_f32 = fft_size as f32;
        let window: Vec<f32> = (0..fft_size)
            .map(|n| {
                let phase = TAU * (n as f32) / n_f32;
                0.35875 - 0.48829 * phase.cos() + 0.14128 * (2.0 * phase).cos()
                    - 0.01168 * (3.0 * phase).cos()
            })
            .collect();

        let coherent_gain = window.iter().sum::<f32>() / n_f32;
        let total_norm = coherent_gain * n_f32;
        let inv_norm_sq = if total_norm > 0.0 {
            1.0 / (total_norm * total_norm)
        } else {
            1.0 / (n_f32 * n_f32)
        };

        Ok(Self {
            config,
            fft,
            window,
            inv_norm_sq,
            fft_buffer: vec![Complex::new(0.0, 0.0); fft_size],
            fft_scratch: vec![Complex::new(0.0, 0.0); scratch_len],
            frame_linear_accum: vec![0.0; fft_size],
            frame_peak_dbfs: vec![f32::NEG_INFINITY; fft_size],
            sorted_scratch: vec![0.0; fft_size],
            avg_psd_dbfs: vec![-120.0; fft_size],
            max_hold_dbfs: vec![-120.0; fft_size],
            initialized: false,
        })
    }

    pub fn fft_size(&self) -> usize {
        self.config.fft_size
    }

    pub fn reset_max_hold(&mut self) {
        self.max_hold_dbfs.fill(-120.0);
        self.initialized = false;
    }

    pub fn analyze_chunk(&mut self, chunk: &IqChunk) -> Result<SpectrumFrame, SpectraError> {
        let fft_size = self.config.fft_size;
        if chunk.samples.len() < fft_size {
            return Err(SpectraError::InvalidDspConfig(format!(
                "chunk sample count {} is smaller than fft_size {}",
                chunk.samples.len(),
                fft_size
            )));
        }

        self.frame_linear_accum.fill(0.0);
        self.frame_peak_dbfs.fill(f32::NEG_INFINITY);

        let hop_size = fft_size / 2;
        let half = fft_size / 2;
        let mut frame_count = 0u32;
        let mut offset = 0usize;

        while offset + fft_size <= chunk.samples.len() {
            let slice = &chunk.samples[offset..offset + fft_size];
            for ((dst, &sample), &coeff) in self
                .fft_buffer
                .iter_mut()
                .zip(slice.iter())
                .zip(self.window.iter())
            {
                *dst = sample * coeff;
            }

            self.fft
                .process_with_scratch(&mut self.fft_buffer, &mut self.fft_scratch);

            for shifted_idx in 0..fft_size {
                let raw_idx = (shifted_idx + half) & (fft_size - 1);
                let bin = self.fft_buffer[raw_idx];
                let power_linear = (bin.re * bin.re + bin.im * bin.im) * self.inv_norm_sq;
                let power_dbfs = 10.0 * power_linear.max(1e-15).log10();

                self.frame_linear_accum[shifted_idx] += power_linear;
                if power_dbfs > self.frame_peak_dbfs[shifted_idx] {
                    self.frame_peak_dbfs[shifted_idx] = power_dbfs;
                }
            }

            frame_count += 1;
            offset += hop_size;
        }

        let inv_frames = 1.0 / (frame_count.max(1) as f32);
        let mut bins_dbfs = Vec::with_capacity(fft_size);
        let alpha = self.config.avg_alpha;
        let one_minus_alpha = 1.0 - alpha;
        let decay_db = self.config.max_hold_decay_db_per_frame * (frame_count as f32);

        for i in 0..fft_size {
            let mean_linear = self.frame_linear_accum[i] * inv_frames;
            let bin_db = 10.0 * mean_linear.max(1e-15).log10();
            bins_dbfs.push(bin_db);

            if !self.initialized {
                self.avg_psd_dbfs[i] = bin_db;
                self.max_hold_dbfs[i] = self.frame_peak_dbfs[i];
            } else {
                self.avg_psd_dbfs[i] = alpha * bin_db + one_minus_alpha * self.avg_psd_dbfs[i];
                let decayed_hold = self.max_hold_dbfs[i] - decay_db;
                self.max_hold_dbfs[i] = decayed_hold.max(self.frame_peak_dbfs[i]);
            }
        }
        self.initialized = true;

        self.sorted_scratch.copy_from_slice(&bins_dbfs);
        let percentile_idx = fft_size / 4;
        let (_, nth_val, _) = self
            .sorted_scratch
            .select_nth_unstable_by(percentile_idx, |a, b| a.total_cmp(b));
        let noise_floor_dbfs = *nth_val;

        let occ_threshold_dbfs = noise_floor_dbfs + self.config.occupancy_threshold_db;
        let occupied_bins = self
            .frame_peak_dbfs
            .iter()
            .filter(|&&db| db >= occ_threshold_dbfs)
            .count();
        let channel_occupancy_pct = (occupied_bins as f32 / fft_size as f32) * 100.0;

        Ok(SpectrumFrame {
            timestamp_ns: chunk.timestamp_ns,
            center_freq_hz: chunk.center_freq_hz,
            sample_rate_hz: chunk.sample_rate_hz,
            bins_dbfs,
            max_hold_dbfs: self.max_hold_dbfs.clone(),
            avg_psd_dbfs: self.avg_psd_dbfs.clone(),
            noise_floor_dbfs,
            channel_occupancy_pct,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn centers_dc_and_calibrates_full_scale_tone() -> Result<(), SpectraError> {
        let fft_size = 1024;
        let sample_rate_hz = 1_024_000;
        let mut analyzer = SpectrumAnalyzer::new(SpectrumConfig {
            fft_size,
            ..SpectrumConfig::default()
        })?;

        let samples = vec![Complex::new(1.0, 0.0); fft_size * 2];
        let chunk = IqChunk {
            samples,
            center_freq_hz: 868_300_000,
            sample_rate_hz,
            timestamp_ns: 0,
        };

        let frame = analyzer.analyze_chunk(&chunk)?;
        let dc_bin_dbfs = frame.bins_dbfs[fft_size / 2];
        assert!((dc_bin_dbfs - 0.0).abs() < 0.05);
        assert!(frame.noise_floor_dbfs < -90.0);
        Ok(())
    }
}