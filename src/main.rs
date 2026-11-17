mod dsp;
mod error;
mod sdr;

use std::net::SocketAddr;
use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand, ValueEnum};
use tokio::sync::mpsc;
use tracing::{info, warn};
use tracing_subscriber::EnvFilter;

use crate::dsp::{BurstDetector, BurstDetectorConfig, SpectrumAnalyzer, SpectrumConfig};
use crate::error::SpectraError;
use crate::sdr::sigmf::{SigMfAnnotation, SigMfDatatype, SigMfReader, SigMfWriter};
use crate::sdr::simulator::{Scenario, SimulatedRfSource, SimulatorConfig, ThreatCommand};
use crate::sdr::{BandPreset, IqChunk, RfSource};

#[derive(Debug, Parser)]
#[command(
    name = "spectraguard",
    version,
    about = "Passive RF Wireless Intrusion Detection System and Spectrum Anomaly Monitor"
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum SourceKind {
    Sim,
    Sigmf,
}

#[derive(Debug, Subcommand)]
enum Commands {
    Monitor {
        #[arg(long, value_enum, default_value_t = SourceKind::Sim)]
        source: SourceKind,
        #[arg(long)]
        file: Option<PathBuf>,
        #[arg(long, default_value_t = 868)]
        band: u64,
        #[arg(long = "sample-rate", default_value_t = 2_048_000)]
        sample_rate_hz: u32,
        #[arg(long = "fft-size", default_value_t = 2048)]
        fft_size: usize,
        #[arg(long, default_value = "127.0.0.1:8080")]
        bind: SocketAddr,
    },
    GenerateSigmf {
        #[arg(long)]
        output: PathBuf,
        #[arg(long = "duration-sec", default_value_t = 10)]
        duration_sec: u64,
        #[arg(long, default_value_t = 868)]
        band: u64,
        #[arg(long, value_enum, default_value_t = Scenario::Clean)]
        scenario: Scenario,
        #[arg(long = "sample-rate", default_value_t = 2_048_000)]
        sample_rate_hz: u32,
        #[arg(long, value_enum, default_value_t = SigMfDatatype::Cf32Le)]
        datatype: SigMfDatatype,
    },
}

enum ActiveSource {
    Sim(SimulatedRfSource),
    Sigmf(SigMfReader),
}

impl RfSource for ActiveSource {
    async fn next_chunk(&mut self) -> Result<IqChunk, SpectraError> {
        match self {
            Self::Sim(src) => src.next_chunk().await,
            Self::Sigmf(src) => src.next_chunk().await,
        }
    }

    fn center_freq_hz(&self) -> u64 {
        match self {
            Self::Sim(src) => src.center_freq_hz(),
            Self::Sigmf(src) => src.center_freq_hz(),
        }
    }

    fn sample_rate_hz(&self) -> u32 {
        match self {
            Self::Sim(src) => src.sample_rate_hz(),
            Self::Sigmf(src) => src.sample_rate_hz(),
        }
    }

    fn set_center_freq(&mut self, freq_hz: u64) -> Result<(), SpectraError> {
        match self {
            Self::Sim(src) => src.set_center_freq(freq_hz),
            Self::Sigmf(src) => src.set_center_freq(freq_hz),
        }
    }
}

