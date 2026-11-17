use std::fmt;
use std::fs::{self, File};
use std::io::{BufReader, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::time::Duration;

use chrono::Utc;
use clap::ValueEnum;
use num_complex::Complex;
use serde::{Deserialize, Serialize};

use crate::error::SpectraError;
use crate::sdr::{IqChunk, RfSource};

/// Supported SigMF binary sample encodings normalized into `Complex<f32>` in `[-1.0, 1.0]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum SigMfDatatype {
    #[value(name = "cf32_le")]
    Cf32Le,
    #[value(name = "ci8")]
    Ci8,
    #[value(name = "cu8")]
    Cu8,
}

impl SigMfDatatype {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Cf32Le => "cf32_le",
            Self::Ci8 => "ci8",
            Self::Cu8 => "cu8",
        }
    }

    pub const fn bytes_per_sample(self) -> usize {
        match self {
            Self::Cf32Le => 8,
            Self::Ci8 | Self::Cu8 => 2,
        }
    }

    pub fn decode_samples(self, raw_bytes: &[u8]) -> Result<Vec<Complex<f32>>, SpectraError> {
        let stride = self.bytes_per_sample();
        if raw_bytes.len() % stride != 0 {
            return Err(SpectraError::InvalidSigMfMetadata(format!(
                "byte buffer length {} is not aligned to {} bytes/sample",
                raw_bytes.len(),
                stride
            )));
        }

        let sample_count = raw_bytes.len() / stride;
        let mut samples = Vec::with_capacity(sample_count);

        match self {
            Self::Cf32Le => {
                for pair in raw_bytes.chunks_exact(8) {
                    let re = f32::from_le_bytes([pair[0], pair[1], pair[2], pair[3]]);
                    let im = f32::from_le_bytes([pair[4], pair[5], pair[6], pair[7]]);
                    samples.push(Complex::new(re.clamp(-1.0, 1.0), im.clamp(-1.0, 1.0)));
                }
            }
            Self::Ci8 => {
                for pair in raw_bytes.chunks_exact(2) {
                    let re = ((pair[0] as i8 as f32) / 128.0).clamp(-1.0, 1.0);
                    let im = ((pair[1] as i8 as f32) / 128.0).clamp(-1.0, 1.0);
                    samples.push(Complex::new(re, im));
                }
            }
            Self::Cu8 => {
                for pair in raw_bytes.chunks_exact(2) {
                    let re = ((pair[0] as f32 - 127.5) / 127.5).clamp(-1.0, 1.0);
                    let im = ((pair[1] as f32 - 127.5) / 127.5).clamp(-1.0, 1.0);
                    samples.push(Complex::new(re, im));
                }
            }
        }

        Ok(samples)
    }

    pub fn encode_samples(self, samples: &[Complex<f32>], out: &mut Vec<u8>) {
        out.clear();
        out.reserve(samples.len() * self.bytes_per_sample());

        match self {
            Self::Cf32Le => {
                for sample in samples {
                    out.extend_from_slice(&sample.re.clamp(-1.0, 1.0).to_le_bytes());
                    out.extend_from_slice(&sample.im.clamp(-1.0, 1.0).to_le_bytes());
                }
            }
            Self::Ci8 => {
                for sample in samples {
                    let re = (sample.re.clamp(-1.0, 1.0) * 127.0).round() as i8 as u8;
                    let im = (sample.im.clamp(-1.0, 1.0) * 127.0).round() as i8 as u8;
                    out.push(re);
                    out.push(im);
                }
            }
            Self::Cu8 => {
                for sample in samples {
                    let re = (sample.re.clamp(-1.0, 1.0) * 127.5 + 127.5)
                        .round()
                        .clamp(0.0, 255.0) as u8;
                    let im = (sample.im.clamp(-1.0, 1.0) * 127.5 + 127.5)
                        .round()
                        .clamp(0.0, 255.0) as u8;
                    out.push(re);
                    out.push(im);
                }
            }
        }
    }
}

impl fmt::Display for SigMfDatatype {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for SigMfDatatype {
    type Err = SpectraError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim() {
            "cf32_le" => Ok(Self::Cf32Le),
            "ci8" => Ok(Self::Ci8),
            "cu8" => Ok(Self::Cu8),
            other => Err(SpectraError::UnsupportedSigMfDatatype(other.to_string())),
        }
    }
}

/// Top-level Signal Metadata Format (`.sigmf-meta`) document.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SigMfMetadata {
    pub global: SigMfGlobal,
    #[serde(default)]
    pub captures: Vec<SigMfCapture>,
    #[serde(default)]
    pub annotations: Vec<SigMfAnnotation>,
}

