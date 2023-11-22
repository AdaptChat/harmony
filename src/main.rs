#![feature(lazy_cell)]

#[macro_use]
extern crate log;

mod callbacks;
mod config;
mod error;
mod events;
mod presence;
mod socket_accept;
mod websocket;

use std::time::Duration;

use amqprs::{
    callbacks::{DefaultChannelCallback, DefaultConnectionCallback},
    connection::{Connection, OpenConnectionArguments},
};
use tokio::{net::TcpListener, runtime::Runtime};

async fn entry() {
    dotenvy::dotenv().expect("failed to load dotenv");

    essence::connect(
        &std::env::var("DB_URL").expect("missing DB_URL"),
        &std::env::var("REDIS_URL").expect("missing REDIS_URL"),
    )
    .await
    .expect("essence connect failed");

    let listener = TcpListener::bind("0.0.0.0:8076")
        .await
        .expect("failed to bind");

    let (global_shutdown, _global_rx) = tokio::sync::watch::channel(false);
    let mut shutting_down = global_shutdown.subscribe();

    tokio::spawn(async move {
        tokio::signal::ctrl_c()
            .await
            .expect("failed to await ctrl-c");
        let _ = global_shutdown.send(true);
    });

    let con = Connection::open(&OpenConnectionArguments::default())
        .await
        .expect("failed to open amqp conn");
    let _ = con.register_callback(DefaultConnectionCallback).await;
    events::setup(
        con.open_channel(None)
            .await
            .expect("failed to open amqp channel"),
    );

    loop {
        tokio::select! {
            socket = listener.accept() => match socket {
                Ok((stream, local_ip)) => {
                    match socket_accept::accept(stream).await {
                        Ok((websocket, ip, settings)) => {
                            let ip = ip.unwrap_or(local_ip.ip());
                            let channel = con.open_channel(None).await.expect("failed to open amqp channel.");
                            let _ = channel.register_callback(DefaultChannelCallback).await;

                            tokio::spawn(async move {
                                if let Err(e) = websocket::process_events(websocket, channel, ip, settings).await {
                                    error!("process_events returned with error: {e:?}");
                                }
                            });
                        },
                        Err(_) => {}
                    }
                },
                Err(err) => error!("Couldn't accept client: {err}")
            },
            _ = shutting_down.changed() => {
                break;
            }
        }
    }
}

fn main() {
    let rt = Runtime::new().unwrap();
    rt.block_on(entry());
    rt.shutdown_timeout(Duration::from_secs(5));
}
