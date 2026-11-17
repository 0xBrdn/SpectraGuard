export type ModulationType = 'Ook' | 'Fsk2' | 'ContinuousCarrier' | 'Unknown';

export type AlertSeverity = 'Info' | 'Warning' | 'Critical';

export type AlertCategory =
  | 'BroadbandJamming'
  | 'NarrowbandJamming'
  | 'ReplayAttack'
  | 'RogueTransmitter'
  | 'DutyCycleAnomaly';

export interface SpectrumFrame {
  timestamp_ns: number;
  center_freq_hz: number;
  sample_rate_hz: number;
  bins_dbfs: number[];
  max_hold_dbfs: number[];
  avg_psd_dbfs: number[];
  noise_floor_dbfs: number;
  channel_occupancy_pct: number;
}

export interface DetectedBurst {
  id: number;
  timestamp_ns: number;
  duration_us: number;
  center_offset_hz: number;
  peak_snr_db: number;
  mean_rssi_dbfs: number;
  modulation_hint: ModulationType;
  estimated_baud_rate: number;
  envelope_signature: number[];
  bitstream_hex: string;
}

export interface AlertEvidence {
  noise_floor_delta_db?: number;
  channel_occupancy_pct?: number;
  correlation_score?: number;
  inter_arrival_ms?: number;
  duration_us?: number;
  freq_offset_hz?: number;
  estimated_baud_rate?: number;
  reference_burst_id?: number;
  offending_burst_id?: number;
  reference_burst_envelope?: number[];
  offending_burst_envelope?: number[];
  bitstream_hex?: string;
  deviation_details?: string;
}

export interface WidsAlert {
  id: number;
  timestamp_utc: string;
  last_updated_utc: string;
  occurrence_count: number;
  severity: AlertSeverity;
  category: AlertCategory;
  title: string;
  center_freq_hz: number;
  snr_db: number;
  confidence_pct: number;
  evidence: AlertEvidence;
}

export interface DeviceBaselineProfile {
  name: string;
  node_id: number;
  expected_offset_hz: number;
  offset_tolerance_hz: number;
  modulation: ModulationType;
  expected_baud_rate: number;
  baud_tolerance_pct: number;
  min_duration_us: number;
  max_duration_us: number;
  expected_period_ms: number;
}

export interface SystemStatus {
  source_mode: 'sim' | 'sigmf' | string;
  center_freq_hz: number;
  sample_rate_hz: number;
  fft_size: number;
  baseline_noise_floor_dbfs: number;
  uptime_sec: number;
  active_alert_count: number;
  total_alerts_emitted: number;
  total_bursts_detected: number;
  baselines: DeviceBaselineProfile[];
}

export type TelemetryEvent =
  | { type: 'spectrum'; payload: SpectrumFrame }
  | { type: 'burst'; payload: DetectedBurst }
  | { type: 'alert'; payload: WidsAlert }
  | { type: 'status'; payload: SystemStatus };

export type ConnectionState = 'connected' | 'reconnecting' | 'disconnected';

export type InjectThreatPayload =
  | {
      threat: 'broadband_jammer';
      duration_ms: number;
      power_offset_db: number;
    }
  | {
      threat: 'narrowband_jammer';
      freq_offset_hz: number;
      duration_ms: number;
      power_db: number;
    }
  | {
      threat: 'replay_attack';
      repeat_count: number;
      interval_ms: number;
    }
  | {
      threat: 'rogue_transmitter';
      freq_offset_hz: number;
      baud_rate: number;
      duration_ms: number;
    };