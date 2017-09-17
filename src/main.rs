#[macro_use]
extern crate prost_derive;
#[macro_use]
extern crate error_chain;

extern crate byteorder;
extern crate bytes;
extern crate chrono;
extern crate futures;
extern crate hyper;
extern crate hyper_tls;
extern crate petronel;
extern crate prost;
extern crate redis;
extern crate serde_json;
extern crate tk_bufstream;
extern crate tk_http;
extern crate tk_listen;
extern crate tokio_core;
extern crate tokio_io;

mod persistence;
mod error;
mod protobuf;
mod codec;
mod websocket;

use futures::{Future, Stream};
use hyper_tls::HttpsConnector;
use petronel::{ClientBuilder, Token};
use petronel::error::*;
use std::time::Duration;
use tk_http::server::{Config, Proto};
use tk_listen::ListenExt;
use tokio_core::reactor::{Core, Interval};

quick_main!(|| -> Result<()> {
    let token = Token::new(
        env("CONSUMER_KEY")?,
        env("CONSUMER_SECRET")?,
        env("ACCESS_TOKEN")?,
        env("ACCESS_TOKEN_SECRET")?,
    );

    let mut core = Core::new().chain_err(|| "failed to create Core")?;
    let handle = core.handle();

    // TODO: Configurable port
    let bind_address = "0.0.0.0:8080".parse().chain_err(
        || "failed to parse address",
    )?;
    let listener = tokio_core::net::TcpListener::bind(&bind_address, &handle)
        .chain_err(|| "failed to bind TCP listener")?;

    let hyper_client = hyper::Client::configure()
        .connector(HttpsConnector::new(1, &handle).chain_err(|| "HTTPS error")?)
        .build(&handle);

    // TODO: Make cache optional
    let cache = persistence::Cache::new(
        env("REDIS_URL")?,
        "petronel_bosses".to_string(),
        Some("bosses".to_string()),
    )?;
    let bosses = cache.get_bosses()?;

    let (petronel_client, petronel_worker) =
        ClientBuilder::from_hyper_client(&hyper_client, &token)
            .with_history_size(10)
            .with_subscriber::<codec::WebsocketSubscriber<tokio_core::net::TcpStream>>()
            .filter_map_message(protobuf::convert::petronel_message_to_bytes)
            .with_bosses(bosses)
            .with_metrics(petronel::metrics::simple(
                |ref m| serde_json::to_vec(&m).unwrap(),
            ))
            .build();

    let config = Config::new().done();

    // Send heartbeat every 30 seconds
    let heartbeat_petronel_client = petronel_client.clone();
    let heartbeat = Interval::new(Duration::new(30, 0), &core.handle())
        .chain_err(|| "failed to create Interval")?
        .for_each(move |_| Ok(heartbeat_petronel_client.heartbeat()))
        .then(|r| r.chain_err(|| "heartbeat failed"));

    let http_websocket_server = listener
        .incoming()
        .sleep_on_error(Duration::from_millis(1000), &handle)
        .map(move |(socket, _addr)| {
            let dispatcher = codec::RequestDispatcher {
                handle: handle.clone(),
                petronel_client: petronel_client.clone(),
            };

            Proto::new(socket, &config, dispatcher, &handle)
                .map_err(|e| eprintln!("Connection error: {}", e))
                .then(|_| Ok(()))
        })
        .listen(1000)
        .map_err(|()| Error::from_kind(ErrorKind::Msg("HTTP/websocket server failed".into())));

    println!("Listening on {}", bind_address);

    core.run(http_websocket_server.join3(petronel_worker, heartbeat))
        .chain_err(|| "stream failed")?;

    Ok(())
});

fn env(name: &str) -> Result<String> {
    ::std::env::var(name).chain_err(|| {
        format!("invalid value for {} environment variable", name)
    })
}
