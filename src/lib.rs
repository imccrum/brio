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

use request::{Encoding, Event, Method};
use router::{Middleware, Path, Route, Router};

pub const BUF_LEN: usize = 256;
pub const KEEP_ALIVE_TIMEOUT: u64 = 10;

pub(crate) type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;
pub(crate) type Sender<T> = mpsc::Sender<T>;
pub(crate) type Receiver<T> = mpsc::Receiver<T>;
pub(crate) type BoxFuture<'a, Response> = Pin<Box<dyn Future<Output = Response> + Send + 'static>>;

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
    let transfer_encoding = req.transfer_endcoding();
    let body_read = req.bytes.len() as usize;
    let (body_sender, body_receiver) = mpsc::channel(1);
    if req.content_length > 0 {
        req.set_body(body_receiver)
    }

    let request_handle = task::spawn(handle_request(req, writer, router));
    let body_handle = body_loop(
        &mut reader,
        body_read,
        content_length,
        transfer_encoding,
        body_sender,
    );
    let (_, keep_alive) = join!(body_handle, request_handle);

    Ok(keep_alive?)
}

async fn parse_loop<'a>(reader: &'a mut TcpStream, buf: &'a mut [u8]) -> Result<Request> {
    //
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
        total_bytes_read += bytes_read;
        if parse_res.is_complete() {
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

            let header_end_index: usize = header_len - (total_bytes_read - bytes_read) - 1;
            let body_end_index: usize = cmp::min(header_end_index + content_length, bytes_read - 1);
            println!(
                "debug_header_end_index {}, debug_body_end_index {}",
                header_end_index, body_end_index
            );
            let mut bytes = vec![];
            bytes.extend_from_slice(&buf[(header_end_index + 1)..=body_end_index]);
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
    transfer_encoding: Encoding,
    mut tx: Sender<Event>,
) -> Result<()> {
    if transfer_encoding == Encoding::Chunked {
        let mut total_bytes_read = 0;
        let mut body_buf = [0u8; BUF_LEN];
        let mut unread = 0;
        loop {
            let bytes_read = stream.read(&mut body_buf[unread..]).await?;
            if bytes_read == 0 {
                break;
            }
            total_bytes_read += bytes_read;
            let parse_res = match httparse::parse_chunk_size(&body_buf) {
                Ok(parse_res) => parse_res,
                Err(_) => break,
            };
            if parse_res.is_complete() {
                let (index, chunk_size) = parse_res.unwrap();
                if total_bytes_read > index + chunk_size as usize {
                    body_buf.rotate_right(index);
                    if let Err(_) = tx
                        .send(Event::Message {
                            msg: body_buf,
                            size: chunk_size as usize,
                        })
                        .await
                    {
                        break;
                    }
                    unread = cmp::max(0, total_bytes_read - index - chunk_size as usize);
                    if unread > 0 {
                        body_buf.rotate_left(index);
                    }
                } else {
                    let mut body = stream.take((content_length - body_read) as u64);
                    while total_bytes_read < index + chunk_size as usize {
                        let mut body_buf = [0u8; BUF_LEN];
                        select! {
                            res = body.read(&mut body_buf).fuse() => match res {
                                Ok(bytes_read) => {
                                    if bytes_read == 0 {
                                        // disconnected
                                        break;
                                    }
                                    body_read += bytes_read;
                                    if let Err(err) = tx
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
                }
            }
        }
    } else {
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
                        if let Err(err) = tx
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
        drop(tx);
        if body_read < content_length {
            println!("skipping remaining bytes???");
            futures::io::copy(body, &mut futures::io::sink()).await?;
        }
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
