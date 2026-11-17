use std::sync::{Arc, RwLock};
use std::time::Instant;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::State;
use axum::http::{StatusCode, Uri, header};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use rust_embed::RustEmbed;
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::sync::{broadcast, mpsc};
use tower_http::cors::CorsLayer;
use tracing::warn;

use crate::dsp::{DetectedBurst, SpectrumFrame};
use crate::error::SpectraError;
use crate::ids::{DeviceBaselineProfile, SharedWidsState, WidsAlert};
use crate::sdr::BandPreset;
use crate::sdr::simulator::ThreatCommand;

#[derive(RustEmbed)]
#[folder = "ui/dist"]
#[allow_missing = true]
struct UiAssets;

const FALLBACK_INDEX_HTML: &str = r#"<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8" />
  <meta name="viewport" content="width=device-width, initial-scale=1" />
  <title>SpectraGuard WIDS Telemetry</title>
  <style>
    body {
      margin: 0;
      padding: 2rem;
      background: #0a0e14;
      color: #d6deeb;
      font-family: ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, monospace;
    }
    .panel {
      max-width: 760px;
      margin: 0 auto;
      padding: 1.5rem;
      border: 1px solid #1e293b;
      border-radius: 6px;
      background: #111622;
    }
    h1 { margin-top: 0; font-size: 1.25rem; color: #38bdf8; }
    code { color: #34d399; }
    ul { line-height: 1.8; }
  </style>
</head>
<body>
  <div class="panel">
    <h1>SpectraGuard Passive RF WIDS Engine Online</h1>
    <p>Frontend bundle (<code>ui/dist</code>) has not been compiled yet. Backend telemetry and control endpoints are active:</p>
    <ul>
      <li>WebSocket Telemetry: <code>GET /ws/telemetry</code></li>
      <li>System Status: <code>GET /api/v1/status</code></li>
      <li>Band Configuration: <code>POST /api/v1/config/band</code></li>
      <li>Simulator Threat Injection: <code>POST /api/v1/sim/inject</code></li>
      <li>WIDS Alerts: <code>GET /api/v1/alerts</code> &amp; <code>POST /api/v1/alerts/clear</code></li>
      <li>Device Baselines: <code>GET /api/v1/baselines</code> &amp; <code>POST /api/v1/baselines</code></li>
    </ul>
  </div>
</body>
</html>"#;

/// Tagged JSON telemetry message streamed over `GET /ws/telemetry`.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", content = "payload", rename_all = "snake_case")]
pub enum TelemetryEvent {
    Spectrum(SpectrumFrame),
    Burst(DetectedBurst),
    Alert(WidsAlert),
    Status(SystemStatus),
}

/// Control commands dispatched from REST endpoints to the DSP/WIDS worker task.
#[derive(Debug, Clone)]
pub enum PipelineCommand {
    SetCenterFreq(u64),
    AddBaseline(DeviceBaselineProfile),
    ResetMaxHold,
}

/// Mutable runtime configuration shared between the DSP task and REST handlers.
#[derive(Debug, Clone)]
pub struct RuntimeMetadata {
    pub source_mode: String,
    pub center_freq_hz: u64,
    pub sample_rate_hz: u32,
    pub fft_size: usize,
    pub started_at: Instant,
    pub baselines: Vec<DeviceBaselineProfile>,
}

/// Shared application state injected into Axum HTTP and WebSocket handlers.
#[derive(Clone)]
pub struct ApiState {
    pub wids_state: SharedWidsState,
    pub runtime: Arc<RwLock<RuntimeMetadata>>,
    pub telemetry_tx: broadcast::Sender<TelemetryEvent>,
    pub pipeline_cmd_tx: mpsc::Sender<PipelineCommand>,
    pub threat_tx: Option<mpsc::Sender<ThreatCommand>>,
}

impl ApiState {
    pub fn snapshot_status(&self) -> Result<SystemStatus, SpectraError> {
        let rt = self
            .runtime
            .read()
            .map_err(|_| SpectraError::ChannelClosed)?;
        let wids = self
            .wids_state
            .read()
            .map_err(|_| SpectraError::ChannelClosed)?;

        Ok(SystemStatus {
            source_mode: rt.source_mode.clone(),
            center_freq_hz: rt.center_freq_hz,
            sample_rate_hz: rt.sample_rate_hz,
            fft_size: rt.fft_size,
            baseline_noise_floor_dbfs: wids.baseline_noise_floor_dbfs,
            uptime_sec: rt.started_at.elapsed().as_secs(),
            active_alert_count: wids.alerts.len(),
            total_alerts_emitted: wids.total_alerts_emitted,
            total_bursts_detected: wids.total_bursts_detected,
            baselines: rt.baselines.clone(),
        })
    }
}

/// System telemetry and configuration summary returned by `GET /api/v1/status`.
#[derive(Debug, Clone, Serialize)]
pub struct SystemStatus {
    pub source_mode: String,
    pub center_freq_hz: u64,
    pub sample_rate_hz: u32,
    pub fft_size: usize,
    pub baseline_noise_floor_dbfs: f32,
    pub uptime_sec: u64,
    pub active_alert_count: usize,
    pub total_alerts_emitted: u64,
    pub total_bursts_detected: u64,
    pub baselines: Vec<DeviceBaselineProfile>,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub enum BandInput {
    Numeric(u64),
    Text(String),
}

#[derive(Debug, Deserialize)]
pub struct SetBandRequest {
    #[serde(default)]
    pub band: Option<BandInput>,
    #[serde(default)]
    pub center_freq_hz: Option<u64>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "threat", rename_all = "snake_case")]
pub enum InjectThreatRequest {
    BroadbandJammer {
        #[serde(default = "default_jam_duration_ms")]
        duration_ms: u64,
        #[serde(default = "default_broadband_offset_db")]
        power_offset_db: f32,
    },
    NarrowbandJammer {
        #[serde(default = "default_nb_offset_hz")]
        freq_offset_hz: f32,
        #[serde(default = "default_jam_duration_ms")]
        duration_ms: u64,
        #[serde(default = "default_nb_power_db")]
        power_db: f32,
    },
    ReplayAttack {
        #[serde(default = "default_replay_count")]
        repeat_count: u8,
        #[serde(default = "default_replay_interval_ms")]
        interval_ms: u64,
    },
    RogueTransmitter {
        #[serde(default = "default_rogue_offset_hz")]
        freq_offset_hz: f32,
        #[serde(default = "default_rogue_baud_rate")]
        baud_rate: u32,
        #[serde(default = "default_rogue_duration_ms")]
        duration_ms: u64,
    },
}

const fn default_jam_duration_ms() -> u64 {
    2_000
}

const fn default_broadband_offset_db() -> f32 {
    24.0
}

const fn default_nb_offset_hz() -> f32 {
    120_000.0
}

const fn default_nb_power_db() -> f32 {
    -14.0
}

const fn default_replay_count() -> u8 {
    4
}

const fn default_replay_interval_ms() -> u64 {
    85
}

const fn default_rogue_offset_hz() -> f32 {
    -140_000.0
}

const fn default_rogue_baud_rate() -> u32 {
    12_500
}

const fn default_rogue_duration_ms() -> u64 {
    1_500
}

/// Downsamples PSD vectors to `target_bins` using peak-preserving bucket maxima.
pub fn decimate_spectrum_frame(frame: &SpectrumFrame, target_bins: usize) -> SpectrumFrame {
    let src_len = frame.bins_dbfs.len();
    if target_bins == 0 || src_len <= target_bins {
        return frame.clone();
    }

    let decimate = |src: &[f32]| -> Vec<f32> {
        let n = src.len();
        if n <= target_bins {
            return src.to_vec();
        }
        let mut out = Vec::with_capacity(target_bins);
        for i in 0..target_bins {
            let start = (i * n) / target_bins;
            let end = (((i + 1) * n) / target_bins).max(start + 1).min(n);
            let peak = src[start..end]
                .iter()
                .copied()
                .fold(f32::NEG_INFINITY, f32::max);
            out.push(peak);
        }
        out
    };

    SpectrumFrame {
        timestamp_ns: frame.timestamp_ns,
        center_freq_hz: frame.center_freq_hz,
        sample_rate_hz: frame.sample_rate_hz,
        bins_dbfs: decimate(&frame.bins_dbfs),
        max_hold_dbfs: decimate(&frame.max_hold_dbfs),
        avg_psd_dbfs: decimate(&frame.avg_psd_dbfs),
        noise_floor_dbfs: frame.noise_floor_dbfs,
        channel_occupancy_pct: frame.channel_occupancy_pct,
    }
}

pub fn build_router(state: ApiState) -> Router {
    Router::new()
        .route("/ws/telemetry", get(ws_telemetry_handler))
        .route("/api/v1/status", get(get_status))
        .route("/api/v1/config/band", post(post_config_band))
        .route("/api/v1/spectrum/reset-hold", post(post_spectrum_reset_hold))
        .route("/api/v1/sim/inject", post(post_sim_inject))
        .route("/api/v1/alerts", get(get_alerts))
        .route("/api/v1/alerts/clear", post(post_alerts_clear))
        .route("/api/v1/baselines", get(get_baselines).post(post_baselines))
        .fallback(static_asset_fallback)
        .layer(CorsLayer::permissive())
        .with_state(state)
}

async fn post_spectrum_reset_hold(State(state): State<ApiState>) -> Response {
    if state
        .pipeline_cmd_tx
        .send(PipelineCommand::ResetMaxHold)
        .await
        .is_err()
    {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({ "error": "DSP pipeline command channel closed" })),
        )
            .into_response();
    }

    (StatusCode::OK, Json(json!({ "status": "reset" }))).into_response()
}

