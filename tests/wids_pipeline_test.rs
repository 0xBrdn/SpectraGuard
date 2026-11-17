use spectraguard::dsp::{
    BurstDetector, BurstDetectorConfig, DetectedBurst, ModulationType, SpectrumAnalyzer,
    SpectrumConfig,
};
use spectraguard::error::SpectraError;
use spectraguard::ids::{
    AlertCategory, AlertSeverity, DeviceBaselineProfile, WidsEngine, WidsRuleConfig,
};
use spectraguard::sdr::RfSource;
use spectraguard::sdr::simulator::{SimulatedRfSource, SimulatorConfig, ThreatCommand};

/// Deterministic LCG + Box-Muller complex AWGN generator for repeatable DSP tests.
struct DeterministicAwgn {
    state: u64,
}

impl DeterministicAwgn {
    fn new(seed: u64) -> Self {
        Self { state: seed }
    }

    fn next_uniform(&mut self) -> f64 {
        self.state = self
            .state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        let bits = (self.state >> 11) | 1;
        (bits as f64) / ((1u64 << 53) as f64)
    }

    fn next_gaussian(&mut self) -> (f32, f32) {
        let u1 = self.next_uniform().max(1e-12);
        let u2 = self.next_uniform();
        let radius = (-2.0 * u1.ln()).sqrt();
        let theta = 2.0 * std::f64::consts::PI * u2;
        ((radius * theta.cos()) as f32, (radius * theta.sin()) as f32)
    }
}