/// SigMF `global` metadata object specifying sample encoding and sample rate in Hz.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SigMfGlobal {
    #[serde(rename = "core:datatype")]
    pub datatype: String,
    #[serde(rename = "core:sample_rate")]
    pub sample_rate_hz: f64,
    #[serde(rename = "core:version")]
    pub version: String,
    #[serde(rename = "core:description", skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(rename = "core:hw", skip_serializing_if = "Option::is_none")]
    pub hw: Option<String>,
}

/// SigMF `captures` segment entry specifying start sample index and tuned center frequency in Hz.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SigMfCapture {
    #[serde(rename = "core:sample_start")]
    pub sample_start: u64,
    #[serde(rename = "core:frequency", skip_serializing_if = "Option::is_none")]
    pub frequency_hz: Option<f64>,
    #[serde(rename = "core:datetime", skip_serializing_if = "Option::is_none")]
    pub datetime: Option<String>,
}

/// SigMF `annotations` entry marking time-frequency regions in sample offsets and Hz.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SigMfAnnotation {
    #[serde(rename = "core:sample_start")]
    pub sample_start: u64,
    #[serde(rename = "core:sample_count")]
    pub sample_count: u64,
    #[serde(rename = "core:freq_lower_edge", skip_serializing_if = "Option::is_none")]
    pub freq_lower_edge_hz: Option<f64>,
    #[serde(rename = "core:freq_upper_edge", skip_serializing_if = "Option::is_none")]
    pub freq_upper_edge_hz: Option<f64>,
    #[serde(rename = "core:label", skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(rename = "core:comment", skip_serializing_if = "Option::is_none")]
    pub comment: Option<String>,
}

pub fn resolve_sigmf_paths(path: &Path) -> (PathBuf, PathBuf) {
    let raw = path.to_string_lossy();
    let prefix = if let Some(stripped) = raw.strip_suffix(".sigmf-meta") {
        stripped
    } else if let Some(stripped) = raw.strip_suffix(".sigmf-data") {
        stripped
    } else {
        raw.as_ref()
    };
    (
        PathBuf::from(format!("{prefix}.sigmf-meta")),
        PathBuf::from(format!("{prefix}.sigmf-data")),
    )
}

/// Streaming SigMF reader supporting continuous looping playback as a live `RfSource`.
pub struct SigMfReader {
    metadata: SigMfMetadata,
    datatype: SigMfDatatype,
    data_file: BufReader<File>,
    center_freq_hz: u64,
    sample_rate_hz: u32,
    chunk_size: usize,
    loop_playback: bool,
    realtime_pacing: bool,
    emitted_samples: u64,
    raw_buffer: Vec<u8>,
}

impl SigMfReader {
    pub fn open(
        path: impl AsRef<Path>,
        chunk_size: usize,
        loop_playback: bool,
        realtime_pacing: bool,
    ) -> Result<Self, SpectraError> {
        if chunk_size == 0 {
            return Err(SpectraError::InvalidDspConfig(
                "SigMF chunk_size must be greater than zero".to_string(),
            ));
        }

        let (meta_path, data_path) = resolve_sigmf_paths(path.as_ref());
        let meta_raw = fs::read_to_string(&meta_path)?;
        let metadata: SigMfMetadata = serde_json::from_str(&meta_raw)?;
        let datatype = metadata.global.datatype.parse::<SigMfDatatype>()?;

        if !metadata.global.sample_rate_hz.is_finite()
            || metadata.global.sample_rate_hz < 1.0
            || metadata.global.sample_rate_hz > u32::MAX as f64
        {
            return Err(SpectraError::InvalidSigMfMetadata(format!(
                "invalid core:sample_rate: {}",
                metadata.global.sample_rate_hz
            )));
        }
        let sample_rate_hz = metadata.global.sample_rate_hz.round() as u32;

        let center_freq_hz = metadata
            .captures
            .first()
            .and_then(|capture| capture.frequency_hz)
            .filter(|freq| freq.is_finite() && *freq >= 1.0 && *freq <= u64::MAX as f64)
            .map(|freq| freq.round() as u64)
            .ok_or_else(|| {
                SpectraError::InvalidSigMfMetadata(
                    "missing or invalid core:frequency in captures[0]".to_string(),
                )
            })?;

        let file = File::open(&data_path)?;
        let byte_len = file.metadata()?.len();
        let stride = datatype.bytes_per_sample() as u64;
        if byte_len == 0 || byte_len % stride != 0 {
            return Err(SpectraError::InvalidSigMfMetadata(format!(
                "data file size {} bytes is empty or unaligned to {} bytes/sample",
                byte_len, stride
            )));
        }

        let target_bytes = chunk_size * datatype.bytes_per_sample();
        Ok(Self {
            metadata,
            datatype,
            data_file: BufReader::new(file),
            center_freq_hz,
            sample_rate_hz,
            chunk_size,
            loop_playback,
            realtime_pacing,
            emitted_samples: 0,
            raw_buffer: vec![0u8; target_bytes],
        })
    }

