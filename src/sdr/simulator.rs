use std::collections::VecDeque;
use std::f32::consts::{PI as PI_F32, SQRT_2};
use std::f64::consts::TAU;
use std::time::Duration;

use clap::ValueEnum;
use num_complex::Complex;
use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};
use rand_distr::{Distribution, Normal};
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;
use tokio::time::{Interval, MissedTickBehavior};

use crate::error::SpectraError;
use crate::sdr::{IqChunk, RfSource};

/// Predefined offline baseband synthesis profiles for SigMF dataset generation.
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

/// Real-time baseband anomaly injection commands sent over an `mpsc` channel.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ThreatCommand {
    InjectBroadbandJammer {
        duration_ms: u64,
        power_offset_db: f32,
    },
    InjectNarrowbandJammer {
        freq_offset_hz: f32,
        duration_ms: u64,
        power_db: f32,
    },
    InjectReplayAttack {
        repeat_count: u8,
        interval_ms: u64,
    },
    InjectRogueTransmitter {
        freq_offset_hz: f32,
        baud_rate: u32,
        duration_ms: u64,
    },
}

/// Physical front-end parameters for the synthetic zero-IF SDR receiver.
#[derive(Debug, Clone, Copy)]
pub struct SimulatorConfig {
    pub noise_floor_dbfs: f32,
    pub reference_fft_size: usize,
    pub dc_spike_offset_db: f32,
    pub iq_gain_imbalance: f32,
    pub iq_phase_imbalance_rad: f32,
    pub seed: u64,
}

impl Default for SimulatorConfig {
    fn default() -> Self {
        Self {
            noise_floor_dbfs: -92.0,
            reference_fft_size: 2048,
            dc_spike_offset_db: 3.0,
            iq_gain_imbalance: 0.015,
            iq_phase_imbalance_rad: 0.021,
            seed: 0x5350_4543_5452_4131,
        }
    }
}

#[derive(Debug, Clone, Copy)]
enum NodeModulation {
    Ook,
    Fsk2 { deviation_hz: f64 },
}

struct IndustrialNode {
    node_id: u8,
    freq_offset_hz: f64,
    modulation: NodeModulation,
    baud_rate_hz: u32,
    burst_duration_ms: u64,
    period_ms: u64,
    amplitude_linear: f32,
    phase_rad: f64,
    shaped_symbol: f32,
    shaping_alpha: f32,
    next_tx_sample: u64,
    tx_counter: u32,
    active_bits: Vec<u8>,
    active_burst_samples: u64,
    burst_elapsed_samples: u64,
    capture_buffer: Vec<Complex<f32>>,
}

impl IndustrialNode {
    #[allow(clippy::too_many_arguments)]
    fn new(
        node_id: u8,
        freq_offset_hz: f64,
        modulation: NodeModulation,
        baud_rate_hz: u32,
        burst_duration_ms: u64,
        period_ms: u64,
        amplitude_dbfs: f32,
        initial_delay_ms: u64,
        sample_rate_hz: u32,
    ) -> Self {
        let fs = sample_rate_hz as f32;
        let filter_bw_hz = (baud_rate_hz as f32) * 1.35;
        let shaping_alpha = (1.0 - (-2.0 * PI_F32 * filter_bw_hz / fs).exp()).clamp(0.001, 1.0);
        let next_tx_sample = (initial_delay_ms * sample_rate_hz as u64) / 1000;

        Self {
            node_id,
            freq_offset_hz,
            modulation,
            baud_rate_hz,
            burst_duration_ms,
            period_ms,
            amplitude_linear: 10.0_f32.powf(amplitude_dbfs / 20.0),
            phase_rad: 0.0,
            shaped_symbol: 0.0,
            shaping_alpha,
            next_tx_sample,
            tx_counter: 0,
            active_bits: Vec::new(),
            active_burst_samples: 0,
            burst_elapsed_samples: 0,
            capture_buffer: Vec::new(),
        }
    }

