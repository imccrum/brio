# brio

Toy async http server and web framework

[![MIT licensed](https://img.shields.io/badge/license-MIT-blue.svg)](./LICENSE)

```rust
#![feature(async_closure)]

use brio::{App, Ctx, Request, Response, Status};
use log::info;
use std::{future::Future, pin::Pin, time::Instant};

type BoxFuture<'a, Response> = Pin<Box<dyn Future<Output = Response> + Send + 'static>>;

async fn foo(_: Request) -> Response {
    Response::with_status(Status::Ok)
}

fn main() {
    std::env::set_var("RUST_LOG", "brio=info,server=debug");
    env_logger::init();
    let mut app = App::new();
    app.get("/foo", foo);
    app.get("/bar", async move |_req: Request| {
        Response::with_status(Status::Ok)
    });
    app.middleware("*", logger);
    app.files("/public/", "./examples/static/");
    app.run(8000).unwrap();
}

fn logger(ctx: Ctx) -> BoxFuture<Response> {
    let now = Instant::now();
    let path = ctx.req().route().path().clone();
    let method = ctx.req().route().method().unwrap().clone();
    let fut = ctx.next();
    Box::pin(async move {
        let res = fut.await;
        info!(
            "request {} {} took {:?} ({})",
            method,
            path,
            Instant::now().duration_since(now),
            res.status() as u32
        );
        res
    })
}
```
