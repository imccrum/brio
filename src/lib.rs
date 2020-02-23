#![feature(async_closure)]

use async_std::{
    net::{TcpListener, TcpStream},
    prelude::*,
    task,
};
use futures::{channel::mpsc, join, select, sink::SinkExt, FutureExt};
use futures_timer::Delay;
use std::{
    cmp, collections::hash_map::HashMap, future::Future, pin::Pin, str, sync::Arc, thread,
    time::Duration,
};

mod request;
mod response;
mod router;

pub use request::Request;
pub use response::{Response, Status};
pub use router::Context;

use request::Method;
use router::{Middleware, Path, Route, Router};

pub const BUF_LEN: usize = 256;
pub const KEEP_ALIVE_TIMEOUT: u64 = 10;

pub(crate) type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;
pub(crate) type Sender<T> = mpsc::Sender<T>;
pub(crate) type Receiver<T> = mpsc::Receiver<T>;
pub(crate) type BoxFuture<'a, Response> = Pin<Box<dyn Future<Output = Response> + Send + 'static>>;
#[derive(Debug)]
pub enum Void {}
pub struct App<Routes> {
    router: Router<Routes>,
}

impl<Routes: Send + Sync + Copy + Clone + 'static> App<Routes> {
    pub fn new(_routes: Routes) -> App<()> {
        App {
            router: Router::new(()),
        }
    }
    pub fn run(self, port: u32) -> Result<()> {
        let router = Arc::new(self.router);
        task::block_on(accept_loop("127.0.0.1", port, router))
    }
    pub fn get(&mut self, path: &'static str, handler: impl Route) -> &Self {
        self.add_route(Path::new(Method::Get, path.to_owned()), handler);
        self
    }
    pub fn post(&mut self, path: &'static str, handler: impl Route) -> &Self {
        self.add_route(Path::new(Method::Post, path.to_owned()), handler);
        self
    }
    pub fn put(&mut self, path: &'static str, handler: impl Route) -> &Self {
        self.add_route(Path::new(Method::Put, path.to_owned()), handler);
        self
    }
    pub fn delete(&mut self, path: &'static str, handler: impl Route) -> &Self {
        self.add_route(Path::new(Method::Delete, path.to_owned()), handler);
        self
    }
    pub fn middleware(&mut self, middleware: impl Middleware + 'static) -> &Self {
        self.add_middleware(middleware);
        self
    }
    fn add_route(&mut self, route: Path, handler: impl Route) {
        self.router.routes.insert(route, Box::new(handler));
    }
    fn add_middleware(&mut self, middleware: impl Middleware + 'static) {
        self.router.middleware.push(Arc::new(middleware));
    }
}

async fn accept_loop<Routes: Send + Sync + Copy + Clone + 'static>(
    host: &'static str,
    port: u32,
    router: Arc<Router<Routes>>,
) -> Result<()> {
    let listener = TcpListener::bind(format!("{}:{}", host, port)).await?;
    println!("listening on {}:{}", host, port);
    let mut incoming = listener.incoming();
    let mut sigterm_rx = register_sigterm_listener()?.fuse();
    loop {
        select! {
            stream = incoming.next().fuse() => match stream {
                Some(stream) => {
                    println!("DEBUG did something happen yes?");
                    let stream = stream?;
                    println!("accepting from: {}", stream.peer_addr()?);
                    spawn_and_log_error(keep_alive_loop(stream, router.clone()));
                },
                None => {
                    println!("DEBUG did something happen no?");
                    break;
                }
            },
            _ = sigterm_rx =>  {
                break;
            }
        }
    }
    println!("shutting down");
    Ok(())
}

fn spawn_and_log_error<F>(fut: F) -> task::JoinHandle<()>
where
    F: Future<Output = Result<()>> + Send + 'static,
{
    task::spawn(async move {
        if let Err(e) = fut.await {
            eprintln!("{}", e)
        }
    })
}

async fn keep_alive_loop<Routes: Send + Sync + Copy + Clone + 'static>(
    mut stream: TcpStream,
    router: Arc<Router<Routes>>,
) -> Result<()> {
    loop {
        select! {
            keep_alive = request_loop(&mut stream, router.clone()).fuse() => {
                if !keep_alive? {
                    println!("client did not request keep-alive, closing..");
                    break;
                }
            },
            _ = Delay::new(Duration::from_secs(KEEP_ALIVE_TIMEOUT)).fuse() => {
                println!("keep-alive timeout expired, closing..");
            }
        }
    }
    Ok(())
}

