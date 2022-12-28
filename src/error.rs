#[derive(Debug)]
pub enum Error {
    /// Client sent invalid data.
    InvalidData(String),
    /// Caller should ignore this event and continue processing.
    Ignore,
}

impl From<simd_json::Error> for Error {
    fn from(val: simd_json::Error) -> Self {
        Self::InvalidData(val.to_string())
    }
}

impl From<rmp_serde::decode::Error> for Error {
    fn from(val: rmp_serde::decode::Error) -> Self {
        Self::InvalidData(val.to_string())
    }
}

impl From<axum::Error> for Error {
    fn from(_: axum::Error) -> Self {
        // In the context of WS, there is not really a way to return error to the client if they already disconnected
        // So we are just going to ignore it.
        Self::Ignore
    }
}

pub type Result<T> = std::result::Result<T, Error>;