    fn build_packet_bits(&mut self) -> Vec<u8> {
        self.tx_counter = self.tx_counter.wrapping_add(1);
        let rolling_code = self.tx_counter.wrapping_mul(0x9E37_79B9) ^ (self.node_id as u32 * 0x45D9_F3B);
        let sensor_reading = 2150u16.wrapping_add(((self.tx_counter * 17) % 180) as u16);

        let mut bytes = Vec::with_capacity(10);
        bytes.push(0xAA);
        bytes.push(0xAA);
        bytes.push(0x2D);
        bytes.push(0xD4);
        bytes.push(self.node_id);
        bytes.extend_from_slice(&((rolling_code >> 16) as u16).to_be_bytes());
        bytes.extend_from_slice(&sensor_reading.to_be_bytes());
        let checksum = bytes.iter().fold(0u8, |acc, &b| acc ^ b);
        bytes.push(checksum);

        let total_bits = ((self.burst_duration_ms * self.baud_rate_hz as u64) / 1000).max(32) as usize;
        let mut bits = Vec::with_capacity(total_bits);
        for i in 0..total_bits {
            let byte = bytes[(i / 8) % bytes.len()];
            let bit = (byte >> (7 - (i % 8))) & 1;
            bits.push(bit);
        }
        bits
    }

    fn step(
        &mut self,
        sample_index: u64,
        sample_rate_hz: u32,
        rng: &mut StdRng,
    ) -> (Complex<f32>, Option<CapturedBurst>) {
        let fs_f64 = sample_rate_hz as f64;
        let mut completed_burst = None;

        if self.active_burst_samples == 0 && sample_index >= self.next_tx_sample {
            self.active_bits = self.build_packet_bits();
            self.active_burst_samples =
                ((self.burst_duration_ms * sample_rate_hz as u64) / 1000).max(1);
            self.burst_elapsed_samples = 0;
            self.capture_buffer.clear();
            self.capture_buffer.reserve(self.active_burst_samples as usize);
            let jitter_ms = rng.gen_range(0..=15u64);
            self.next_tx_sample = sample_index
                .saturating_add(((self.period_ms + jitter_ms) * sample_rate_hz as u64) / 1000);
        }

        if self.active_burst_samples == 0 {
            self.shaped_symbol += self.shaping_alpha * (0.0 - self.shaped_symbol);
            return (Complex::new(0.0, 0.0), None);
        }

        let bit_idx = ((self.burst_elapsed_samples * self.active_bits.len() as u64)
            / self.active_burst_samples) as usize;
        let bit = self.active_bits[bit_idx.min(self.active_bits.len() - 1)];

        let edge_samples = (sample_rate_hz / 2000).max(1);
        let edge_gain = if self.burst_elapsed_samples < edge_samples as u64 {
            let x = self.burst_elapsed_samples as f32 / edge_samples as f32;
            0.5 * (1.0 - (PI_F32 * x).cos())
        } else if self.active_burst_samples - self.burst_elapsed_samples < edge_samples as u64 {
            let rem = (self.active_burst_samples - self.burst_elapsed_samples) as f32;
            let x = rem / edge_samples as f32;
            0.5 * (1.0 - (PI_F32 * x).cos())
        } else {
            1.0
        };

        let sample_out = match self.modulation {
            NodeModulation::Ook => {
                let target = if bit == 1 { 1.0 } else { 0.0 };
                self.shaped_symbol += self.shaping_alpha * (target - self.shaped_symbol);
                self.phase_rad =
                    (self.phase_rad + TAU * self.freq_offset_hz / fs_f64).rem_euclid(TAU);
                let carrier = Complex::new(self.phase_rad.cos() as f32, self.phase_rad.sin() as f32);
                carrier * (self.amplitude_linear * self.shaped_symbol * edge_gain)
            }
            NodeModulation::Fsk2 { deviation_hz } => {
                let target = if bit == 1 { 1.0 } else { -1.0 };
                self.shaped_symbol += self.shaping_alpha * (target - self.shaped_symbol);
                let inst_freq_hz = self.freq_offset_hz + (self.shaped_symbol as f64) * deviation_hz;
                self.phase_rad = (self.phase_rad + TAU * inst_freq_hz / fs_f64).rem_euclid(TAU);
                let carrier = Complex::new(self.phase_rad.cos() as f32, self.phase_rad.sin() as f32);
                carrier * (self.amplitude_linear * edge_gain)
            }
        };

        self.capture_buffer.push(sample_out);
        self.burst_elapsed_samples += 1;

        if self.burst_elapsed_samples >= self.active_burst_samples {
            completed_burst = Some(CapturedBurst {
                samples: std::mem::take(&mut self.capture_buffer),
            });
            self.active_burst_samples = 0;
            self.burst_elapsed_samples = 0;
            self.shaped_symbol = 0.0;
        }

        (sample_out, completed_burst)
    }
}