async fn request_loop<Routes: Send + Sync + Copy + Clone + 'static>(
    stream: &mut TcpStream,
    router: Arc<Router<Routes>>,
) -> Result<bool> {
    let writer = stream.clone();
    let mut reader = stream.clone();
    let mut buf = [0u8; BUF_LEN];

    let mut req = parse_loop(&mut reader, &mut buf).await?;

    let content_length = req.content_length as usize;
    let body_read = req.bytes.len() as usize;
    let (body_sender, body_receiver) = mpsc::channel(1);
    if req.content_length > 0 {
        req.set_body(body_receiver)
    }

    let request_handle = task::spawn(handle_request(req, writer, router));
    let body_handle = body_loop(&mut reader, body_read, content_length, body_sender);
    let (_, keep_alive) = join!(body_handle, request_handle);

    Ok(keep_alive?)
}

async fn parse_loop<'a>(reader: &'a mut TcpStream, buf: &'a mut [u8]) -> Result<Request> {
    let mut total_bytes_read = 0;
    loop {
        let bytes_read = reader.read(buf).await?;
        if bytes_read == 0 {
            return Err(Box::new(std::io::Error::new(
                std::io::ErrorKind::ConnectionReset,
                "client disconnected",
            )));
        }
        let mut headers = [httparse::EMPTY_HEADER; 16];
        let mut parser = httparse::Request::new(&mut headers);
        let parse_res = parser.parse(&buf)?;
        if parse_res.is_partial() {
            total_bytes_read += bytes_read;
        } else {
            let header_len = parse_res.unwrap();
            let headers: HashMap<String, String> = parser
                .headers
                .iter()
                .map(|&x| {
                    (
                        x.name.to_owned().to_lowercase(),
                        str::from_utf8(x.value).unwrap().to_owned(),
                    )
                })
                .collect();
            let content_length: usize = match headers.get("content-length") {
                Some(cl) => cl.parse()?,
                None => 0,
            };
            let header_end: usize = header_len - total_bytes_read;
            let body_end: usize = header_end + content_length;
            let mut bytes = vec![];
            bytes.extend_from_slice(&buf[header_end..cmp::min(body_end, BUF_LEN)]);
            return Ok(Request::from_parts(
                parser,
                bytes,
                content_length as u64,
                headers,
            )?);
        }
    }
}

async fn body_loop<'a>(
    stream: &mut TcpStream,
    mut body_read: usize,
    content_length: usize,
    mut body_sender: Sender<Event>,
) -> Result<()> {
    let mut body = stream.take((content_length - body_read) as u64);
    while body_read < content_length {
        let mut body_buf = [0u8; BUF_LEN];
        select! {
            res = body.read(&mut body_buf).fuse() => match res {
                Ok(bytes_read) => {
                    if bytes_read == 0 {
                        // disconnected
                        break;
                    }
                    body_read += bytes_read;
                    if let Err(err) = body_sender
                        .send(Event::Message { msg: body_buf, size: bytes_read })
                        .await {
                            break;
                        }
                },
                Err(err) => {
                    break;
                }
            },
        }
    }
    drop(body_sender);
    if body_read < content_length {
        println!("skipping remaining bytes???");
        futures::io::copy(body, &mut futures::io::sink()).await?;
    }
    Ok(())
}

async fn handle_request<Routes: Send + Sync + Copy + Clone + 'static>(
    req: Request,
    mut writer: TcpStream,
    router: Arc<Router<Routes>>,
) -> Result<bool> {
    let keep_alive = req.is_keep_alive();
    let handler = match router.routes.get(&req.path()) {
        Some(route) => route,
        None => &router.not_found,
    };
    let context = Context {
        req: req,
        endpoint: handler,
        next_middleware: router.middleware.as_slice(),
    };
    let mut res = context.next().await;
    if keep_alive {
        res.headers
            .insert("connection".to_owned(), "keep-alive".to_owned());
        res.headers.insert(
            "keep-alive".to_owned(),
            format!("timeout={}, max=1000", KEEP_ALIVE_TIMEOUT),
        );
    }
    writer.write_all(res.to_bytes().as_slice()).await.unwrap();
    Ok(keep_alive)
}

pub enum Event {
    Message { msg: [u8; BUF_LEN], size: usize },
}

fn register_sigterm_listener() -> Result<futures::channel::oneshot::Receiver<bool>> {
    let signals =
        signal_hook::iterator::Signals::new(&[signal_hook::SIGTERM, signal_hook::SIGINT])?;
    let (sigterm_tx, sigterm_rx) = futures::channel::oneshot::channel::<bool>();
    thread::spawn(move || {
        let mut count = 0u32;
        for _signal in signals.forever() {
            count += 1;
            if count > 0 {
                break;
            }
        }
        sigterm_tx.send(true).expect("shutdown notify failed");
    });
    Ok(sigterm_rx)
}