async fn run_monitor(
    source_kind: SourceKind,
    file: Option<PathBuf>,
    band: u64,
    sample_rate_hz: u32,
    fft_size: usize,
    bind: SocketAddr,
) -> Result<()> {
    let mut analyzer = SpectrumAnalyzer::new(SpectrumConfig {
        fft_size,
        ..SpectrumConfig::default()
    })?;
    let mut burst_detector = BurstDetector::new(BurstDetectorConfig {
        fft_size,
        ..BurstDetectorConfig::default()
    })?;

    let band_preset = BandPreset::from_cli_value(band)?;
    let target_freq_hz = band_preset.center_freq_hz();
    let chunk_size = fft_size * 16;

    let (mut source, _threat_tx) = match source_kind {
        SourceKind::Sim => {
            let sim_config = SimulatorConfig {
                reference_fft_size: fft_size,
                ..SimulatorConfig::default()
            };
            let (sim, tx) = SimulatedRfSource::new(
                target_freq_hz,
                sample_rate_hz,
                chunk_size,
                sim_config,
                true,
            )?;
            (ActiveSource::Sim(sim), Some(tx))
        }
        SourceKind::Sigmf => {
            let path = file.ok_or_else(|| {
                SpectraError::InvalidSigMfMetadata(
                    "--file <PATH> is required when --source sigmf is selected".to_string(),
                )
            })?;
            let mut reader = SigMfReader::open(&path, chunk_size, true, true)?;
            if reader.sample_rate_hz() != sample_rate_hz {
                return Err(SpectraError::SampleRateMismatch {
                    expected_hz: sample_rate_hz,
                    actual_hz: reader.sample_rate_hz(),
                }
                .into());
            }
            if reader.center_freq_hz() != target_freq_hz {
                reader.set_center_freq(target_freq_hz)?;
            }
            info!(
                sigmf_version = %reader.metadata().global.version,
                description = reader
                    .metadata()
                    .global
                    .description
                    .as_deref()
                    .unwrap_or("none"),
                "loaded SigMF capture"
            );
            (ActiveSource::Sigmf(reader), None)
        }
    };

    info!(
        center_freq_hz = source.center_freq_hz(),
        sample_rate_hz = source.sample_rate_hz(),
        fft_size,
        bind_addr = %bind,
        "SpectraGuard passive monitor initialized"
    );

    let (tx, mut rx) = mpsc::channel::<Result<IqChunk, SpectraError>>(16);
    tokio::spawn(async move {
        loop {
            let item = source.next_chunk().await;
            let is_err = item.is_err();
            if tx.send(item).await.is_err() || is_err {
                break;
            }
        }
    });

    let mut chunk_count = 0u64;
    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {
                info!(processed_chunks = chunk_count, "shutdown signal received");
                break;
            }
            maybe_chunk = rx.recv() => {
                let chunk = maybe_chunk.ok_or(SpectraError::ChannelClosed)??;
                chunk_count += 1;

                let spectrum = analyzer.analyze_chunk(&chunk)?;
                let bursts = burst_detector.process_chunk(&chunk, spectrum.noise_floor_dbfs)?;

                for burst in &bursts {
                    info!(
                        burst_id = burst.id,
                        duration_us = burst.duration_us,
                        offset_hz = format_args!("{:.1}", burst.center_offset_hz),
                        peak_snr_db = format_args!("{:.1}", burst.peak_snr_db),
                        rssi_dbfs = format_args!("{:.1}", burst.mean_rssi_dbfs),
                        modulation = ?burst.modulation_hint,
                        baud_rate = burst.estimated_baud_rate,
                        bitstream_hex = %burst.bitstream_hex,
                        "burst detected"
                    );
                }

                if chunk_count % 30 == 0 {
                    info!(
                        chunk = chunk_count,
                        noise_floor_dbfs = format_args!("{:.2}", spectrum.noise_floor_dbfs),
                        channel_occupancy_pct = format_args!("{:.2}", spectrum.channel_occupancy_pct),
                        "spectrum frame telemetry"
                    );
                }
            }
        }
    }

    Ok(())
}

