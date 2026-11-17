use std::f32::consts::{PI, SQRT_2, TAU};
use std::time::Duration;

use clap::ValueEnum;
use num_complex::Complex;
use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};
use rand_distr::{Distribution, Normal};

use crate::error::SpectraError;
use crate::sdr::{IqChunk, RfSource};

/// Baseband RF physical-layer simulation scenarios for passive WIDS validation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Scenario {
    Clean,
    Jamming,
    Replay,
    Rogue,
}

impl Scenario {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Clean => "clean",
            Self::Jamming => "jamming",
            Self::Replay => "replay",
            Self::Rogue => "rogue",
        }
    }
}

/// Deterministic mathematical baseband IQ synthesizer implementing `RfSource`.
pub struct SimulatedRfSource {
    center_freq_hz: u64,
    sample_rate_hz: u32,
    chunk_size: usize,
    scenario: Scenario,
    realtime_pacing: bool,
    emitted_samples: u64,
    rng: StdRng,
    thermal_noise: Normal<f32>,
}

impl SimulatedRfSource {
    pub fn new(
        center_freq_hz: u64,
        sample_rate_hz: u32,
        chunk_size: usize,
        scenario: Scenario,
        realtime_pacing: bool,
    ) -> Result<Self, SpectraError> {
        if center_freq_hz == 0 {
            return Err(SpectraError::InvalidFrequency {
                freq_hz: center_freq_hz,
                reason: "center frequency must be greater than zero".to_string(),
            });
        }
        if sample_rate_hz == 0 || chunk_size == 0 {
            return Err(SpectraError::InvalidDspConfig(
                "sample_rate_hz and chunk_size must be greater than zero".to_string(),
            ));
        }

        let noise_floor_dbfs = -92.0_f32;
        let noise_rms = 10.0_f32.powf(noise_floor_dbfs / 20.0) / SQRT_2;
        let thermal_noise = Normal::new(0.0, noise_rms)
            .map_err(|err| SpectraError::InvalidDspConfig(err.to_string()))?;

        Ok(Self {
            center_freq_hz,
            sample_rate_hz,
            chunk_size,
            scenario,
            realtime_pacing,
            emitted_samples: 0,
            rng: StdRng::from_entropy(),
            thermal_noise,
        })
    }

    fn synthesize_sample(&mut self, sample_index: u64) -> Complex<f32> {
        let fs = self.sample_rate_hz as f32;
        let t_sec = sample_index as f32 / fs;

        let mut iq = Complex::new(
            self.thermal_noise.sample(&mut self.rng),
            self.thermal_noise.sample(&mut self.rng),
        );

        let frame_period_samples = (self.sample_rate_hz / 4) as u64;
        let burst_len_samples = (self.sample_rate_hz / 80) as u64;
        let phase_in_frame = sample_index % frame_period_samples;

        if phase_in_frame < burst_len_samples {
            let bit_index = (phase_in_frame * 64) / burst_len_samples;
            let preamble = bit_index < 16 && (bit_index % 2 == 0);
            let payload_pattern = ((0xA5C3_5A3C_u32 >> (bit_index % 32)) & 1) == 1;
            if preamble || payload_pattern {
                let carrier_phase = TAU * 15_000.0 * t_sec;
                iq += Complex::from_polar(0.08, carrier_phase);
            }
        }

        match self.scenario {
            Scenario::Clean => {}
            Scenario::Jamming => {
                let sweep_hz = 250_000.0 * (TAU * 3.5 * t_sec).sin();
                let jam_phase = TAU * sweep_hz * t_sec;
                let broadband_i = self.rng.gen_range(-0.18_f32..0.18_f32);
                let broadband_q = self.rng.gen_range(-0.18_f32..0.18_f32);
                iq += Complex::from_polar(0.38, jam_phase) + Complex::new(broadband_i, broadband_q);
            }
            Scenario::Replay => {
                let replay_offset = burst_len_samples + (self.sample_rate_hz / 100) as u64;
                if phase_in_frame >= replay_offset
                    && phase_in_frame < replay_offset + burst_len_samples
                {
                    let local_pos = phase_in_frame - replay_offset;
                    let bit_index = (local_pos * 64) / burst_len_samples;
                    let preamble = bit_index < 16 && (bit_index % 2 == 0);
                    let payload_pattern = ((0xA5C3_5A3C_u32 >> (bit_index % 32)) & 1) == 1;
                    if preamble || payload_pattern {
                        let carrier_phase = TAU * 15_000.0 * t_sec + PI * 0.25;
                        iq += Complex::from_polar(0.14, carrier_phase);
                    }
                }
            }
            Scenario::Rogue => {
                let rogue_period = (self.sample_rate_hz / 6) as u64;
                let rogue_duration = (self.sample_rate_hz / 22) as u64;
                let rogue_pos = sample_index % rogue_period;
                if rogue_pos < rogue_duration {
                    let symbol = ((rogue_pos * 96) / rogue_duration) % 2;
                    let fsk_dev_hz = if symbol == 0 { -35_000.0 } else { 35_000.0 };
                    let rogue_freq_hz = 180_000.0 + fsk_dev_hz;
                    let rogue_phase = TAU * rogue_freq_hz * t_sec;
                    iq += Complex::from_polar(0.28, rogue_phase);
                }
            }
        }

        Complex::new(iq.re.clamp(-1.0, 1.0), iq.im.clamp(-1.0, 1.0))
    }
}

impl RfSource for SimulatedRfSource {
    async fn next_chunk(&mut self) -> Result<IqChunk, SpectraError> {
        let mut samples = Vec::with_capacity(self.chunk_size);
        let start_sample = self.emitted_samples;

        for idx in 0..self.chunk_size {
            samples.push(self.synthesize_sample(start_sample + idx as u64));
        }

        let timestamp_ns = ((self.emitted_samples as u128 * 1_000_000_000u128)
            / self.sample_rate_hz as u128) as u64;
        self.emitted_samples = self.emitted_samples.saturating_add(self.chunk_size as u64);

        if self.realtime_pacing {
            let chunk_duration_sec = self.chunk_size as f64 / self.sample_rate_hz as f64;
            tokio::time::sleep(Duration::from_secs_f64(chunk_duration_sec)).await;
        }

        Ok(IqChunk {
            samples,
            center_freq_hz: self.center_freq_hz,
            sample_rate_hz: self.sample_rate_hz,
            timestamp_ns,
        })
    }

    fn center_freq_hz(&self) -> u64 {
        self.center_freq_hz
    }

    fn sample_rate_hz(&self) -> u32 {
        self.sample_rate_hz
    }

    fn set_center_freq(&mut self, freq_hz: u64) -> Result<(), SpectraError> {
        if freq_hz == 0 {
            return Err(SpectraError::InvalidFrequency {
                freq_hz,
                reason: "center frequency must be greater than zero".to_string(),
            });
        }
        self.center_freq_hz = freq_hz;
        Ok(())
    }
}