async fn not_found(_req: Request) -> Response {
    Response::status(Status::NotFound)
}

#[cfg(test)]
mod tests {

    //cargo test keeps_buffer -- --nocapture --test-threads=1

    use crate::{App, Context, Request, Response, Status};
    use futures::Future;
    use httparse;
    use hyper::{Client, Uri};
    use rand::{thread_rng, Rng};
    use serde_json::{json, Value};
    use std::panic;

    use std::{
        cmp,
        collections::HashMap,
        io::prelude::*,
        net::{Shutdown, TcpStream},
        pin::Pin,
        thread, time,
        time::Duration,
    };

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

    #[test]
    fn keeps_buffer() -> std::result::Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let port: u32 = run_app();
        let uri = format!("127.0.0.1:{}", port);
        let body = json!({"hello": "world"});
        let mut req = connect(&uri)?;
        req.write_all(
            b"\
            POST /foo HTTP/1.1\r\n\
            Host: localhost\r\n\
            Content-Type: application/json\r\n\
            Content-Length: 18\r\n\
            \r\n\
            {\"hello\": \"world\"}\r\n\
        ",
        )
        .unwrap();

        let (_headers, _body) = parse(&mut req)?;
        println!("headers {:?}, body {:?}", _headers, _body);
        let json = serde_json::from_slice::<Value>(&_body)?;
        assert_eq!(body, json);
        Ok(())
    }

    fn parse<'a>(
        req: &'a mut TcpStream,
    ) -> Result<(HashMap<String, String>, Vec<u8>), Box<dyn std::error::Error + Send + Sync>> {
        pub const BUF_LEN: usize = 256;
        let mut total_bytes_read = 0;
        loop {
            let mut buf = [0u8; BUF_LEN];
            let bytes_read = req.read(&mut buf)?;
            if bytes_read == 0 {
                return Err(Box::new(std::io::Error::new(
                    std::io::ErrorKind::ConnectionReset,
                    "client disconnected",
                )));
            }
            let mut headers = [httparse::EMPTY_HEADER; 16];
            let mut parser = httparse::Response::new(&mut headers);
            let parse_res = parser.parse(&buf)?;
            if parse_res.is_partial() {
                total_bytes_read += bytes_read;
            } else {
                let header_len = parse_res.unwrap();
                let headers: HashMap<String, String> = parser
                    .headers
                    .iter()
                    .map(|&x| {
                        (
                            x.name.to_owned().to_lowercase(),
                            std::str::from_utf8(x.value).unwrap().to_owned(),
                        )
                    })
                    .collect();
                let content_length: usize = match headers.get("content-length") {
                    Some(cl) => cl.parse()?,
                    None => 0,
                };
                let header_end: usize = header_len - total_bytes_read;
                let body_end: usize = header_end + content_length;
                let mut bytes = vec![];
                bytes.extend_from_slice(&buf[header_end..cmp::min(body_end, BUF_LEN)]);

                let mut take = req.take((content_length - bytes.len()) as u64);
                take.read_to_end(&mut bytes)?;
                return Ok((headers, bytes));
            }
        }
    }

    fn run_app() -> u32 {
        let mut rng = thread_rng();
        let port: u32 = rng.gen_range(10000, 20000);
        thread::spawn(move || {
            let mut app = App::new(());
            app.post("/foo", handler);
            app.post("/bar", async move |_req| Response::status(Status::Ok));
            app.middleware(logger);
            app.run(port)
        });

        let mut attempts = 0;
        while attempts < 10 {
            match connect(&format!("127.0.0.1:{}", port)) {
                Ok(req) => {
                    req.shutdown(Shutdown::Both).unwrap();
                    break;
                }
                Err(_) => {
                    if attempts > 10 {
                        panic!("could not connect!");
                    } else {
                        thread::sleep(time::Duration::from_millis(10));
                        attempts += 1;
                    }
                }
            }
        }

        port
    }

    fn connect(
        addr: &str,
    ) -> Result<std::net::TcpStream, Box<dyn std::error::Error + Send + Sync>> {
        let req = TcpStream::connect(addr)?;
        req.set_read_timeout(Some(Duration::from_secs(1)))?;
        req.set_write_timeout(Some(Duration::from_secs(1)))?;
        Ok(req)
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
}
