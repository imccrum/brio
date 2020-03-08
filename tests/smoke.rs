#![feature(async_closure)]
//cargo test keeps_buffer -- --nocapture --test-threads=1

use brio::{App, Context, Request, Response, Status};
use futures::Future;
use httparse;
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

fn logger(ctx: Context) -> BoxFuture<Response> {
    println!("request recived: {}", ctx.req.path);
    ctx.next()
}

#[test]
fn get() -> std::result::Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let port: u32 = run_app();
    let uri = format!("127.0.0.1:{}", port);
    let mut req = connect(&uri)?;
    req.write_all(
        b"\
            POST /foo HTTP/1.1\r\n\
            Host: localhost:8000\r\n\
            Content-Type: application/json\r\n\
            Content-Length: 18\r\n\
            \r\n\
            {\"hello\": \"world\"}\r\n\
        ",
    )
    .unwrap();

    let res = call(&mut req)?;
    let json = serde_json::from_slice::<Value>(&res.bytes)?;
    assert_eq!(json, json!({"hello": "world"}));
    Ok(())
}

#[test]
fn discards_unread_body() -> std::result::Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let port: u32 = run_app();
    let uri = format!("127.0.0.1:{}", port);
    let mut req = connect(&uri)?;
    req.write_all(
        b"\
            POST /bar HTTP/1.1\r\n\
            Host: localhost:8000\r\n\
            Content-Type: application/json\r\n\
            Content-Length: 18\r\n\
            \r\n\
            {\"hello\": \"world\"}\r\n\
        ",
    )
    .unwrap();

    let res = call(&mut req)?;
    let json = serde_json::from_slice::<Value>(&res.bytes)?;
    assert_eq!(json, json!({"foo": "bar"}));
    Ok(())
}

#[test]
fn pipelinined() -> std::result::Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let port: u32 = run_app();
    let uri = format!("127.0.0.1:{}", port);
    let mut req = connect(&uri)?;
    req.write_all(
        b"\
            POST /bar HTTP/1.1\r\n\
            Host: localhost:8000\r\n\
            Content-Type: application/json\r\n\
            Content-Length: 18\r\n\
            \r\n\
            {\"hello\": \"world\"}\r\n\
            POST /foo HTTP/1.1\r\n\
            Host: localhost:8000\r\n\
            Content-Type: application/json\r\n\
            Content-Length: 18\r\n\
            \r\n\
            {\"hello\": \"world\"}\r\n\
        ",
    )
    .unwrap();

    let res = call(&mut req)?;
    let json = serde_json::from_slice::<Value>(&res.bytes)?;
    assert_eq!(json, json!({"foo": "bar"}));

    let res = call(&mut req)?;
    let json = serde_json::from_slice::<Value>(&res.bytes)?;
    assert_eq!(json, json!({"hello": "world"}));

    Ok(())
}

#[test]
fn preserves_partial() -> std::result::Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let port: u32 = run_app();
    let uri = format!("127.0.0.1:{}", port);
    let mut req = connect(&uri)?;
    req.write_all(
        b"\
            POST /bar HTTP/1.1\r\n\
            Host: localhost:8000\r\n\
            Content-Type: application/json\r\n\
            Content-Length: 18\r\n\
            \r\n\
            {\"hello\": \"world\"}\r\n\
            POST /foo HTTP/1.1\r\n\
            Host: localh",
    )
    .unwrap();

    let res = call(&mut req)?;
    let json = serde_json::from_slice::<Value>(&res.bytes)?;
    assert_eq!(json, json!({"foo": "bar"}));

    req.write_all(
        b"\
            ost:8000\r\n\
            Content-Type: application/json\r\n\
            Content-Length: 18\r\n\
            \r\n\
            {\"hello\": \"world\"}\r\n\
        ",
    )
    .unwrap();

    let res = call(&mut req)?;
    let json = serde_json::from_slice::<Value>(&res.bytes)?;
    assert_eq!(json, json!({"hello": "world"}));

    Ok(())
}

#[test]
fn chunked() -> std::result::Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let port: u32 = run_app();
    let uri = format!("127.0.0.1:{}", port);
    let mut req = connect(&uri)?;
    req.write_all(
        b"\
            POST /foo HTTP/1.1\r\n\
            Host: localhost:8000\r\n\
            Transfer-Encoding: chunked\r\n\
            \r\n\
            1\r\n\
            {\r\n\
            7\r\n\
            \"hello\"\r\n\
            1\r\n\
            :\r\n\
            9\r\n \"world\"}\r\n\
            0\r\n\
            \r\n\
        ",
    )
    .unwrap();

    let res = call(&mut req)?;
    let json = serde_json::from_slice::<Value>(&res.bytes)?;
    assert_eq!(json, json!({"hello": "world"}));
    Ok(())
}

fn call<'a>(req: &'a mut TcpStream) -> Result<Response, Box<dyn std::error::Error + Send + Sync>> {
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
            return Ok(Response::from_parts(
                parser,
                bytes,
                content_length,
                headers,
            )?);
        }
    }
}

async fn handler(mut req: Request) -> Response {
    let json = match req.json().await {
        Ok(json) => json,
        Err(_err) => {
            println!("err {}", _err);
            return Response::status(Status::BadRequest);
        }
    };
    let mut res = Response::status(Status::Ok);
    res.json(json);
    res
}

fn run_app() -> u32 {
    let mut rng = thread_rng();
    let port: u32 = rng.gen_range(10000, 20000);
    thread::spawn(move || {
        let mut app = App::new(());
        app.post("/foo", handler);
        app.post("/bar", async move |_req| {
            let mut res = Response::status(Status::Ok);
            res.json(json!({"foo": "bar"}));
            res
        });
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

fn connect(addr: &str) -> Result<std::net::TcpStream, Box<dyn std::error::Error + Send + Sync>> {
    let req = TcpStream::connect(addr)?;
    req.set_read_timeout(Some(Duration::from_secs(1)))?;
    req.set_write_timeout(Some(Duration::from_secs(1)))?;
    Ok(req)
}
