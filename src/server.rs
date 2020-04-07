use crate::request::{Chunk, Encoding};
use crate::router::{Ctx, Router};
use crate::{Request, Result};
use crate::{Sender, BUF_LEN, KEEP_ALIVE_TIMEOUT};
use async_std::{
    net::{TcpListener, TcpStream},
    prelude::*,
    task,
};
use futures::{channel::mpsc, join, select, sink::SinkExt, FutureExt};
use futures_timer::Delay;
use signal_hook::iterator::Signals;
use std::{cmp, collections::HashMap, future::Future, str, sync::Arc, thread, time::Duration};

pub(crate) async fn accept_loop<Routes: Send + Sync + Copy + Clone + 'static>(
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
    let mut buf_read_len = 0;
    loop {
        select! {
            conn = handle_request(&mut stream, &mut buf, buf_read_len, router.clone()).fuse() => {
                let (keep_alive, buf_next_len) = conn?;
                if !keep_alive {
                    println!("client did not request keep-alive, closing..");
                    break;
                }
                buf_read_len = buf_next_len;
            },
            _ = Delay::new(Duration::from_secs(KEEP_ALIVE_TIMEOUT)).fuse() => {
                println!("keep-alive timeout expired, closing..");
                break;
            }
        }
    }
    Ok(())
}

async fn handle_request<'a, Routes: Send + Sync + Copy + Clone + 'static>(
    stream: &mut TcpStream,
    buf: &'a mut [u8],
    buf_read_len: usize,
    router: Arc<Router<Routes>>,
) -> Result<(bool, usize)> {
    let writer = stream.clone();
    let mut reader = stream.clone();

    let (mut req, buf_read_len) = read_head(&mut reader, buf, buf_read_len).await?;

    let transfer_encoding = req.transfer_endcoding();
    let (body_tx, body_rx) = mpsc::channel(1);
    let trailers = req.check_trailers();
    let cl = req.content_len();
    if transfer_encoding == Encoding::Chunked || cl.unwrap_or(0) > 0 {
        req.stream(body_rx)
    }
    let response_handle = task::spawn(write_response(req, writer, router));
    let body_handle = read_body(
        &mut reader,
        buf,
        buf_read_len,
        cl,
        transfer_encoding,
        body_tx,
        trailers,
    );

    let (buf_read_len, keep_alive) = join!(body_handle, response_handle);
    Ok((keep_alive?, buf_read_len?))
}

async fn write_response<Routes: Send + Sync + Copy + Clone + 'static>(
    req: Request,
    mut writer: TcpStream,
    router: Arc<Router<Routes>>,
) -> Result<bool> {
    let keep_alive = req.is_keep_alive();
    let route = match router.routes.get(&req.route()) {
        Some(route) => route,
        None => &router.not_found,
    };
    let ctx = Ctx {
        req,
        route,
        next_middleware: router.middleware.as_slice(),
    };
    let mut res = ctx.next().await;
    if keep_alive {
        res.headers
            .insert("connection".to_owned(), "keep-alive".to_owned());
        res.headers.insert(
            "keep-alive".to_owned(),
            format!("timeout={}, max=1000", KEEP_ALIVE_TIMEOUT),
        );
    }
    if res.stream.is_some() {
        let stream = res.stream.take().unwrap();
        writer
            .write_all(res.head_as_bytes().as_slice())
            .await
            .unwrap();
        futures::io::copy(stream, &mut writer).await?;
    } else {
        writer.write_all(res.into_bytes().as_slice()).await.unwrap();
    }
    Ok(keep_alive)
}

async fn read_head<'a>(
    reader: &'a mut TcpStream,
    buf: &'a mut [u8],
    mut buf_read_len: usize,
) -> Result<(Request, usize)> {
    let mut total_head_len = 0;
    let mut extend_buf = vec![];
    let (req, buf_head_len, buf_read_len) = loop {
        if buf_read_len == 0 {
            buf_read_len = not_zero(reader.read(buf).await?)?;
        }
        total_head_len += buf_read_len;

        let mut headers = [httparse::EMPTY_HEADER; 16];
        let mut parser = httparse::Request::new(&mut headers);

        let parse_res = if extend_buf.is_empty() {
            parser.parse(&buf[..buf_read_len])?
        } else {
            extend_buf.extend_from_slice(&buf[..buf_read_len]);
            parser.parse(&extend_buf)?
        };
        if parse_res.is_partial() {
            if extend_buf.is_empty() {
                extend_buf.extend_from_slice(&buf[..buf_read_len]);
            }
        } else {
            let header_len = parse_res.unwrap();
            let buf_head_len: usize = header_len - (total_head_len - buf_read_len);
            let req = Request::from_parser(parser)?;
            break (req, buf_head_len, buf_read_len);
        }
        buf_read_len = 0;
    };
    rotate_buf(buf, buf_head_len);
    Ok((req, buf_read_len - buf_head_len))
}