async fn ws_telemetry_handler(
    ws: WebSocketUpgrade,
    State(state): State<ApiState>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_telemetry_socket(socket, state))
}

async fn handle_telemetry_socket(mut socket: WebSocket, state: ApiState) {
    let mut rx = state.telemetry_tx.subscribe();

    if let Ok(status) = state.snapshot_status() {
        if let Ok(payload) = serde_json::to_string(&TelemetryEvent::Status(status)) {
            if socket.send(Message::Text(payload.into())).await.is_err() {
                return;
            }
        }
    }

    loop {
        tokio::select! {
            incoming = socket.recv() => {
                match incoming {
                    Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break,
                    Some(Ok(Message::Ping(data))) => {
                        if socket.send(Message::Pong(data)).await.is_err() {
                            break;
                        }
                    }
                    Some(Ok(_)) => {}
                }
            }
            event = rx.recv() => {
                match event {
                    Ok(msg) => {
                        if let Ok(json_text) = serde_json::to_string(&msg) {
                            if socket.send(Message::Text(json_text.into())).await.is_err() {
                                break;
                            }
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(skipped)) => {
                        warn!(skipped_messages = skipped, "WebSocket telemetry subscriber lagged");
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
        }
    }
}

async fn get_status(State(state): State<ApiState>) -> Response {
    match state.snapshot_status() {
        Ok(status) => (StatusCode::OK, Json(status)).into_response(),
        Err(err) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": err.to_string() })),
        )
            .into_response(),
    }
}

fn parse_target_freq_hz(req: &SetBandRequest) -> Result<u64, SpectraError> {
    if let Some(freq_hz) = req.center_freq_hz {
        return BandPreset::from_cli_value(freq_hz).map(BandPreset::center_freq_hz);
    }

    let Some(band) = &req.band else {
        return Err(SpectraError::InvalidFrequency {
            freq_hz: 0,
            reason: "provide either center_freq_hz or band".to_string(),
        });
    };

    match band {
        BandInput::Numeric(val) => BandPreset::from_cli_value(*val).map(BandPreset::center_freq_hz),
        BandInput::Text(raw) => {
            let norm = raw.trim().to_ascii_lowercase().replace(' ', "");
            match norm.as_str() {
                "433" | "433.92" | "433.92mhz" | "eu433" | "ism433" => Ok(433_920_000),
                "868" | "868.3" | "868.30" | "868.30mhz" | "eu868" | "srd868" => Ok(868_300_000),
                other => {
                    if let Some(mhz_str) = other.strip_suffix("mhz") {
                        let mhz: f64 =
                            mhz_str
                                .parse()
                                .map_err(|_| SpectraError::InvalidFrequency {
                                    freq_hz: 0,
                                    reason: format!("invalid band specification: {raw}"),
                                })?;
                        if !mhz.is_finite() || mhz < 1.0 {
                            return Err(SpectraError::InvalidFrequency {
                                freq_hz: 0,
                                reason: format!("invalid MHz frequency: {raw}"),
                            });
                        }
                        Ok((mhz * 1_000_000.0).round() as u64)
                    } else {
                        let hz: u64 = other.parse().map_err(|_| SpectraError::InvalidFrequency {
                            freq_hz: 0,
                            reason: format!("invalid band specification: {raw}"),
                        })?;
                        BandPreset::from_cli_value(hz).map(BandPreset::center_freq_hz)
                    }
                }
            }
        }
    }
}

async fn post_config_band(
    State(state): State<ApiState>,
    Json(payload): Json<SetBandRequest>,
) -> Response {
    let target_freq_hz = match parse_target_freq_hz(&payload) {
        Ok(freq) => freq,
        Err(err) => {
            return (StatusCode::BAD_REQUEST, Json(json!({ "error": err.to_string() })))
                .into_response();
        }
    };

    if state
        .pipeline_cmd_tx
        .send(PipelineCommand::SetCenterFreq(target_freq_hz))
        .await
        .is_err()
    {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({ "error": "DSP pipeline command channel closed" })),
        )
            .into_response();
    }

    if let Ok(mut rt) = state.runtime.write() {
        rt.center_freq_hz = target_freq_hz;
    }

    match state.snapshot_status() {
        Ok(status) => {
            let _ = state
                .telemetry_tx
                .send(TelemetryEvent::Status(status.clone()));
            (StatusCode::OK, Json(status)).into_response()
        }
        Err(err) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": err.to_string() })),
        )
            .into_response(),
    }
}

