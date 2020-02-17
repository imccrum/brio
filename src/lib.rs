#![feature(async_closure)]

use async_std::{
    net::{TcpListener, TcpStream, ToSocketAddrs},
    prelude::*,
    task,
};
use futures::channel::mpsc;
use futures::sink::SinkExt;
use futures::{join, select, FutureExt};
use std::{
    cmp,
    collections::hash_map::HashMap,
    future::Future,
    pin::Pin,
    str,
    sync::{Arc, Mutex},
    thread,
};

const BUF_LEN: usize = 256;

type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;
type Sender<T> = mpsc::Sender<T>;
type Receiver<T> = mpsc::Receiver<T>;
type BoxFuture<'a, Response> = Pin<Box<dyn Future<Output = Response> + Send + 'static>>;

#[derive(Debug)]
pub enum Void {}

pub struct App<Routes> {
    router: Router<Routes>,
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

impl<Routes: Send + Sync + Copy + Clone + 'static> App<Routes> {
    pub fn new(_routes: Routes) -> App<()> {
        App {
            router: Router::new(()),
        }
    }
    pub fn run(self, port: u32) -> Result<()> {
        let router = Arc::new(self.router);
        task::block_on(accept_loop(format!("127.0.0.1:{}", port), router))
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

pub struct Router<Routes> {
    pub routes: HashMap<Path, Box<dyn Route + 'static>>,
    pub middleware: Vec<Arc<dyn Middleware>>,
    pub config: Routes,
    pub not_found: Box<dyn Route + 'static>,
}

impl<Routes: Send + Sync + Copy + Clone + 'static> Router<Routes> {
    pub fn new(config: Routes) -> Router<Routes> {
        Router {
            routes: HashMap::new(),
            middleware: vec![],
            config: config,
            not_found: Box::new(not_found),
        }
    }
}

#[derive(Debug, Hash, Eq, PartialEq)]
pub struct Path {
    method: Method,
    path: String,
}

impl Path {
    pub fn new(method: Method, path: String) -> Path {
        Path { method, path }
    }
}

async fn accept_loop<Routes: Send + Sync + Copy + Clone + 'static>(
    addr: impl ToSocketAddrs,
    router: Arc<Router<Routes>>,
) -> Result<()> {
    let listener = TcpListener::bind(addr).await?;
    let mut incoming = listener.incoming();
    let mut sigterm_rx = register_sigterm_listener()?.fuse();
    loop {
        select! {
            stream = incoming.next().fuse() => match stream {
                Some(stream) => {
                    let stream = stream?;
                    println!("accepting from: {}", stream.peer_addr()?);
                    spawn_and_log_error(keep_alive_loop(stream, router.clone()));
                },
                None => {
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

async fn keep_alive_loop<Routes: Send + Sync + Copy + Clone + 'static>(
    mut stream: TcpStream,
    router: Arc<Router<Routes>>,
) -> Result<()> {
    loop {
        if !request_loop(&mut stream, router.clone()).await? {
            // not keep-alive
            break;
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
    let keep_alive = req.keep_alive();

    let request_handle = task::spawn(handle_request(req, writer, router));
    let body_handle = body_loop(&mut reader, body_read, content_length, body_sender);
    let _ = join!(body_handle, request_handle);

    Ok(keep_alive)
}

async fn parse_loop<'a>(reader: &'a mut TcpStream, buf: &'a mut [u8]) -> Result<Request> {
    let mut total_bytes_read = 0;
    loop {
        let bytes_read = reader.read(buf).await?;
        if bytes_read == 0 {
            return Err(Box::new(std::io::Error::new(
                std::io::ErrorKind::ConnectionReset,
                "disconnected",
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
            let content_length: usize = headers
                .get("content-length")
                .ok_or(Box::new(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "missing content-length",
                )))?
                .parse()?;
            let header_end: usize = header_len - total_bytes_read;
            let body_end: usize = header_end + content_length;
            // read body
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
                    body_sender
                        .send(Event::Message { msg: body_buf, size: bytes_read })
                        .await
                        .unwrap();
                    body_read += bytes_read;
                },
                Err(err) => {
                    break;
                }
            },
        }
    }
    drop(body_sender);
    if body_read < content_length {
        futures::io::copy(body, &mut futures::io::sink()).await?;
    }
    Ok(())
}

async fn handle_request<Routes: Send + Sync + Copy + Clone + 'static>(
    req: Request,
    mut writer: TcpStream,
    router: Arc<Router<Routes>>,
) -> Result<()> {
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
    writer.write_all(res.to_bytes().as_slice()).await.unwrap();
    Ok(())
}

pub struct Request {
    pub bytes: Vec<u8>,
    pub version: u8,
    pub method: Method,
    pub path: String,
    pub headers: HashMap<String, String>,
    pub content_length: u64,
    pub body: Arc<Mutex<Option<Receiver<Event>>>>,
}

impl Request {
    fn from_parts(
        parsed: httparse::Request,
        bytes: Vec<u8>,
        content_length: u64,
        headers: HashMap<String, String>,
    ) -> Result<Request> {
        Ok(Request {
            bytes: bytes,
            version: parsed.version.unwrap(),
            path: parsed.path.unwrap().to_owned(),
            method: parsed.method.unwrap().to_lowercase().parse().unwrap(),
            headers: headers,
            content_length: content_length,
            body: Arc::new(Mutex::new(None)),
        })
    }

    pub fn set_body(&mut self, receiver: Receiver<Event>) {
        self.body = Arc::new(Mutex::new(Some(receiver)))
    }

    pub fn keep_alive(&self) -> bool {
        self.headers
            .get("connection")
            .map(|h| h.parse().unwrap())
            .unwrap_or(false)
    }

    pub fn path(&self) -> Path {
        Path::new(self.method, self.path.clone())
    }

    pub async fn json(&mut self) -> serde_json::Value {
        self.body().await;
        println!("utf8 {:?}", std::str::from_utf8(&self.bytes));
        serde_json::from_slice(&self.bytes.as_slice()).unwrap()
    }

    pub async fn bytes(&mut self) -> &Vec<u8> {
        self.body().await;
        &self.bytes
    }

    async fn body(&mut self) {
        let take = self.body.clone().lock().unwrap().take();
        let mut stream = take.unwrap().fuse();
        while self.bytes.len() < self.content_length as usize {
            let chunk = select! {
                chunk = stream.next().fuse() => match chunk {
                    None => break, // 2
                    Some(chunk) => chunk,
                },
            };
            match chunk {
                Event::Message { msg, size } => {
                    self.bytes.extend_from_slice(&msg[..size]);
                }
            }
        }
    }
}
#[derive(Debug, Hash, Eq, PartialEq, Copy, Clone)]
pub enum Method {
    Get,
    Head,
    Post,
    Put,
    Delete,
    Connect,
    Options,
    Trace,
    Patch,
}

impl std::str::FromStr for Method {
    type Err = ();
    fn from_str(s: &str) -> std::result::Result<Self, ()> {
        match s {
            "get" => Ok(Method::Get),
            "head" => Ok(Method::Head),
            "post" => Ok(Method::Post),
            "put" => Ok(Method::Put),
            "delete" => Ok(Method::Delete),
            "connect" => Ok(Method::Connect),
            "options" => Ok(Method::Options),
            "trace" => Ok(Method::Trace),
            "patch" => Ok(Method::Patch),
            _ => Err(()),
        }
    }
}

pub enum Event {
    Message { msg: [u8; BUF_LEN], size: usize },
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

pub trait Route: Send + Sync + 'static {
    fn run<'a>(&'a self, req: Request) -> BoxFuture<'a, Response>;
}

impl<F: Send + Sync + 'static, Fut> Route for F
where
    F: Fn(Request) -> Fut,
    Fut: Future<Output = Response> + Send + 'static,
{
    fn run<'a>(&'a self, req: Request) -> BoxFuture<'a, Response> {
        let fut = (self)(req);
        Box::pin(async move { fut.await })
    }
}

pub trait Middleware: Send + Sync {
    fn handle<'a>(&'a self, ctx: Context<'a>) -> BoxFuture<'a, Response>;
}

impl<F> Middleware for F
where
    F: Send + Sync + Fn(Context) -> BoxFuture<Response>,
{
    fn handle<'a>(&'a self, ctx: Context<'a>) -> BoxFuture<'a, Response> {
        (self)(ctx)
    }
}

pub struct Context<'a> {
    pub req: Request,
    pub(crate) endpoint: &'a Box<dyn Route>,
    pub(crate) next_middleware: &'a [Arc<dyn Middleware>],
}