async fn read_body<'a>(
    reader: &mut TcpStream,
    buf: &'a mut [u8],
    buf_read_len: usize,
    content_len: Option<usize>,
    transfer_encoding: Encoding,
    body_tx: Sender<Chunk>,
    trailers: Vec<String>,
) -> Result<usize> {
    if transfer_encoding == Encoding::Chunked {
        Ok(read_chunked(reader, buf, buf_read_len, trailers, body_tx).await?)
    } else {
        Ok(read_identity(reader, buf, buf_read_len, content_len, body_tx).await?)
    }
}

async fn read_identity<'a>(
    reader: &mut TcpStream,
    buf: &'a mut [u8],
    buf_read_len: usize,
    content_len: Option<usize>,
    mut body_tx: Sender<Chunk>,
) -> Result<usize> {
    let content_len = content_len.unwrap_or(0);
    // Check how much of the body was read when parsing the request head
    let mut buf_body_len: usize = cmp::min(content_len, buf_read_len);
    // Check how much of a subsequent pipelined request was read when parsing
    // the request head
    let buf_next_len = buf_read_len - buf_body_len;
    // Create a reader for unread body bytes
    let mut body = reader.take((content_len - buf_body_len) as u64);
    let mut total_body_len = 0;
    while total_body_len < content_len {
        if buf_body_len == 0 {
            // If no unparsed bytes in the current buffer read from the socket
            buf_body_len = select! {
                res = body.read(buf).fuse() => not_zero(res?)?
            };
        }
        if send_body_chunk(buf, &mut body_tx, buf_body_len)
            .await
            .is_err()
        {
            break;
        }
        total_body_len += buf_body_len;
        if total_body_len == content_len {
            // Entire body has been read
            break;
        }
        buf_body_len = 0;
    }
    // All bytes were sent or the receiver disconnected, drop the body sender
    drop(body_tx);
    if total_body_len < content_len {
        // Discard any unread bytes for the current request. This can happen if
        // the request handler did not read the body)
        futures::io::copy(body, &mut futures::io::sink()).await?;
    }
    // Rotate the buffer so that any bytes from a subsequent pipelined request
    // are at the beginning
    rotate_buf(buf, buf_body_len);
    Ok(buf_next_len)
}

async fn read_chunked<'a>(
    reader: &mut TcpStream,
    buf: &'a mut [u8],
    mut buf_read_len: usize,
    trailers: Vec<String>,
    mut body_tx: Sender<Chunk>,
) -> Result<usize> {
    let mut extend_buf = vec![];
    loop {
        // Continue parsing chunks until we get 0\r\n\r\n\
        let (index, chunk_len, skip_len) = loop {
            // Continue parsing until we get a chunk size
            if buf_read_len == 0 {
                // If no unparsed bytes in the current buffer read from the
                // socket
                buf_read_len = not_zero(reader.read(buf).await?)?;
            }
            let mut skip_len = 0;
            if extend_buf.is_empty() {
                // Check if we should skip the terminating \r\n\ from the
                // previous chunk. N.B. this only happens if the line feed
                // occured right at the end of the last buffer
                match &buf[..2] {
                    [b'\r', b'\n'] => {
                        skip_len = 2;
                    }
                    [b'\r', ..] | [b'\n', ..] => {
                        skip_len = 1;
                    }
                    _ => {}
                }
                // If found bytes from the last chunk subtract from
                // buf_read_len because they are not part of the current
                // chunk
                buf_read_len -= skip_len;
            }

            if buf_read_len > 0 {
                let parse_res = if extend_buf.is_empty() {
                    httparse::parse_chunk_size(&buf[skip_len..(buf_read_len + skip_len)])
                } else {
                    extend_buf.extend_from_slice(&buf[skip_len..(buf_read_len + skip_len)]);
                    httparse::parse_chunk_size(&extend_buf)
                };
                let parse_res = match parse_res {
                    Ok(parse_res) => parse_res,
                    Err(_) => {
                        return Err(Box::new(std::io::Error::new(
                            std::io::ErrorKind::ConnectionReset,
                            "invalid chunk",
                        )));
                    }
                };

                if parse_res.is_partial() {
                    if extend_buf.is_empty() {
                        extend_buf.extend_from_slice(&buf[skip_len..(buf_read_len + skip_len)]);
                    }
                } else {
                    let (index, chunk_len) = parse_res.unwrap();
                    break (index, chunk_len as usize, skip_len);
                }
            }
            buf_read_len = 0;
        };
        // Check what part of the currnent buf is the chunk index
        let offset = cmp::min(
            index,
            index - (cmp::max(extend_buf.len(), buf_read_len) - buf_read_len),
        );
        buf_read_len -= offset;
        // Rotate the buffer so that any bytes from the chunk body are at the
        // beginning
        rotate_buf(buf, offset + skip_len);

        if chunk_len == 0 {
            // This is the last chunk
            break;
        }
        // Track how many bytes from this chunk are in the buf
        let mut buf_chunk_len: usize = cmp::min(chunk_len, buf_read_len);
        let mut total_chunk_len = 0;
        loop {
            if buf_read_len == 0 {
                // If no unparsed bytes in the current buffer read from the socket
                buf_read_len += select! {
                    res = reader.read(&mut buf[buf_read_len..]).fuse() => not_zero(res?)?
                };
                // Make sure we only consider bytes from the current chunk
                buf_chunk_len = cmp::min(chunk_len - total_chunk_len, buf_read_len);
            }
            if send_body_chunk(buf, &mut body_tx, buf_chunk_len)
                .await
                .is_err()
            {
                println!("discarding unread bytes");
            }

            total_chunk_len += buf_chunk_len;
            buf_read_len -= buf_chunk_len;
            if total_chunk_len == chunk_len {
                // The entire chunk has been read
                // Rotate the buffer so that any bytes from the next chunk (or the next
                // request) are at the beginning
                rotate_buf(buf, buf_chunk_len);
                break;
            }
            buf_chunk_len = 0;
        }

        extend_buf = vec![];
    }

    if !trailers.is_empty() {
        let (trailers, trailer_buf_read_len) = read_trailers(reader, buf, buf_read_len).await?;
        buf_read_len = trailer_buf_read_len;
        if send_trailers(buf, &mut body_tx, trailers).await.is_err() {
            println!("discarding unread trailers");
        }
    }

    Ok(buf_read_len)
}