#[derive(Clone)]
struct CapturedBurst {
    samples: Vec<Complex<f32>>,
}

struct BroadbandJammerState {
    remaining_samples: u64,
    total_samples: u64,
    gain_linear: f32,
    filter_state: Complex<f32>,
}

struct NarrowbandJammerState {
    freq_offset_hz: f64,
    remaining_samples: u64,
    total_samples: u64,
    amplitude_linear: f32,
    carrier_phase_rad: f64,
    fm_mod_phase_rad: f64,
}

struct ReplayAttackState {
    remaining_repeats: u8,
    interval_samples: u64,
    cooldown_samples: u64,
    playback_index: Option<usize>,
    phase_rotation: Complex<f32>,
}

struct RogueTransmitterState {
    freq_offset_hz: f64,
    baud_rate_hz: u32,
    remaining_samples: u64,
    total_samples: u64,
    amplitude_linear: f32,
    phase_rad: f64,
    shaped_envelope: f32,
    shaping_alpha: f32,
    lfsr: u32,
    samples_in_symbol: u32,
    samples_per_symbol: u32,
    current_bit: u8,
}

/// Deterministic synthetic RF baseband engine implementing `RfSource` with real-time threat injection.
pub struct SimulatedRfSource {
    center_freq_hz: u64,
    sample_rate_hz: u32,
    chunk_size: usize,
    config: SimulatorConfig,
    realtime_pacing: bool,
    pace_interval: Option<Interval>,
    emitted_samples: u64,
    rng: StdRng,
    thermal_noise: Normal<f32>,
    noise_rms: f32,
    dc_offset: Complex<f32>,
    iq_cos_phi: f32,
    iq_sin_phi: f32,
    nodes: Vec<IndustrialNode>,
    last_legitimate_burst: Option<CapturedBurst>,
    threat_rx: mpsc::Receiver<ThreatCommand>,
    broadband_jammers: Vec<BroadbandJammerState>,
    narrowband_jammers: Vec<NarrowbandJammerState>,
    replay_queue: VecDeque<ReplayAttackState>,
    rogue_transmitters: Vec<RogueTransmitterState>,
}

