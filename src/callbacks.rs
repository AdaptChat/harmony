use amqprs::{callbacks::ChannelCallback, channel::Channel, Cancel, CloseChannel};
use uuid::Uuid;

use crate::shutdown_notifier::SHUTDOWN_NOTIFIER;

type Result<T> = std::result::Result<T, amqprs::error::Error>;
pub struct ChannelCallbacks {
    session_id: Uuid,
    session_id_str: String,
}

#[async_trait::async_trait]
impl ChannelCallback for ChannelCallbacks {
    fn cancel(&mut self, channel: &Channel, cancel: Cancel) -> Result<()> {
        info!(
            "channel {}'s consumer id: {} is canceled.",
            self.session_id_str,
            cancel.consumer_tag()
        );

        Ok(())
    }

    fn close(&mut self, channel: &Channel, _: CloseChannel) -> Result<()> {
        info!(
            "Channel {} is closing, shutting down client.",
            self.session_id_str
        );
        SHUTDOWN_NOTIFIER.shutdown(&self.session_id);

        Ok(())
    }
}
