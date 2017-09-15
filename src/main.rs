#[macro_use]
extern crate prost_derive;

extern crate byteorder;
extern crate bytes;
extern crate prost;
extern crate tk_bufstream;
extern crate tk_listen;
extern crate tk_http;
extern crate tokio_core;
extern crate tokio_io;
extern crate futures;

mod protobuf;
mod codec;
mod websocket;

use futures::{Future, Stream};
use std::time::Duration;
use tk_http::server::{Config, Proto};
use tk_listen::ListenExt;
use tokio_core::net::TcpListener;
use tokio_core::reactor::Core;

fn main() {
    let mut core = Core::new().expect("failed to create Core");
    let handle = core.handle();

    let addr = "0.0.0.0:8080".parse().unwrap();
    let listener = TcpListener::bind(&addr, &handle).unwrap();
    let config = Config::new().done();

    let done = listener
        .incoming()
        .sleep_on_error(Duration::from_millis(1000), &handle)
        .map(move |(socket, _addr)| {
            let dispatcher = codec::RequestDispatcher { handle: handle.clone() };

            Proto::new(socket, &config, dispatcher, &handle)
                .map_err(|e| eprintln!("Connection error: {}", e))
                .then(|_| Ok(()))
        })
        .listen(1000);

    println!("Listening on {}", addr);

    core.run(done).unwrap();
}
