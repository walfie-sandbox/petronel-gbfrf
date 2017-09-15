#[macro_use]
extern crate prost_derive;
#[macro_use]
extern crate error_chain;

extern crate hyper;
extern crate hyper_tls;
extern crate byteorder;
extern crate bytes;
extern crate futures;
extern crate petronel;
extern crate prost;
extern crate tk_bufstream;
extern crate tk_http;
extern crate tk_listen;
extern crate tokio_core;
extern crate tokio_io;

mod error;
mod protobuf;
mod codec;
mod websocket;

use bytes::Bytes;
use error::*;
use futures::{Future, Stream};
use hyper_tls::HttpsConnector;
use petronel::{Client, ClientBuilder, Subscriber, Subscription, Token};
use petronel::metrics;
use petronel::model::{BossName, Message as PetronelMessage};
use std::time::Duration;
use tk_http::server::{Config, Proto};
use tk_listen::ListenExt;
use tokio_core::net::TcpListener;
use tokio_core::reactor::Core;

fn petronel_message_filter_map(msg: PetronelMessage) -> Option<Bytes> {
    use PetronelMessage::*;
    use protobuf::ResponseMessage;
    use protobuf::response_message::Data::*;


    let data = match msg {
        Heartbeat => Some(KeepAliveMessage(protobuf::KeepAliveResponse {})),
        Tweet(tweet) => None,
        TweetList(tweets) => None,
        BossUpdate(boss) => None,
        BossList(bosses) => None,
        BossRemove(boss_name) => None,
    };

    data.and_then(|d| {
        websocket::serialize_protobuf(ResponseMessage { data: Some(d) })
    })
}

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
        .connector(HttpsConnector::new(4, &handle).chain_err(|| "HTTPS error")?)
        .build(&handle);

    use tokio_io::{AsyncRead, AsyncWrite};
    let (petronel_client, petronel_worker) = ClientBuilder::from_hyper_client(&hyper_client, &token)
            .with_history_size(10)
            //.with_metrics(metrics::simple(|ref m| m)) // TODO
            .with_subscriber::<codec::WebsocketSubscriber<tokio_core::net::TcpStream>>()
            .filter_map_message(petronel_message_filter_map)
            .build();

    let config = Config::new().done();

    let done = listener
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
        .listen(1000);

    println!("Listening on {}", bind_address);

    core.run(done).expect("failed to run");
    Ok(())
});

fn env(name: &str) -> Result<String> {
    ::std::env::var(name).chain_err(|| {
        format!("invalid value for {} environment variable", name)
    })
}
