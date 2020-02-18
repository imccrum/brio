#![feature(async_closure)]
use loops::App;
use loops::Context;
use loops::Request;
use loops::Response;
use loops::Status;
use serde_json::json;
use std::future::Future;
use std::pin::Pin;

type BoxFuture<'a, Response> = Pin<Box<dyn Future<Output = Response> + Send + 'static>>;

fn main() {
    let mut app = App::new(());
    app.get("/foo", foo);
    app.post("/bar", bar);
    app.get("/baz", async move |_req: Request| {
        Response::status(Status::Ok)
    });
    app.middleware(logger);
    app.run(8000).unwrap();
}

fn logger(ctx: Context) -> BoxFuture<Response> {
    ctx.next()
}

async fn foo(_req: Request) -> Response {
    let json = json!({
        "hello": "world"
    });
    let mut res = Response::status(Status::Ok);
    res.json(json);
    res
}

async fn bar(mut req: Request) -> Response {
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