impl<'a> Context<'a> {
    pub fn next(mut self) -> BoxFuture<'a, Response> {
        if let Some((current, next)) = self.next_middleware.split_first() {
            self.next_middleware = next;
            current.handle(self)
        } else {
            self.endpoint.run(self.req)
        }
    }
}

pub struct Response {
    pub status: Status,
    pub headers: HashMap<String, String>,
    pub body: Vec<u8>,
    pub bytes: Vec<u8>,
    pub content_length: usize,
}

impl Response {
    pub fn status(status: Status) -> Response {
        Response {
            status,
            headers: HashMap::new(),
            body: vec![],
            bytes: vec![],
            content_length: 0,
        }
    }

    pub fn json(&mut self, json: serde_json::Value) {
        self.body = json.to_string().into_bytes();
    }

    pub fn to_bytes(&mut self) -> Vec<u8> {
        let mut bytes: Vec<u8> = vec![];
        self.headers
            .insert("content-length".to_owned(), self.body.len().to_string());
        if !self.body.is_empty() {
            self.headers
                .insert("content-type".to_owned(), "application/json".to_owned());
        }
        bytes.extend_from_slice(b"HTTP/1.1 ");
        bytes.extend_from_slice(self.status.bytes());
        let mut headers: Vec<u8> = self
            .headers
            .drain()
            .flat_map(|mut e| {
                e.0.push_str(": ");
                e.0.push_str(&e.1);
                e.0.push_str("\r\n");
                e.0.into_bytes()
            })
            .collect();
        bytes.append(&mut headers);
        bytes.extend_from_slice(b"\r\n");
        bytes.append(&mut self.body);
        bytes
    }
}

