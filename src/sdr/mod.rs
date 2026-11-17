pub mod sigmf;
pub mod sim;

use num_complex::Complex;

use crate::error::SpectraError;

/// Normalized complex baseband IQ sample block (`[-1.0, 1.0]`) with capture metadata.
#[derive(Debug, Clone)]
pub struct IqChunk {
    pub samples: Vec<Complex<f32>>,
    pub center_freq_hz: u64,
    pub sample_rate_hz: u32,
    pub timestamp_ns: u64,
}

/// European ISM/SRD center frequency presets in Hz.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BandPreset {
    EuIsm433,
    EuSrd868,
    Custom(u64),
}

impl BandPreset {
    pub const fn center_freq_hz(self) -> u64 {
        match self {
            Self::EuIsm433 => 433_920_000,
            Self::EuSrd868 => 868_300_000,
            Self::Custom(freq_hz) => freq_hz,
        }
    }

    pub fn from_cli_value(value: u64) -> Result<Self, SpectraError> {
        match value {
            433 => Ok(Self::EuIsm433),
            868 => Ok(Self::EuSrd868),
            freq_hz if freq_hz >= 1_000_000 => Ok(Self::Custom(freq_hz)),
            freq_hz => Err(SpectraError::InvalidFrequency {
                freq_hz,
                reason: "specify 433, 868, or a frequency >= 1_000_000 Hz".to_string(),
            }),
        }
    }
}

/// Passive RF receiver interface yielding normalized baseband IQ chunks.
pub trait RfSource {
    async fn next_chunk(&mut self) -> Result<IqChunk, SpectraError>;
    fn center_freq_hz(&self) -> u64;
    fn sample_rate_hz(&self) -> u32;
    fn set_center_freq(&mut self, freq_hz: u64) -> Result<(), SpectraError>;
}