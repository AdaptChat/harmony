#[macro_use]
extern crate log;

mod config;
mod socket;

use tokio::net::TcpListener;

#[tokio::main]
async fn main() {
    let listener = TcpListener::bind("0.0.0.0:8076")
        .await
        .expect("failed to bind");

    let (shutdown, rx) = tokio::sync::broadcast::channel(1);
    let mut shutting_down = shutdown.subscribe();

    tokio::spawn(async move {
        tokio::signal::ctrl_c().await.expect("failed to await ctrl-c");
        let _ = shutdown.send(true);
    });

    loop {
        tokio::select! {
            socket = listener.accept() => match socket {
                Ok((stream, local_ip)) => {
                    match socket::accept(stream).await {
                        
                    }
                },
                Err(err) => error!("Couldn't accept client: {err}")
            },
            _ = shutting_down.recv() => {}
        }
    }
    
}