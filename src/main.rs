#![feature(lazy_cell)]

#[macro_use]
extern crate log;

mod callbacks;
mod client_event;
mod config;
mod error;
mod presence;
mod socket_accept;
mod websocket;

use tokio::net::TcpListener;

#[tokio::main]
async fn main() {
    dotenvy::dotenv().expect("failed to load dotenv");

    essence::connect(
        &std::env::var("DB_URL").expect("missing DB_URL"),
        &std::env::var("REDIS_URL").expect("missing REDIS_URL"),
    );

    let listener = TcpListener::bind("0.0.0.0:8076")
        .await
        .expect("failed to bind");

    let (global_shutdown, global_rx) = tokio::sync::watch::channel(false);
    let mut shutting_down = global_shutdown.subscribe();

    tokio::spawn(async move {
        tokio::signal::ctrl_c()
            .await
            .expect("failed to await ctrl-c");
        let _ = global_shutdown.send(true);
    });

    loop {
        tokio::select! {
            socket = listener.accept() => match socket {
                Ok((stream, local_ip)) => {
                    match socket_accept::accept(stream).await {
                        Ok((websocket, ip, settings)) => {
                            let ip = ip.unwrap_or(local_ip.ip());
                        },
                        Err(_) => {}
                    }
                },
                Err(err) => error!("Couldn't accept client: {err}")
            },
            _ = shutting_down.changed() => {
                tokio::task::yield_now().await;

                break;
            }
        }
    }
}
