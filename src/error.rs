use std::fmt::Display;

#[derive(Debug, Default)]
pub struct Error {
    inner: Option<Box<dyn std::fmt::Debug + Send + Sync>>,
    ctx: Option<String>,
}

impl Error {
    pub fn ctx<C: Display>(self, ctx: C) -> Self {
        Self {
            ctx: Some(ctx.to_string()),
            ..self
        }
    }
}

impl From<&str> for Error {
    fn from(value: &str) -> Self {
        Self {
            inner: None,
            ctx: Some(value.to_string()),
        }
    }
}

macro_rules! impl_errors {
    ($($T:ty),*) => {
        $(
            impl From<$T> for Error {
                fn from(value: $T) -> Self {
                    Self {
                        inner: Some(Box::new(value)),
                        ctx: None,
                    }
                }
            }
        )*
    };
}

#[macro_export]
macro_rules! to_err {
    ($e:expr) => {
        $crate::error::Error::from($e)
    };
}

#[macro_export]
macro_rules! err_with_ctx {
    ($e:expr, $ctx:literal) => {{
        $crate::to_err!($e).ctx($ctx)
    }};
}

#[macro_export]
macro_rules! err_with_ctx_wrapped {
    ($e:expr, $ctx:literal) => {{
        std::result::Result::Err($crate::to_err!($e).ctx($ctx))
    }};
}

#[macro_export]
macro_rules! bail {
    ($e:expr) => {
        return std::result::Result::Err($crate::to_err!($e))
    };
}

#[macro_export]
macro_rules! bail_with_ctx {
    ($e:expr, $ctx:literal) => {
        return $crate::err_with_ctx_wrapped!($e, $ctx)
    };
}

impl_errors! {
    essence::Error,
    essence::db::sqlx::Error,
    rmp_serde::decode::Error,
    simd_json::Error,
    deadpool_redis::PoolError,
    deadpool_redis::redis::RedisError,
    bincode::error::EncodeError,
    bincode::error::DecodeError,
    amqprs::error::Error,
    tokio_tungstenite::tungstenite::Error
}

impl Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let ctx = match self.ctx {
            Some(ref ctx) => ctx,
            None => "Unknown",
        };

        match self.inner {
            Some(ref inner) => write!(f, "{:?} caused by: {}", inner, ctx)?,
            None => f.write_str(ctx)?,
        }

        Ok(())
    }
}

pub type Result<T> = std::result::Result<T, Error>;