async fn run_generate_sigmf(
    output: PathBuf,
    duration_sec: u64,
    band: u64,
    scenario: Scenario,
    sample_rate_hz: u32,
    datatype: SigMfDatatype,
) -> Result<()> {
    if duration_sec == 0 {
        return Err(SpectraError::InvalidDspConfig(
            "duration_sec must be greater than zero".to_string(),
        )
        .into());
    }

    let band_preset = BandPreset::from_cli_value(band)?;
    let center_freq_hz = band_preset.center_freq_hz();
    let total_samples = duration_sec.saturating_mul(sample_rate_hz as u64);
    let chunk_size = 32_768usize;

    let (mut sim, threat_tx) = SimulatedRfSource::new(
        center_freq_hz,
        sample_rate_hz,
        chunk_size,
        SimulatorConfig::default(),
        false,
    )?;

    let description = format!(
        "SpectraGuard synthetic baseband capture (scenario={}, band_hz={})",
        scenario.as_str(),
        center_freq_hz
    );
    let mut writer = SigMfWriter::create(
        &output,
        datatype,
        center_freq_hz,
        sample_rate_hz,
        Some(description),
    )?;

    let trigger_sample = total_samples / 4;
    let secondary_trigger_sample = (total_samples * 3) / 5;
    let mut primary_injected = false;
    let mut secondary_injected = false;

    while writer.written_samples() < total_samples {
        let written = writer.written_samples();

        if !primary_injected && written >= trigger_sample {
            primary_injected = true;
            match scenario {
                Scenario::Clean => {}
                Scenario::Jamming => {
                    let jam_ms = ((duration_sec * 1000) / 4).clamp(250, 2_500);
                    let _ = threat_tx.try_send(ThreatCommand::InjectBroadbandJammer {
                        duration_ms: jam_ms,
                        power_offset_db: 24.0,
                    });
                    let half_bw_hz = (sample_rate_hz as f64) * 0.425;
                    writer.add_annotation(SigMfAnnotation {
                        sample_start: written,
                        sample_count: (jam_ms * sample_rate_hz as u64) / 1000,
                        freq_lower_edge_hz: Some(center_freq_hz as f64 - half_bw_hz),
                        freq_upper_edge_hz: Some(center_freq_hz as f64 + half_bw_hz),
                        label: Some("broadband_jammer".to_string()),
                        comment: Some("Injected +24 dB wideband noise jammer".to_string()),
                    });
                }
                Scenario::Replay => {
                    let _ = threat_tx.try_send(ThreatCommand::InjectReplayAttack {
                        repeat_count: 5,
                        interval_ms: 45,
                    });
                    writer.add_annotation(SigMfAnnotation {
                        sample_start: written,
                        sample_count: (sample_rate_hz as u64) / 2,
                        freq_lower_edge_hz: Some(center_freq_hz as f64 - 400_000.0),
                        freq_upper_edge_hz: Some(center_freq_hz as f64 + 520_000.0),
                        label: Some("replay_attack".to_string()),
                        comment: Some("Injected bit-identical burst replay sequence".to_string()),
                    });
                }
                Scenario::Rogue => {
                    let rogue_ms = ((duration_sec * 1000) / 3).clamp(300, 3_000);
                    let offset_hz = -140_000.0_f32;
                    let _ = threat_tx.try_send(ThreatCommand::InjectRogueTransmitter {
                        freq_offset_hz: offset_hz,
                        baud_rate: 12_500,
                        duration_ms: rogue_ms,
                    });
                    writer.add_annotation(SigMfAnnotation {
                        sample_start: written,
                        sample_count: (rogue_ms * sample_rate_hz as u64) / 1000,
                        freq_lower_edge_hz: Some(center_freq_hz as f64 + offset_hz as f64 - 25_000.0),
                        freq_upper_edge_hz: Some(center_freq_hz as f64 + offset_hz as f64 + 25_000.0),
                        label: Some("rogue_transmitter".to_string()),
                        comment: Some("Unauthorized OOK burst on unallocated offset".to_string()),
                    });
                }
            }
        }

        if !secondary_injected && written >= secondary_trigger_sample && scenario == Scenario::Jamming {
            secondary_injected = true;
            let nb_ms = ((duration_sec * 1000) / 4).clamp(250, 2_500);
            let offset_hz = 120_000.0_f32;
            let _ = threat_tx.try_send(ThreatCommand::InjectNarrowbandJammer {
                freq_offset_hz: offset_hz,
                duration_ms: nb_ms,
                power_db: -14.0,
            });
            writer.add_annotation(SigMfAnnotation {
                sample_start: written,
                sample_count: (nb_ms * sample_rate_hz as u64) / 1000,
                freq_lower_edge_hz: Some(center_freq_hz as f64 + offset_hz as f64 - 15_000.0),
                freq_upper_edge_hz: Some(center_freq_hz as f64 + offset_hz as f64 + 15_000.0),
                label: Some("narrowband_jammer".to_string()),
                comment: Some("Injected narrowband CW/FM channel jammer".to_string()),
            });
        }

        let chunk = sim.next_chunk().await?;
        let remaining = (total_samples - writer.written_samples()) as usize;
        let slice_len = remaining.min(chunk.samples.len());
        writer.write_samples(&chunk.samples[..slice_len])?;
    }

    let written_samples = writer.written_samples();
    writer.finalize().context("failed to finalize SigMF files")?;

    info!(
        output_prefix = %output.display(),
        datatype = %datatype,
        center_freq_hz,
        sample_rate_hz,
        written_samples,
        scenario = scenario.as_str(),
        "SigMF dataset generated"
    );

    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .init();

    let cli = Cli::parse();

    match cli.command {
        Commands::Monitor {
            source,
            file,
            band,
            sample_rate_hz,
            fft_size,
            bind,
        } => {
            if source == SourceKind::Sigmf && file.is_none() {
                warn!("--source sigmf requires --file <PATH>");
            }
            run_monitor(source, file, band, sample_rate_hz, fft_size, bind).await
        }
        Commands::GenerateSigmf {
            output,
            duration_sec,
            band,
            scenario,
            sample_rate_hz,
            datatype,
        } => {
            run_generate_sigmf(
                output,
                duration_sec,
                band,
                scenario,
                sample_rate_hz,
                datatype,
            )
            .await
        }
    }
}