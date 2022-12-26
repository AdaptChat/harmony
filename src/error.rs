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
    fn from(_: simd_json::Error) -> Self {
        Self::InvalidData
    }
}

impl From<rmp_serde::decode::Error> for Error {
    fn from(_: rmp_serde::decode::Error) -> Self {
        Self::InvalidData
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
