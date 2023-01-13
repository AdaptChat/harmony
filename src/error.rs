use std::{
    fmt::{Debug, Display},
    sync::Arc,
};

use axum::extract::ws::Message;
use essence::db::sqlx;
use lapin::{acker::Acker, options::BasicNackOptions};

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
    fn unwrap_or_nack(self, acker: Arc<Acker>, message: impl AsRef<str>) -> T;
}

impl<T, E: Debug> NackExt<T> for std::result::Result<T, E> {
    fn unwrap_or_nack(self, acker: Arc<Acker>, message: impl AsRef<str>) -> T {
        if let Err(e) = self {
            tokio::spawn(async move {
                acker
                    .nack(BasicNackOptions::default())
                    .await
                    .expect("Failed to nack");
            });

            panic!("{}: {e:?}", message.as_ref())
        } else {
            unsafe { self.unwrap_unchecked() }
        }
    }
}