    pub fn metadata(&self) -> &SigMfMetadata {
        &self.metadata
    }
}

impl RfSource for SigMfReader {
    async fn next_chunk(&mut self) -> Result<IqChunk, SpectraError> {
        let stride = self.datatype.bytes_per_sample();
        let target_bytes = self.chunk_size * stride;
        let mut filled = 0usize;

        while filled < target_bytes {
            let bytes_read = self.data_file.read(&mut self.raw_buffer[filled..target_bytes])?;
            if bytes_read == 0 {
                if self.loop_playback {
                    self.data_file.seek(SeekFrom::Start(0))?;
                } else {
                    break;
                }
            } else {
                filled += bytes_read;
            }
        }

        if filled == 0 {
            return Err(SpectraError::EndOfStream);
        }

        let samples = self.datatype.decode_samples(&self.raw_buffer[..filled])?;
        let timestamp_ns = ((self.emitted_samples as u128 * 1_000_000_000u128)
            / self.sample_rate_hz as u128) as u64;
        self.emitted_samples = self.emitted_samples.saturating_add(samples.len() as u64);

        if self.realtime_pacing {
            let chunk_duration_sec = samples.len() as f64 / self.sample_rate_hz as f64;
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

/// SigMF dataset writer emitting paired `.sigmf-meta` and `.sigmf-data` files.
pub struct SigMfWriter {
    meta_path: PathBuf,
    metadata: SigMfMetadata,
    datatype: SigMfDatatype,
    data_file: BufWriter<File>,
    encode_buffer: Vec<u8>,
    written_samples: u64,
}

impl SigMfWriter {
    pub fn create(
        prefix: impl AsRef<Path>,
        datatype: SigMfDatatype,
        center_freq_hz: u64,
        sample_rate_hz: u32,
        description: Option<String>,
    ) -> Result<Self, SpectraError> {
        if center_freq_hz == 0 {
            return Err(SpectraError::InvalidFrequency {
                freq_hz: center_freq_hz,
                reason: "center frequency must be greater than zero".to_string(),
            });
        }
        if sample_rate_hz == 0 {
            return Err(SpectraError::InvalidDspConfig(
                "sample_rate_hz must be greater than zero".to_string(),
            ));
        }

        let (meta_path, data_path) = resolve_sigmf_paths(prefix.as_ref());
        if let Some(parent) = meta_path.parent() {
            if !parent.as_os_str().is_empty() {
                fs::create_dir_all(parent)?;
            }
        }

        let metadata = SigMfMetadata {
            global: SigMfGlobal {
                datatype: datatype.as_str().to_string(),
                sample_rate_hz: sample_rate_hz as f64,
                version: "1.2.0".to_string(),
                description,
                hw: Some("SpectraGuard Passive Baseband Simulator".to_string()),
            },
            captures: vec![SigMfCapture {
                sample_start: 0,
                frequency_hz: Some(center_freq_hz as f64),
                datetime: Some(Utc::now().to_rfc3339()),
            }],
            annotations: Vec::new(),
        };

        let meta_json = serde_json::to_string_pretty(&metadata)?;
        fs::write(&meta_path, meta_json)?;
        let data_file = BufWriter::new(File::create(&data_path)?);

        Ok(Self {
            meta_path,
            metadata,
            datatype,
            data_file,
            encode_buffer: Vec::new(),
            written_samples: 0,
        })
    }

    pub fn write_samples(&mut self, samples: &[Complex<f32>]) -> Result<(), SpectraError> {
        self.datatype.encode_samples(samples, &mut self.encode_buffer);
        self.data_file.write_all(&self.encode_buffer)?;
        self.written_samples = self.written_samples.saturating_add(samples.len() as u64);
        Ok(())
    }

    pub fn add_annotation(&mut self, annotation: SigMfAnnotation) {
        self.metadata.annotations.push(annotation);
    }

    pub fn written_samples(&self) -> u64 {
        self.written_samples
    }

    pub fn finalize(mut self) -> Result<(), SpectraError> {
        self.data_file.flush()?;
        let meta_json = serde_json::to_string_pretty(&self.metadata)?;
        fs::write(&self.meta_path, meta_json)?;
        Ok(())
    }
}