#[derive(Debug)]
pub enum Error {
    /// Client sent invalid data.
    InvalidData,
    /// Caller should ignore this event and continue processing.
    Ignore,
    /// Caller sent data using an inalid format under current context.
    InvalidFormat(String),
}

impl From<simd_json::Error> for Error {
    fn from(value: simd_json::Error) -> Self {
        Self::InvalidData
    }
}

impl From<rmp_serde::decode::Error> for Error {
    fn from(value: rmp_serde::decode::Error) -> Self {
        Self::InvalidData
    }
}

pub type Result<T> = std::result::Result<T, Error>;
