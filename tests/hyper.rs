#![feature(async_closure)]
//cargo test keeps_buffer -- --nocapture --test-threads=1

use brio::{App, Context, Request, Response, Status};
use futures::Future;
use hyper::{Client, Uri};
use rand::{thread_rng, Rng};
use serde_json::{json, Value};
use std::panic;

use std::{pin::Pin, thread};

type BoxFuture<'a, Response> = Pin<Box<dyn Future<Output = Response> + Send + 'static>>;

async fn handler(mut req: Request) -> Response {
    let json = match req.json().await {
        Ok(json) => json,
        Err(_err) => {
            return Response::status(Status::BadRequest);
        }
    };
    let mut res = Response::status(Status::Ok);
    res.json(json);
    res
}

fn logger(ctx: Context) -> BoxFuture<Response> {
    println!("request recived: {}", ctx.req.path);
    ctx.next()
}

#[tokio::test]
async fn get() -> Result<(), Box<dyn std::error::Error>> {
    let mut rng = thread_rng();
    let port: u32 = rng.gen_range(10000, 20000);
    thread::spawn(move || {
        let mut app = App::new(());
        app.get("/bar", async move |_req| Response::status(Status::Ok));
        app.middleware(logger);
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
        let mut app = App::new(());
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
        let mut app = App::new(());
        app.post("/foo", handler);
        app.middleware(logger);
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
        let mut app = App::new(());
        app.post("/foo", handler);
        app.post("/bar", async move |_req| Response::status(Status::Ok));
        app.middleware(logger);
        app.run(port)
    });

    let body = large_body();
    let arr = vec![body.clone(), body.clone()];

    let client = Client::new();

    let uri: Uri = format!("http://127.0.0.1:{}/bar", port).parse()?;
    let req = hyper::Request::builder()
        .method(hyper::Method::POST)
        .uri(uri.clone())
        .header("content-type", "application/json")
        .body(hyper::Body::from(serde_json::to_vec(&arr)?))?;
    let resp = client.request(req).await?;
    assert_eq!(resp.status(), 200);

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

fn large_body() -> Value {
    json!({
        "text":
        "Two households, both alike in dignity,
  In fair Verona, where we lay our scene,
  From ancient grudge break to new mutiny,
  Where civil blood makes civil hands unclean.
  From forth the fatal loins of these two foes
  A pair of star-cross'd lovers take their life;
  Whose misadventured piteous overthrows
  Do with their death bury their parents' strife.
  The fearful passage of their death-mark'd love,
  And the continuance of their parents' rage,
  Which, but their children's end, nought could remove,
  Is now the two hours' traffic of our stage;
  The which if you with patient ears attend,
  What here shall miss, our toil shall strive to mend.",
    })
}