impl SimulatedRfSource {
    pub fn new(
        center_freq_hz: u64,
        sample_rate_hz: u32,
        chunk_size: usize,
        config: SimulatorConfig,
        realtime_pacing: bool,
    ) -> Result<(Self, mpsc::Sender<ThreatCommand>), SpectraError> {
        if center_freq_hz == 0 {
            return Err(SpectraError::InvalidFrequency {
                freq_hz: center_freq_hz,
                reason: "center frequency must be greater than zero".to_string(),
            });
        }
        if sample_rate_hz < 100_000 || chunk_size == 0 {
            return Err(SpectraError::InvalidDspConfig(
                "sample_rate_hz must be >= 100_000 Hz and chunk_size > 0".to_string(),
            ));
        }
        if config.reference_fft_size == 0 {
            return Err(SpectraError::InvalidDspConfig(
                "reference_fft_size must be greater than zero".to_string(),
            ));
        }

        let bin_noise_rms = 10.0_f32.powf(config.noise_floor_dbfs / 20.0);
        let noise_rms = bin_noise_rms * (config.reference_fft_size as f32).sqrt();
        let component_std = noise_rms / SQRT_2;
        let thermal_noise = Normal::new(0.0, component_std)
            .map_err(|err| SpectraError::InvalidDspConfig(err.to_string()))?;

        let dc_power_ratio = 10.0_f32.powf(config.dc_spike_offset_db / 10.0) - 1.0;
        let dc_mag = bin_noise_rms * dc_power_ratio.max(0.0).sqrt();
        let dc_offset = Complex::from_polar(dc_mag, 0.42);

        let nodes = vec![
            IndustrialNode::new(
                1,
                -350_000.0,
                NodeModulation::Ook,
                4_800,
                18,
                430,
                -26.0,
                40,
                sample_rate_hz,
            ),
            IndustrialNode::new(
                2,
                120_000.0,
                NodeModulation::Fsk2 {
                    deviation_hz: 25_000.0,
                },
                9_600,
                28,
                610,
                -22.0,
                140,
                sample_rate_hz,
            ),
            IndustrialNode::new(
                3,
                480_000.0,
                NodeModulation::Ook,
                3_200,
                36,
                790,
                -24.0,
                260,
                sample_rate_hz,
            ),
        ];

        let (threat_tx, threat_rx) = mpsc::channel(64);

        Ok((
            Self {
                center_freq_hz,
                sample_rate_hz,
                chunk_size,
                config,
                realtime_pacing,
                pace_interval: None,
                emitted_samples: 0,
                rng: StdRng::seed_from_u64(config.seed),
                thermal_noise,
                noise_rms,
                dc_offset,
                iq_cos_phi: config.iq_phase_imbalance_rad.cos(),
                iq_sin_phi: config.iq_phase_imbalance_rad.sin(),
                nodes,
                last_legitimate_burst: None,
                threat_rx,
                broadband_jammers: Vec::new(),
                narrowband_jammers: Vec::new(),
                replay_queue: VecDeque::new(),
                rogue_transmitters: Vec::new(),
            },
            threat_tx,
        ))
    }

    pub fn inject_threat(&mut self, command: ThreatCommand) {
        let fs = self.sample_rate_hz as u64;
        match command {
            ThreatCommand::InjectBroadbandJammer {
                duration_ms,
                power_offset_db,
            } => {
                let total_samples = ((duration_ms * fs) / 1000).max(1);
                let gain_linear =
                    self.noise_rms * 10.0_f32.powf(power_offset_db.clamp(-20.0, 75.0) / 20.0);
                self.broadband_jammers.push(BroadbandJammerState {
                    remaining_samples: total_samples,
                    total_samples,
                    gain_linear,
                    filter_state: Complex::new(0.0, 0.0),
                });
            }
            ThreatCommand::InjectNarrowbandJammer {
                freq_offset_hz,
                duration_ms,
                power_db,
            } => {
                let total_samples = ((duration_ms * fs) / 1000).max(1);
                let target_dbfs = if power_db > 0.0 {
                    (self.config.noise_floor_dbfs + power_db).min(-1.0)
                } else {
                    power_db.clamp(-120.0, -1.0)
                };
                self.narrowband_jammers.push(NarrowbandJammerState {
                    freq_offset_hz: freq_offset_hz as f64,
                    remaining_samples: total_samples,
                    total_samples,
                    amplitude_linear: 10.0_f32.powf(target_dbfs / 20.0),
                    carrier_phase_rad: 0.0,
                    fm_mod_phase_rad: 0.0,
                });
            }
            ThreatCommand::InjectReplayAttack {
                repeat_count,
                interval_ms,
            } => {
                if repeat_count > 0 {
                    let interval_samples = (interval_ms * fs) / 1000;
                    let phase_angle = self.rng.gen_range(0.0..TAU as f32);
                    self.replay_queue.push_back(ReplayAttackState {
                        remaining_repeats: repeat_count,
                        interval_samples,
                        cooldown_samples: 0,
                        playback_index: None,
                        phase_rotation: Complex::from_polar(1.0, phase_angle),
                    });
                }
            }
            ThreatCommand::InjectRogueTransmitter {
                freq_offset_hz,
                baud_rate,
                duration_ms,
            } => {
                let total_samples = ((duration_ms * fs) / 1000).max(1);
                let safe_baud = baud_rate.clamp(300, self.sample_rate_hz / 4);
                let samples_per_symbol = (self.sample_rate_hz / safe_baud).max(1);
                let filter_bw_hz = (safe_baud as f32) * 1.4;
                let shaping_alpha = (1.0
                    - (-2.0 * PI_F32 * filter_bw_hz / self.sample_rate_hz as f32).exp())
                .clamp(0.001, 1.0);
                let lfsr = self.rng.gen::<u32>() | 1;

                self.rogue_transmitters.push(RogueTransmitterState {
                    freq_offset_hz: freq_offset_hz as f64,
                    baud_rate_hz: safe_baud,
                    remaining_samples: total_samples,
                    total_samples,
                    amplitude_linear: 10.0_f32.powf(-18.0_f32 / 20.0),
                    phase_rad: 0.0,
                    shaped_envelope: 0.0,
                    shaping_alpha,
                    lfsr,
                    samples_in_symbol: 0,
                    samples_per_symbol,
                    current_bit: 1,
                });
            }
        }
    }

