#[macro_use]
extern crate prost_derive;

extern crate tk_listen;
extern crate tk_http;
extern crate tokio_core;
extern crate futures;

mod protobuf;

use futures::{Future, Stream};
use futures::future::{self, FutureResult};
use std::time::Duration;
use tk_http::Status;
use tk_http::server::{Config, Encoder, EncoderDone, Error, Proto};
use tk_http::server::buffered::{BufferedDispatcher, Request};
use tk_listen::ListenExt;
use tokio_core::net::TcpListener;
use tokio_core::reactor::Core;

fn process_http<S>(req: Request, mut e: Encoder<S>) -> FutureResult<EncoderDone<S>, Error> {
    if let Some(ws) = req.websocket_handshake() {
        e.status(Status::SwitchingProtocol);
        e.add_header("Connection", "upgrade").unwrap();
        e.add_header("Upgrade", "websocket").unwrap();
        e.format_header("Sec-Websocket-Accept", &ws.accept).unwrap();
        e.done_headers().unwrap();
        future::ok(e.done())
    } else {
        let body = "Not implemented yet";

        e.status(Status::Ok);
        e.add_header("Content-Type", "text/plain").unwrap();
        e.add_length(body.as_bytes().len() as u64).unwrap();

        if e.done_headers().unwrap() {
            e.write_body(body.as_bytes());
        }

        future::ok(e.done())
    }
}

fn main() {
    let mut core = Core::new().expect("failed to create Core");
    let handle = core.handle();

    let addr = "0.0.0.0:8080".parse().unwrap();
    let listener = TcpListener::bind(&addr, &handle).unwrap();
    let config = Config::new().done();

    let done = listener
        .incoming()
        .sleep_on_error(Duration::from_millis(1000), &handle)
        .map(move |(socket, addr)| {
            let dispatcher = BufferedDispatcher::new_with_websockets(
                addr,
                &handle,
                process_http,
                |output, input| {
                    input.forward(output).map(|_| ()).map_err(
                        |e| eprintln!("{:?}", e),
                    )
                },
            );

            Proto::new(socket, &config, dispatcher, &handle)
                .map_err(|e| eprintln!("Connection error: {}", e))
                .then(|_| Ok(()))
        })
        .listen(1000);

    core.run(done).unwrap();
}
