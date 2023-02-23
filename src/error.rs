use std::fmt::{Debug, Display};

use essence::db::sqlx;
use lapin::{acker::Acker, options::BasicNackOptions};

#[derive(Debug, Clone)]
pub enum Error {
    /// Close connection with error message.
    Close(String),
    /// Caller should ignore this event and continue processing.
    Ignore,
}

macro_rules! impl_errors {
    ($($err_ty:ty),*) => {
        $(
            impl From<$err_ty> for Error {
                fn from(value: $err_ty) -> Self {
                    Self::Close(format!("{}: {value:?}", stringify!($err_ty)))
                }
            }
        )*
    }
}

impl_errors!(
    simd_json::Error,
    rmp_serde::decode::Error,
    sqlx::Error,
    essence::Error,
    flume::SendError<tokio_tungstenite::tungstenite::Message>,
    tokio::task::JoinError,
    lapin::Error,
    deadpool_redis::PoolError,
    redis::RedisError,
    bincode::error::DecodeError,
    bincode::error::EncodeError,
    tokio_tungstenite::tungstenite::Error
);

impl Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Close(e) => f.write_str(e),
            Self::Ignore => f.write_str("Error::Ignore"),
        }
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

pub trait NackExt<T> {
    async fn unwrap_or_nack(self, acker: &Acker, message: impl AsRef<str>) -> T;
}

impl<T, E: Debug> NackExt<T> for std::result::Result<T, E> {
    async fn unwrap_or_nack(self, acker: &Acker, message: impl AsRef<str>) -> T {
        match self {
            Ok(r) => r,
            Err(e) => {
                acker
                    .nack(BasicNackOptions::default())
                    .await
                    .expect("Failed to nack");

                panic!("{}: {e:?}", message.as_ref())
            }
        }
    }
}
