use std::fmt::Display;

use anyhow::Context;
use futures_util::{stream::SplitSink, SinkExt};
use tokio_tungstenite::tungstenite::{
    protocol::{frame::coding::CloseCode, CloseFrame},
    Message,
};

use crate::socket_accept::WebSocketStream;

pub trait ResultExt<T> {
    async fn close_with_context_if_err<C: Display + Send + Sync + 'static>(
        self,
        context: C,
        tx: &mut SplitSink<WebSocketStream, Message>,
    ) -> Result<T, anyhow::Error>;
}

impl<T, E: std::error::Error + Send + Sync + 'static> ResultExt<T> for std::result::Result<T, E> {
    async fn close_with_context_if_err<C: Display + Send + Sync + 'static>(
        self,
        context: C,
        tx: &mut SplitSink<WebSocketStream, Message>,
    ) -> Result<T, anyhow::Error> {
        let r = self.context(context);
        if let Err(ref e) = r {
            tx.send(Message::Close(Some(CloseFrame {
                code: CloseCode::Error,
                reason: format!("{e:?}").into(),
            })))
            .await;
        }

        r
    }
}