async fn post_sim_inject(
    State(state): State<ApiState>,
    Json(payload): Json<InjectThreatRequest>,
) -> Response {
    let Some(threat_tx) = &state.threat_tx else {
        return (
            StatusCode::CONFLICT,
            Json(json!({ "error": "threat injection is only supported in simulator mode" })),
        )
            .into_response();
    };

    let (threat_label, cmd) = match payload {
        InjectThreatRequest::BroadbandJammer {
            duration_ms,
            power_offset_db,
        } => (
            "broadband_jammer",
            ThreatCommand::InjectBroadbandJammer {
                duration_ms,
                power_offset_db,
            },
        ),
        InjectThreatRequest::NarrowbandJammer {
            freq_offset_hz,
            duration_ms,
            power_db,
        } => (
            "narrowband_jammer",
            ThreatCommand::InjectNarrowbandJammer {
                freq_offset_hz,
                duration_ms,
                power_db,
            },
        ),
        InjectThreatRequest::ReplayAttack {
            repeat_count,
            interval_ms,
        } => (
            "replay_attack",
            ThreatCommand::InjectReplayAttack {
                repeat_count,
                interval_ms,
            },
        ),
        InjectThreatRequest::RogueTransmitter {
            freq_offset_hz,
            baud_rate,
            duration_ms,
        } => (
            "rogue_transmitter",
            ThreatCommand::InjectRogueTransmitter {
                freq_offset_hz,
                baud_rate,
                duration_ms,
            },
        ),
    };

    if threat_tx.send(cmd).await.is_err() {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({ "error": "simulator threat channel closed" })),
        )
            .into_response();
    }

    (
        StatusCode::OK,
        Json(json!({ "status": "injected", "threat": threat_label })),
    )
        .into_response()
}

