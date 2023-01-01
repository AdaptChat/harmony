use essence::db::sqlx;

#[derive(Debug)]
pub enum Error {
    /// Close connection with error message.
    Close(String),
    /// Caller should ignore this event and continue processing.
    Ignore,
}

impl From<simd_json::Error> for Error {
    fn from(val: simd_json::Error) -> Self {
        Self::Close(val.to_string())
    }
}

impl From<rmp_serde::decode::Error> for Error {
    fn from(val: rmp_serde::decode::Error) -> Self {
        Self::Close(val.to_string())
    }
}

impl From<axum::Error> for Error {
    fn from(_: axum::Error) -> Self {
        // In the context of WS, there is not really a way to return error to the client if they already disconnected
        // So we are just going to ignore it.
        Self::Ignore
    }
}

impl From<sqlx::Error> for Error {
    fn from(value: sqlx::Error) -> Self {
        Self::Close(value.to_string())
    }
}

pub type Result<T> = std::result::Result<T, Error>;

pub trait IsNoneExt<T> {
    fn ok_or_close(self, message: impl Into<String>) -> Result<T>;
}

impl<T> IsNoneExt<T> for Option<T> {
    fn ok_or_close(self, message: impl Into<String>) -> Result<T> {
        self.ok_or_else(|| Error::Close(message.into()))
    }
}