    fn drain_threat_commands(&mut self) {
        while let Ok(command) = self.threat_rx.try_recv() {
            self.inject_threat(command);
        }
    }

    fn envelope_ramp(elapsed: u64, total: u64, ramp_samples: u64) -> f32 {
        let ramp = ramp_samples.max(1).min(total / 2).max(1);
        if elapsed < ramp {
            0.5 * (1.0 - (PI_F32 * (elapsed as f32 / ramp as f32)).cos())
        } else if total - elapsed < ramp {
            let rem = (total - elapsed) as f32;
            0.5 * (1.0 - (PI_F32 * (rem / ramp as f32)).cos())
        } else {
            1.0
        }
    }

    fn synthesize_sample(&mut self, sample_index: u64) -> Complex<f32> {
        let fs_f64 = self.sample_rate_hz as f64;
        let ramp_samples = (self.sample_rate_hz / 2000) as u64;

        let mut iq = Complex::new(
            self.thermal_noise.sample(&mut self.rng),
            self.thermal_noise.sample(&mut self.rng),
        );

        for node in &mut self.nodes {
            let (node_sample, completed) =
                node.step(sample_index, self.sample_rate_hz, &mut self.rng);
            iq += node_sample;
            if let Some(burst) = completed {
                self.last_legitimate_burst = Some(burst);
            }
        }

        for jammer in &mut self.broadband_jammers {
            let elapsed = jammer.total_samples - jammer.remaining_samples;
            let env = Self::envelope_ramp(elapsed, jammer.total_samples, ramp_samples);
            let white = Complex::new(
                self.rng.gen_range(-1.0_f32..1.0_f32),
                self.rng.gen_range(-1.0_f32..1.0_f32),
            ) * (0.577_350_26 * jammer.gain_linear);
            jammer.filter_state = white * 0.88 + jammer.filter_state * 0.12;
            iq += jammer.filter_state * env;
            jammer.remaining_samples -= 1;
        }
        self.broadband_jammers.retain(|j| j.remaining_samples > 0);

        for jammer in &mut self.narrowband_jammers {
            let elapsed = jammer.total_samples - jammer.remaining_samples;
            let env = Self::envelope_ramp(elapsed, jammer.total_samples, ramp_samples);
            jammer.fm_mod_phase_rad =
                (jammer.fm_mod_phase_rad + TAU * 130.0 / fs_f64).rem_euclid(TAU);
            let inst_freq_hz = jammer.freq_offset_hz + 2_500.0 * jammer.fm_mod_phase_rad.sin();
            jammer.carrier_phase_rad =
                (jammer.carrier_phase_rad + TAU * inst_freq_hz / fs_f64).rem_euclid(TAU);
            let tone = Complex::new(
                jammer.carrier_phase_rad.cos() as f32,
                jammer.carrier_phase_rad.sin() as f32,
            );
            iq += tone * (jammer.amplitude_linear * env);
            jammer.remaining_samples -= 1;
        }
        self.narrowband_jammers.retain(|j| j.remaining_samples > 0);

        if let (Some(burst), Some(attack)) =
            (&self.last_legitimate_burst, self.replay_queue.front_mut())
        {
            if let Some(idx) = attack.playback_index {
                if idx < burst.samples.len() {
                    iq += burst.samples[idx] * attack.phase_rotation;
                    attack.playback_index = Some(idx + 1);
                } else {
                    attack.playback_index = None;
                    attack.remaining_repeats = attack.remaining_repeats.saturating_sub(1);
                    attack.cooldown_samples = attack.interval_samples;
                }
            } else if attack.remaining_repeats > 0 {
                if attack.cooldown_samples > 0 {
                    attack.cooldown_samples -= 1;
                } else {
                    attack.playback_index = Some(0);
                }
            }
        }
        while self
            .replay_queue
            .front()
            .is_some_and(|attack| attack.remaining_repeats == 0 && attack.playback_index.is_none())
        {
            self.replay_queue.pop_front();
        }

        for rogue in &mut self.rogue_transmitters {
            let elapsed = rogue.total_samples - rogue.remaining_samples;
            let env = Self::envelope_ramp(elapsed, rogue.total_samples, ramp_samples);

            if rogue.samples_in_symbol == 0 {
                let bit = (rogue.lfsr & 1) as u8;
                let feedback = ((rogue.lfsr >> 31) ^ (rogue.lfsr >> 21) ^ (rogue.lfsr >> 1) ^ rogue.lfsr) & 1;
                rogue.lfsr = (rogue.lfsr >> 1) | (feedback << 31);
                rogue.current_bit = bit;
                rogue.samples_per_symbol = (self.sample_rate_hz / rogue.baud_rate_hz).max(1);
            }
            rogue.samples_in_symbol += 1;
            if rogue.samples_in_symbol >= rogue.samples_per_symbol {
                rogue.samples_in_symbol = 0;
            }

            let target = if rogue.current_bit == 1 { 1.0 } else { 0.0 };
            rogue.shaped_envelope += rogue.shaping_alpha * (target - rogue.shaped_envelope);
            rogue.phase_rad =
                (rogue.phase_rad + TAU * rogue.freq_offset_hz / fs_f64).rem_euclid(TAU);
            let carrier = Complex::new(rogue.phase_rad.cos() as f32, rogue.phase_rad.sin() as f32);
            iq += carrier * (rogue.amplitude_linear * rogue.shaped_envelope * env);
            rogue.remaining_samples -= 1;
        }
        self.rogue_transmitters.retain(|r| r.remaining_samples > 0);

        let i_imb = iq.re * (1.0 + self.config.iq_gain_imbalance);
        let q_imb = (1.0 - self.config.iq_gain_imbalance)
            * (iq.im * self.iq_cos_phi + iq.re * self.iq_sin_phi);
        let biased = Complex::new(i_imb, q_imb) + self.dc_offset;

        Complex::new(biased.re.clamp(-1.0, 1.0), biased.im.clamp(-1.0, 1.0))
    }
}

impl RfSource for SimulatedRfSource {
    async fn next_chunk(&mut self) -> Result<IqChunk, SpectraError> {
        if self.realtime_pacing {
            let chunk_period =
                Duration::from_secs_f64(self.chunk_size as f64 / self.sample_rate_hz as f64);
            let interval = self.pace_interval.get_or_insert_with(|| {
                let mut ticker = tokio::time::interval(chunk_period);
                ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);
                ticker
            });
            interval.tick().await;
        }

        self.drain_threat_commands();

        let mut samples = Vec::with_capacity(self.chunk_size);
        let start_sample = self.emitted_samples;

        for idx in 0..self.chunk_size {
            samples.push(self.synthesize_sample(start_sample + idx as u64));
        }

        let timestamp_ns = ((self.emitted_samples as u128 * 1_000_000_000u128)
            / self.sample_rate_hz as u128) as u64;
        self.emitted_samples = self.emitted_samples.saturating_add(self.chunk_size as u64);

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