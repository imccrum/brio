#![feature(async_closure)]
use brio::App;
use brio::Context;
use brio::Request;
use brio::Response;
use brio::Status;
use std::future::Future;
use std::pin::Pin;

type BoxFuture<'a, Response> = Pin<Box<dyn Future<Output = Response> + Send + 'static>>;

// curl -v -H 'connection:keep-alive' localhost:8000/foo localhost:8000/foo
// curl -v --http1.0 -H 'connection:keep-alive' localhost:8000/foo localhost:8000/foo
// curl -v -X POST localhost:8000/bar -d '{"hello": "world"}'
// wrk -t12 -c400 -d30s http://127.0.0.1:8000/foo
// cargo test chunked -- --nocapture --test-threads=1
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
    let res = Response::status(Status::Ok);
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
