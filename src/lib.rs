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
                    let stream = stream?;
                    //println!("accepting from: {}", stream.peer_addr()?);
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
    let mut buf = [0u8; BUF_LEN];
    let mut next_read = 0;
    loop {
        select! {
            next = request_loop(&mut stream, &mut buf, next_read, router.clone()).fuse() => {
                let (keep_alive, next) = next?;
                if !keep_alive {
                    println!("client did not request keep-alive, closing..");
                    break;
                }
                next_read = next;
            },
            _ = Delay::new(Duration::from_secs(KEEP_ALIVE_TIMEOUT)).fuse() => {
                println!("keep-alive timeout expired, closing..");
            }
        }
    }
    Ok(())
}

async fn request_loop<'a, Routes: Send + Sync + Copy + Clone + 'static>(
    stream: &mut TcpStream,
    buf: &'a mut [u8],
    buf_next_read: usize,
    router: Arc<Router<Routes>>,
) -> Result<(bool, usize)> {
    let writer = stream.clone();
    let mut reader = stream.clone();

    let (mut req, buf_head_read, buf_read) = parse_head(&mut reader, buf, buf_next_read).await?;

    let content_len = req.content_len as usize;
    let transfer_encoding = req.transfer_endcoding();
    let (body_tx, body_rx) = mpsc::channel(1);
    if transfer_encoding == Encoding::Chunked || req.content_len > 0 {
        req.set_body(body_rx)
    }

    let request_handle = task::spawn(handle_request(req, writer, router));
    let body_handle = body_loop(
        &mut reader,
        buf,
        buf_read,
        buf_head_read,
        content_len,
        transfer_encoding,
        body_tx,
    );
    let (buf_next_read, keep_alive) = join!(body_handle, request_handle);
    Ok((keep_alive?, buf_next_read?))
}

async fn parse_head<'a>(
    reader: &'a mut TcpStream,
    buf: &'a mut [u8],
    mut buf_read: usize,
) -> Result<(Request, usize, usize)> {
    let mut total_head_read = 0;
    let mut head = vec![];
    let (req, buf_head_read, buf_read) = loop {
        if buf_read == 0 {
            buf_read = not_zero(reader.read(buf).await?)?;
        }
        total_head_read += buf_read;

        let mut headers = [httparse::EMPTY_HEADER; 16];
        let mut parser = httparse::Request::new(&mut headers);

        let parse_res = if head.len() == 0 {
            parser.parse(&buf[..buf_read])?
        } else {
            head.extend_from_slice(&buf[..buf_read]);
            parser.parse(&head)?
        };
        if parse_res.is_partial() && head.len() == 0 {
            head.extend_from_slice(&buf[..buf_read]);
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
            let content_len: usize = match headers.get("content-length") {
                Some(cl) => cl.parse()?,
                None => 0,
            };

            let buf_head_read: usize = header_len - (total_head_read - buf_read);
            let req = Request::from_parts(parser, content_len as u64, headers)?;
            break (req, buf_head_read, buf_read);
        }
        buf_read = 0;
    };
    buf.rotate_left(buf_head_read);
    Ok((req, buf_head_read, buf_read))
}

async fn body_loop<'a>(
    reader: &mut TcpStream,
    buf: &'a mut [u8],
    buf_read: usize,
    buf_head_read: usize,
    content_len: usize,
    transfer_encoding: Encoding,
    mut body_tx: Sender<Event>,
) -> Result<usize> {
    let mut total_body_read = 0;
    if transfer_encoding == Encoding::Chunked {
        let mut buf_read = buf_read - buf_head_read;
        let buf_next_chunk_read;
        loop {
            if buf_read == 0 {
                buf_read = not_zero(reader.read(buf).await?)?;
            }
            let parse_res = match httparse::parse_chunk_size(&buf[..buf_read]) {
                Ok(parse_res) => parse_res,
                Err(_) => {
                    return Err(Box::new(std::io::Error::new(
                        std::io::ErrorKind::ConnectionReset,
                        "invalid chunk",
                    )));
                }
            };
            if parse_res.is_complete() {
                let (mut index, chunk_len) = parse_res.unwrap();
                let chunk_len = chunk_len as usize;
                if chunk_len == 0 {
                    // end of body
                    buf_next_chunk_read = buf_read - index - 2;
                    buf.rotate_left(index + 2);
                    break;
                }
                buf.rotate_left(index);
                let mut buf_chunk_read: usize = cmp::min(chunk_len, buf_read - index);
                let mut total_chunk_read = 0;
                loop {
                    if buf_chunk_read == 0 {
                        // if no unparsed bytes in the current buffer read from the socket
                        buf_read = select! {
                            res = reader.read(buf).fuse() => not_zero(res?)?
                        };
                        buf_chunk_read = cmp::min(chunk_len - total_chunk_read, buf_read);
                    }
                    if let Err(_) = send_body_chunk(buf, &mut body_tx, buf_chunk_read).await {
                        println!("discarding unread bytes");
                        // do nothing
                    }
                    total_chunk_read += buf_chunk_read;
                    buf_read -= buf_chunk_read + index;
                    if total_chunk_read == chunk_len {
                        // entire body has been read
                        buf_read -= 2;
                        buf.rotate_left(buf_chunk_read + 2);
                        break;
                    }
                    index = 0;
                    buf_chunk_read = 0;
                }
            }
        }
        Ok(buf_next_chunk_read)
    } else {
        // identity encoding
        // check how much of the body was read when reading parsing the request head
        let mut buf_body_read: usize = cmp::min(content_len, buf_read - buf_head_read);
        // check how much of a later pipelined request was read when parsing the request head
        let buf_next_read = buf_read - buf_head_read - buf_body_read;
        // create a reader for unread body bytes
        let mut body = reader.take((content_len - buf_body_read) as u64);
        loop {
            if buf_body_read == 0 {
                // if no unparsed bytes in the current buffer read from the socket
                buf_body_read = select! {
                    res = body.read(buf).fuse() => not_zero(res?)?
                };
            }
            if let Err(_) = send_body_chunk(buf, &mut body_tx, buf_body_read).await {
                break;
            }
            total_body_read += buf_body_read;
            if total_body_read == content_len {
                // entire body has been read
                break;
            }
            buf_body_read = 0;
        }
        // all bytes were sent or an error occured, drop the body channel
        drop(body_tx);
        if total_body_read < content_len {
            // if there are remaining unread
            futures::io::copy(body, &mut futures::io::sink()).await?;
        }
        if buf_next_read > 0 {
            buf.rotate_left(buf_body_read);
        }
        Ok(buf_next_read)
    }
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

async fn send_body_chunk<'a>(
    buf: &'a [u8],
    body_tx: &mut Sender<Event>,
    buf_body_read: usize,
) -> Result<()> {
    let mut tx_buf = [0u8; BUF_LEN];
    tx_buf.copy_from_slice(&buf);
    let msg = Event::Message {
        msg: tx_buf,
        size: buf_body_read,
    };
    Ok(body_tx.send(msg).await?)
}

fn not_zero(bytes_read: usize) -> Result<usize> {
    if bytes_read == 0 {
        return Err(Box::new(std::io::Error::new(
            std::io::ErrorKind::ConnectionReset,
            "disconnected",
        )));
    } else {
        Ok(bytes_read)
    }
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
