use thiserror::Error;

/// Domain errors for passive RF acquisition, SigMF I/O, and baseband DSP.
#[derive(Debug, Error)]
pub enum SpectraError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("SigMF JSON error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("invalid SigMF metadata: {0}")]
    InvalidSigMfMetadata(String),

    #[error("unsupported SigMF datatype: {0}")]
    UnsupportedSigMfDatatype(String),

    #[error("sample rate mismatch: expected {expected_hz} Hz, got {actual_hz} Hz")]
    SampleRateMismatch { expected_hz: u32, actual_hz: u32 },

    #[error("invalid center frequency {freq_hz} Hz: {reason}")]
    InvalidFrequency { freq_hz: u64, reason: String },

    #[error("RF sample stream channel closed")]
    ChannelClosed,

    #[error("end of SigMF capture stream")]
    EndOfStream,

    #[error("invalid DSP configuration: {0}")]
    InvalidDspConfig(String),
}