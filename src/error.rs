use std::fmt::{Debug, Display};

use axum::extract::ws::Message;
use deadpool_redis::PoolError;
use essence::db::sqlx;
use lapin::{acker::Acker, options::BasicNackOptions};
use redis::RedisError;

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

impl From<essence::Error> for Error {
    fn from(value: essence::Error) -> Self {
        Self::Close(format!("{value:?}"))
    }
}

impl From<flume::SendError<Message>> for Error {
    fn from(_: flume::SendError<Message>) -> Self {
        Self::Close("Internal error while message to mpsc".to_string())
    }
}

impl From<tokio::task::JoinError> for Error {
    fn from(value: tokio::task::JoinError) -> Self {
        Self::Close(value.to_string())
    }
}

impl From<lapin::Error> for Error {
    fn from(value: lapin::Error) -> Self {
        Self::Close(value.to_string())
    }
}

impl From<PoolError> for Error {
    fn from(value: PoolError) -> Self {
        Self::Close(value.to_string())
    }
}

impl From<RedisError> for Error {
    fn from(value: RedisError) -> Self {
        Self::Close(value.to_string())
    }
}

impl From<bincode::error::DecodeError> for Error {
    fn from(value: bincode::error::DecodeError) -> Self {
        Self::Close(value.to_string())
    }
}

impl From<bincode::error::EncodeError> for Error {
    fn from(value: bincode::error::EncodeError) -> Self {
        Self::Close(value.to_string())
    }
}

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
        if let Err(e) = self {
            acker
                .nack(BasicNackOptions::default())
                .await
                .expect("Failed to nack");

            panic!("{}: {e:?}", message.as_ref())
        } else {
            unsafe { self.unwrap_unchecked() }
        }
    }
}