#[derive(Debug)]
pub enum Status {
    Ok,
    BadRequest,
    NotFound,
    RequestTimeout,
}

impl Status {
    pub fn bytes(&self) -> &[u8] {
        match *self {
            Status::Ok => b"200 OK\r\n",
            Status::BadRequest => b"400 Bad Request\r\n",
            Status::NotFound => b"404 Not Found\r\n",
            Status::RequestTimeout => b"408 Request Timeout\r\n",
        }
    }
}

async fn not_found(_req: Request) -> Response {
    Response::status(Status::NotFound)
}

#[cfg(test)]
mod tests {
    use crate::{App, Context, Request, Response, Status};
    use futures::Future;
    use rand::{thread_rng, Rng};
    use reqwest::Client;
    use serde_json::{json, Value};
    use std::{pin::Pin, thread};

    type BoxFuture<'a, Response> = Pin<Box<dyn Future<Output = Response> + Send + 'static>>;

    async fn handler(mut req: Request) -> Response {
        let json = req.json().await;
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
            app.get("/foo", handler);
            app.middleware(logger);
            app.run(port)
        });

        println!("making request...");

        let client = Client::new();

        let body = json!({
            "hello": "world"
        });
        let resp = client
            .get(&format!("http://127.0.0.1:{}/foo", port))
            .json(&body)
            .send()
            .await?;

        println!("response status {:?}", resp.status());

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

        println!("making request...");

        let client = Client::new();

        let body = json!({
            "hello": "world"
        });
        let resp = client
            .post(&format!("http://127.0.0.1:{}/foo", port))
            .json(&body)
            .send()
            .await?;
        let json = resp.json::<Value>().await?;

        assert_eq!(body, json);

        Ok(())
    }

    #[tokio::test]
    async fn large() -> Result<(), Box<dyn std::error::Error>> {
        let mut rng = thread_rng();
        let port: u32 = rng.gen_range(10000, 20000);
        thread::spawn(move || {
            let mut app = App::new(());
            app.post("/foo", handler);
            app.get("/baz", async move |_req: Request| {
                Response::status(Status::Ok)
            });
            app.middleware(logger);
            app.run(port)
        });

        let client = Client::new();
        let body = json!({
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
        });

        println!("content-length {}", body.to_string());
        println!("content-length {}", body.to_string().len());

        for _ in 0..2 {
            println!("making request...");
            let resp = client
                .post(&format!("http://127.0.0.1:{}/foo", port))
                .json(&body)
                .send()
                .await?;
            let json = resp.json::<Value>().await?;
            assert_eq!(body, json);
        }

        Ok(())
    }

    #[tokio::test]
    async fn keep_alive() -> Result<(), Box<dyn std::error::Error>> {
        let mut rng = thread_rng();
        let port: u32 = rng.gen_range(10000, 20000);
        thread::spawn(move || {
            let mut app = App::new(());
            app.post("/foo", handler);
            app.run(port)
        });

        let client = Client::new();
        let body = json!({
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
        });

        for _ in 0..2 {
            println!("making request...");
            let resp = client
                .post(&format!("http://127.0.0.1:{}/foo", port))
                .json(&body)
                .send()
                .await?;
            let json = resp.json::<Value>().await?;
            assert_eq!(body, json);
        }

        Ok(())
    }
}