async fn read_trailers<'a>(
    reader: &'a mut TcpStream,
    buf: &'a mut [u8],
    mut buf_read_len: usize,
) -> Result<(HashMap<String, String>, usize)> {
    let mut total_trailer_read = 0;
    let mut extend_buf = vec![];
    let (trailers, buf_trailer_len, buf_read_len) = loop {
        if buf_read_len == 0 {
            // If no unparsed bytes in the current buffer read from the socket
            buf_read_len = not_zero(reader.read(buf).await?)?;
        }
        total_trailer_read += buf_read_len;

        let mut headers = [httparse::EMPTY_HEADER; 16];
        let parse_res = if extend_buf.is_empty() {
            httparse::parse_headers(&buf[..buf_read_len], &mut headers)?
        } else {
            extend_buf.extend_from_slice(&buf[..buf_read_len]);
            httparse::parse_headers(&extend_buf, &mut headers)?
        };
        if parse_res.is_partial() {
            if extend_buf.is_empty() {
                extend_buf.extend_from_slice(&buf[..buf_read_len]);
            }
        } else {
            let (header_len, parsed) = parse_res.unwrap();
            let buf_head_read: usize = header_len - (total_trailer_read - buf_read_len);
            let trailers: HashMap<String, String> = parsed
                .iter()
                .map(|&x| {
                    (
                        x.name.to_owned().to_lowercase(),
                        std::str::from_utf8(x.value).unwrap().to_owned(),
                    )
                })
                .collect();
            break (trailers, buf_head_read, buf_read_len);
        }
        buf_read_len = 0;
    };
    // Rotate the buffer so that any bytes from a subsequent pipelined request
    // are at the beginning
    rotate_buf(buf, buf_trailer_len);
    Ok((trailers, buf_read_len - buf_trailer_len))
}

async fn send_body_chunk<'a>(
    buf: &'a [u8],
    body_tx: &mut Sender<Chunk>,
    buf_body_len: usize,
) -> Result<()> {
    let mut tx_buf = [0u8; BUF_LEN];
    tx_buf.copy_from_slice(&buf);
    let msg = Chunk::Body {
        buf: tx_buf,
        size: buf_body_len,
    };
    Ok(body_tx.send(msg).await?)
}

async fn send_trailers<'a>(
    buf: &'a [u8],
    body_tx: &mut Sender<Chunk>,
    trailers: HashMap<String, String>,
) -> Result<()> {
    let mut tx_buf = [0u8; BUF_LEN];
    tx_buf.copy_from_slice(&buf);
    let msg = Chunk::Trailers { trailers };
    Ok(body_tx.send(msg).await?)
}

fn not_zero(len: usize) -> Result<usize> {
    if len == 0 {
        Err(Box::new(std::io::Error::new(
            std::io::ErrorKind::ConnectionReset,
            "disconnected",
        )))
    } else {
        Ok(len)
    }
}

fn rotate_buf(buf: &mut [u8], len: usize) {
    if len > 0 {
        for i in &mut buf[0..len] {
            *i = 0
        }
        buf.rotate_left(len);
    }
}

fn register_sigterm_listener() -> Result<futures::channel::oneshot::Receiver<bool>> {
    let signals = Signals::new(&[signal_hook::SIGTERM, signal_hook::SIGINT])?;
    let (sigterm_tx, sigterm_rx) = futures::channel::oneshot::channel::<bool>();
    thread::spawn(move || {
        for (_count, _signal) in signals.forever().enumerate() {
            break;
        }
        sigterm_tx.send(true).expect("shutdown notify failed");
    });
    Ok(sigterm_rx)
}