async fn get_alerts(State(state): State<ApiState>) -> Response {
    match state.wids_state.read() {
        Ok(guard) => {
            let alerts: Vec<WidsAlert> = guard.alerts.iter().rev().cloned().collect();
            (StatusCode::OK, Json(alerts)).into_response()
        }
        Err(_) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": "WIDS state lock poisoned" })),
        )
            .into_response(),
    }
}

async fn post_alerts_clear(State(state): State<ApiState>) -> Response {
    let cleared_count = match state.wids_state.write() {
        Ok(mut guard) => {
            let count = guard.alerts.len();
            guard.alerts.clear();
            count
        }
        Err(_) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": "WIDS state lock poisoned" })),
            )
                .into_response();
        }
    };

    if let Ok(status) = state.snapshot_status() {
        let _ = state.telemetry_tx.send(TelemetryEvent::Status(status));
    }

    (
        StatusCode::OK,
        Json(json!({ "status": "cleared", "cleared_count": cleared_count })),
    )
        .into_response()
}

async fn get_baselines(State(state): State<ApiState>) -> Response {
    match state.runtime.read() {
        Ok(rt) => (StatusCode::OK, Json(rt.baselines.clone())).into_response(),
        Err(_) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": "runtime metadata lock poisoned" })),
        )
            .into_response(),
    }
}

async fn post_baselines(
    State(state): State<ApiState>,
    Json(profile): Json<DeviceBaselineProfile>,
) -> Response {
    if profile.name.trim().is_empty()
        || profile.offset_tolerance_hz <= 0.0
        || profile.max_duration_us < profile.min_duration_us
    {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "invalid device baseline profile parameters" })),
        )
            .into_response();
    }

    if state
        .pipeline_cmd_tx
        .send(PipelineCommand::AddBaseline(profile.clone()))
        .await
        .is_err()
    {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({ "error": "DSP pipeline command channel closed" })),
        )
            .into_response();
    }

    if let Ok(mut rt) = state.runtime.write() {
        rt.baselines.push(profile.clone());
    }

    if let Ok(status) = state.snapshot_status() {
        let _ = state.telemetry_tx.send(TelemetryEvent::Status(status));
    }

    (StatusCode::CREATED, Json(profile)).into_response()
}

async fn static_asset_fallback(uri: Uri) -> Response {
    let raw_path = uri.path().trim_start_matches('/');
    let lookup_path = if raw_path.is_empty() {
        "index.html"
    } else {
        raw_path
    };

    if let Some(asset) = UiAssets::get(lookup_path) {
        let mime = mime_guess::from_path(lookup_path).first_or_octet_stream();
        return (
            StatusCode::OK,
            [(header::CONTENT_TYPE, mime.as_ref())],
            asset.data,
        )
            .into_response();
    }

    if !lookup_path.contains('.') {
        if let Some(index) = UiAssets::get("index.html") {
            return (
                StatusCode::OK,
                [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
                index.data,
            )
                .into_response();
        }
    }

    Html(FALLBACK_INDEX_HTML).into_response()
}