#![allow(incomplete_features)]
#![feature(async_closure)]
#![feature(async_fn_in_trait)]
#![feature(once_cell)]

#[macro_use]
extern crate log;

mod client;
mod config;
mod error;
mod events;
mod presence;
mod recv;
mod socket;
mod upstream;
mod websocket;

use deadpool_lapin::Runtime;
use lapin::{options::ExchangeDeclareOptions, types::FieldTable, ExchangeKind};
use socket::accept;
use std::{env, sync::Arc};
use tokio::net::TcpListener;
use websocket::handle_socket;

#[tokio::main]
async fn main() {
    drop(dotenv::dotenv());

    if std::env::var("RUST_LOG").is_err() {
        std::env::set_var("RUST_LOG", "sqlx::query=warn,rustls=warn,info");
    }

    pretty_env_logger::init();

    essence::connect(
        &env::var("DATABASE_URL").expect("Missing DATABASE_URL env var"),
        &env::var("REDIS_URL").expect("Missing REDIS_URL env var"),
    )
    .await
    .expect("could not connect");

    let pool = Arc::new(
        deadpool_lapin::Config {
            url: Some("amqp://127.0.0.1:5672".to_string()),
            ..Default::default()
        }
        .create_pool(Some(Runtime::Tokio1))
        .expect("Failed to create pool"),
    );

    let con = pool.get().await.expect("Failed to acquire connection");

    con.create_channel()
        .await
        .expect("Failed to create channel")
        .exchange_declare(
            "events",
            ExchangeKind::Topic,
            ExchangeDeclareOptions::default(),
            FieldTable::default(),
        )
        .await
        .expect("Failed to create global event exchange");

    events::setup(con);

    let listener = TcpListener::bind("0.0.0.0:8076")
        .await
        .expect("Failed to bind");

    let (tx, mut rx) = tokio::sync::oneshot::channel();

    tokio::spawn(async move {
        tokio::signal::ctrl_c()
            .await
            .expect("Failed to listen to ctrl-c");

        tx.send(())
    });

    tokio::select! {
        _ = async {
            loop {
                let (stream, addr) = match listener.accept().await {
                    Ok(r) => r,
                    Err(e) => {
                        error!("Error while accepting connection: {e:?}");
                        continue;
                    }
                };
                info!("Stream accepted");

                let pool = pool.clone();

                tokio::spawn(async move {
                    match accept(stream).await {
                        Ok((stream, con, ip)) => {
                            let ip = ip.unwrap_or_else(|| addr.ip());
                            debug!("Accepted connection from: {ip}");

                            // We don't want to hold onto pool for too long.
                            let amqp_con = {
                                pool.get().await.expect("Failed to acquire db connection.")
                            };

                            if let Err(e) = handle_socket(stream, con, ip, amqp_con).await {
                                error!("Error while handling socket: {e:?}");
                            }
                        }
                        Err(e) => {
                            error!("Error while accepting stream: {e:?}");
                        }
                    }
                });
            }
        } => {}
        _ = &mut rx => {
            info!("Received ctrl-c, exiting.");

            return
        }
    }
}
