#![feature(async_closure)]
#[cfg(test)]
use brio::{App, Ctx, Request, Response, Status};
use futures::Future;
use hyper::{Client, Uri};
use rand::{thread_rng, Rng};
use serde_json::{json, Value};
use std::panic;

use std::{pin::Pin, thread};

mod util;

use util::*;

type BoxFuture<'a, Response> = Pin<Box<dyn Future<Output = Response> + Send + 'static>>;

async fn handler(mut req: Request) -> Response {
    let json = match req.json().await {
        Ok(json) => json,
        Err(_err) => {
            return Response::with_status(Status::BadRequest);
        }
    };
    let mut res = Response::with_status(Status::Ok);
    res.set_json(json);
    res
}

fn logger(ctx: Ctx) -> BoxFuture<Response> {
    println!("request recived: {}", ctx.req().route().path());
    ctx.next()
}

#[tokio::test]
async fn get() -> Result<(), Box<dyn std::error::Error>> {
    let mut rng = thread_rng();
    let port: u32 = rng.gen_range(10000, 20000);
    thread::spawn(move || {
        let mut app = App::new();
        app.get("/bar", async move |_req| Response::with_status(Status::Ok));
        app.middleware("*", logger);
        app.run(port)
    });

    let client = Client::new();
    let uri: Uri = format!("http://127.0.0.1:{}/bar", port).parse()?;
    let resp = client.get(uri).await?;
    assert_eq!(resp.status(), 200);

    Ok(())
}

#[tokio::test]
async fn post() -> Result<(), Box<dyn std::error::Error>> {
    let mut rng = thread_rng();
    let port: u32 = rng.gen_range(10000, 20000);
    thread::spawn(move || {
        let mut app = App::new();
        app.post("/foo", handler);
        app.run(port)
    });

    let body = json!({"hello": "world"});
    let client = Client::new();
    let uri: Uri = format!("http://127.0.0.1:{}/foo", port).parse()?;
    let req = hyper::Request::builder()
        .method(hyper::Method::POST)
        .uri(uri)
        .header("content-type", "application/json")
        .body(hyper::Body::from(serde_json::to_vec(&body)?))?;

    let resp = client.request(req).await?;
    assert_eq!(resp.status(), 200);
    let buf = hyper::body::to_bytes(resp).await?;
    let json = serde_json::from_slice::<Value>(&buf)?;
    assert_eq!(body, json);

    Ok(())
}

#[tokio::test]
async fn multiple_chunks() -> Result<(), Box<dyn std::error::Error>> {
    let mut rng = thread_rng();
    let port: u32 = rng.gen_range(10000, 20000);
    thread::spawn(move || {
        let mut app = App::new();
        app.post("/foo", handler);
        app.middleware("*", logger);
        app.run(port)
    });

    let client = Client::new();
    let uri: Uri = format!("http://127.0.0.1:{}/foo", port).parse()?;
    for _ in 0..2 {
        let body = large_body();
        let req = hyper::Request::builder()
            .method(hyper::Method::POST)
            .uri(uri.clone())
            .header("content-type", "application/json")
            .body(hyper::Body::from(serde_json::to_vec(&body)?))?;
        let resp = client.request(req).await?;
        assert_eq!(resp.status(), 200);
        let buf = hyper::body::to_bytes(resp).await?;
        let json = serde_json::from_slice::<Value>(&buf)?;
        assert_eq!(body, json);
    }

    Ok(())
}

#[tokio::test]
async fn skip_body() -> Result<(), Box<dyn std::error::Error>> {
    let mut rng = thread_rng();
    let port: u32 = rng.gen_range(10000, 20000);
    thread::spawn(move || {
        let mut app = App::new();
        app.post("/foo", handler);
        app.post("/bar", async move |_req| Response::with_status(Status::Ok));
        app.middleware("*", logger);
        app.run(port)
    });

    let client = Client::new();

    let arr = Value::Array(vec![large_body(), large_body()]);

    let uri: Uri = format!("http://127.0.0.1:{}/bar", port).parse()?;
    let req = hyper::Request::builder()
        .method(hyper::Method::POST)
        .uri(uri.clone())
        .header("content-type", "application/json")
        .body(hyper::Body::from(serde_json::to_vec(&arr)?))?;
    let resp = client.request(req).await?;
    assert_eq!(resp.status(), 200);

    let body = large_body();

    let uri: Uri = format!("http://127.0.0.1:{}/foo", port).parse()?;
    let req = hyper::Request::builder()
        .method(hyper::Method::POST)
        .uri(uri.clone())
        .header("content-type", "application/json")
        .body(hyper::Body::from(serde_json::to_vec(&body)?))?;
    let resp = client.request(req).await?;
    assert_eq!(resp.status(), 200);
    let buf = hyper::body::to_bytes(resp).await?;
    let json = serde_json::from_slice::<Value>(&buf)?;
    assert_eq!(body, json);

    Ok(())
}