#[tokio::test]
async fn spectrum_analyzer_detects_tone_and_estimates_awgn_noise_floor() -> Result<(), SpectraError>
{
    let sample_rate_hz = 2_048_000u32;
    let fft_size = 2048usize;
    let chunk_size = 32_768usize;
    let tone_offset_hz = 250_000.0_f64;
    let target_tone_dbfs = -40.0_f32;
    let nominal_noise_floor_dbfs = -92.0_f64;

    let (mut sim, _threat_tx) = SimulatedRfSource::new(
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

    // 4-term Blackman-Harris equivalent noise bandwidth = 2.00435 bins.
    // Solve E[|z|^2] * (ENBW / N_fft) = 10^(nominal_noise_floor_dbfs / 10).
    let enbw_bh4 = 2.004_35_f64;
    let per_axis_sigma = ((10.0_f64.powf(nominal_noise_floor_dbfs / 10.0) * (fft_size as f64))
        / (2.0 * enbw_bh4))
        .sqrt() as f32;
    let tone_amp = 10.0_f32.powf(target_tone_dbfs / 20.0);

    let mut rng = DeterministicAwgn::new(0x5EED_8683_2026_0001);
    let mut chunk = sim.next_chunk().await?;

    for (n, sample) in chunk.samples.iter_mut().enumerate() {
        let (ni, nq) = rng.next_gaussian();
        let phase =
            2.0 * std::f64::consts::PI * (tone_offset_hz / (sample_rate_hz as f64)) * (n as f64);
        sample.re = ni * per_axis_sigma + tone_amp * (phase.cos() as f32);
        sample.im = nq * per_axis_sigma + tone_amp * (phase.sin() as f32);
    }

    let frame = analyzer.analyze_chunk(&chunk)?;
    assert_eq!(frame.bins_dbfs.len(), fft_size);

    let expected_bin = (fft_size / 2) + 250; // 1024 + 250 = 1274 at 1 kHz/bin
    let (peak_bin, &peak_db) = frame
        .bins_dbfs
        .iter()
        .enumerate()
        .max_by(|(_, a), (_, b)| a.total_cmp(b))
        .ok_or_else(|| SpectraError::InvalidDspConfig("empty PSD bins".to_string()))?;

    assert!(
        (peak_bin as isize - expected_bin as isize).abs() <= 1,
        "expected peak near bin {expected_bin}, got bin {peak_bin} ({peak_db:.2} dBFS)"
    );
    assert!(
        (peak_db - target_tone_dbfs).abs() <= 1.5,
        "expected peak power near {target_tone_dbfs} dBFS, got {peak_db:.2} dBFS"
    );
    assert!(
        (frame.noise_floor_dbfs - (nominal_noise_floor_dbfs as f32)).abs() <= 1.5,
        "expected unbiased noise floor within +-1.5 dB of {nominal_noise_floor_dbfs} dBFS, got {:.2} dBFS",
        frame.noise_floor_dbfs
    );

    Ok(())
}

#[tokio::test]
async fn wids_engine_remains_silent_over_five_seconds_of_clean_ism_traffic()
-> Result<(), SpectraError> {
    let sample_rate_hz = 2_048_000u32;
    let fft_size = 2048usize;
    let chunk_size = 32_768usize; // 16 ms per chunk -> 315 chunks = 5.04 seconds

    let (mut sim, _threat_tx) = SimulatedRfSource::new(
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

    for _ in 0..315 {
        let chunk = sim.next_chunk().await?;
        let spectrum = analyzer.analyze_chunk(&chunk)?;
        let bursts = detector.process_chunk(&chunk, spectrum.noise_floor_dbfs)?;
        engine.process_frame(&spectrum, &bursts);
    }

    let shared = engine.shared_state();
    let state = shared.read().map_err(|_| SpectraError::ChannelClosed)?;
    let critical_alerts = state
        .alerts
        .iter()
        .filter(|a| a.severity == AlertSeverity::Critical)
        .count();

    assert_eq!(
        critical_alerts, 0,
        "expected 0 critical alerts during 5s clean traffic, got {:?}",
        state.alerts
    );

    Ok(())
}

#[tokio::test]
async fn wids_engine_detects_broadband_jamming_replay_and_rogue_transmitter()
-> Result<(), SpectraError> {
    let sample_rate_hz = 2_048_000u32;
    let fft_size = 2048usize;
    let chunk_size = 32_768usize;

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

    // 1. Warm up noise floor baseline (~0.96 s of clean traffic).
    let mut latest_spectrum = None;
    for _ in 0..60 {
        let chunk = sim.next_chunk().await?;
        let spec = analyzer.analyze_chunk(&chunk)?;
        let bursts = detector.process_chunk(&chunk, spec.noise_floor_dbfs)?;
        engine.process_frame(&spec, &bursts);
        latest_spectrum = Some(spec);
    }

    // 2. Inject broadband barrage jammer for 800 ms (> 250 ms minimum duration).
    let _ = threat_tx.try_send(ThreatCommand::InjectBroadbandJammer {
        duration_ms: 800,
        power_offset_db: 22.0,
    });
    for _ in 0..60 {
        let chunk = sim.next_chunk().await?;
        let spec = analyzer.analyze_chunk(&chunk)?;
        let bursts = detector.process_chunk(&chunk, spec.noise_floor_dbfs)?;
        engine.process_frame(&spec, &bursts);
        latest_spectrum = Some(spec);
    }

    // Allow broadband jammer cooldown (2.0 s = 125 chunks) to expire before subsequent checks.
    for _ in 0..140 {
        let chunk = sim.next_chunk().await?;
        let spec = analyzer.analyze_chunk(&chunk)?;
        let bursts = detector.process_chunk(&chunk, spec.noise_floor_dbfs)?;
        engine.process_frame(&spec, &bursts);
        latest_spectrum = Some(spec);
    }

    // 3. Re-inject an identical OOK envelope 55 ms after initial capture to verify >0.96 correlation.
    let base_spec = latest_spectrum
        .ok_or_else(|| SpectraError::InvalidDspConfig("missing baseline spectrum".to_string()))?;
    let ook_envelope: Vec<f32> = (0..128)
        .map(|i| if (i / 8) % 2 == 0 { 0.94 } else { 0.08 })
        .collect();
    let original_burst = DetectedBurst {
        id: 50_001,
        timestamp_ns: base_spec.timestamp_ns + 10_000_000,
        duration_us: 18_500,
        center_offset_hz: -350_000.0,
        peak_snr_db: 24.5,
        mean_rssi_dbfs: -44.0,
        modulation_hint: ModulationType::Ook,
        estimated_baud_rate: 4_800,
        envelope_signature: ook_envelope.clone(),
        bitstream_hex: "A55A19F3C20E".to_string(),
    };
    let replayed_burst = DetectedBurst {
        id: 50_002,
        timestamp_ns: original_burst.timestamp_ns + 55_000_000,
        envelope_signature: ook_envelope,
        ..original_burst.clone()
    };
    engine.process_frame(&base_spec, &[original_burst, replayed_burst]);

    // 4. Inject an unauthorized rogue transmitter on an unallocated offset & baud rate.
    let _ = threat_tx.try_send(ThreatCommand::InjectRogueTransmitter {
        freq_offset_hz: -140_000.0,
        baud_rate: 12_500,
        duration_ms: 250,
    });
    for _ in 0..35 {
        let chunk = sim.next_chunk().await?;
        let spec = analyzer.analyze_chunk(&chunk)?;
        let bursts = detector.process_chunk(&chunk, spec.noise_floor_dbfs)?;
        engine.process_frame(&spec, &bursts);
    }

    let shared = engine.shared_state();
    let state = shared.read().map_err(|_| SpectraError::ChannelClosed)?;

    let broadband = state
        .alerts
        .iter()
        .find(|a| a.category == AlertCategory::BroadbandJamming)
        .ok_or_else(|| SpectraError::InvalidDspConfig("missing BroadbandJamming alert".into()))?;
    assert_eq!(broadband.severity, AlertSeverity::Critical);
    assert!(broadband.snr_db >= 10.0);

    let replay = state
        .alerts
        .iter()
        .find(|a| a.category == AlertCategory::ReplayAttack)
        .ok_or_else(|| SpectraError::InvalidDspConfig("missing ReplayAttack alert".into()))?;
    assert_eq!(replay.severity, AlertSeverity::Critical);
    let corr = replay.evidence.correlation_score.unwrap_or(0.0);
    assert!(
        corr > 0.96,
        "expected replay correlation > 0.96, got {corr:.4}"
    );

    let rogue = state
        .alerts
        .iter()
        .find(|a| a.category == AlertCategory::RogueTransmitter)
        .ok_or_else(|| SpectraError::InvalidDspConfig("missing RogueTransmitter alert".into()))?;
    assert_eq!(rogue.severity, AlertSeverity::Warning);

    Ok(())
}