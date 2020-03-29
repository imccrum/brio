#![feature(async_closure)]
#[cfg(test)]
use brio::{App, Body, Ctx, Encoding, Request, Response, Status};
use futures::Future;
use httparse;
use rand::{thread_rng, Rng};
use serde_json::{json, Value};
use std::panic;

mod util;

use util::*;

use std::{
    cmp,
    io::prelude::*,
    net::{Shutdown, TcpStream},
    pin::Pin,
    thread, time,
    time::Duration,
};
use time::Instant;

type BoxFuture<'a, Response> = Pin<Box<dyn Future<Output = Response> + Send + 'static>>;

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
    let json = serde_json::from_slice::<Value>(&res.body)?;
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
    let json = serde_json::from_slice::<Value>(&res.body)?;
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
    let json = serde_json::from_slice::<Value>(&res.body)?;
    assert_eq!(json, json!({"foo": "bar"}));

    let res = call(&mut req)?;
    let json = serde_json::from_slice::<Value>(&res.body)?;
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
    let json = serde_json::from_slice::<Value>(&res.body)?;
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
    let json = serde_json::from_slice::<Value>(&res.body)?;
    assert_eq!(json, json!({"hello": "world"}));

    Ok(())
}

#[test]
fn chunked_small() -> std::result::Result<(), Box<dyn std::error::Error + Send + Sync>> {
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
    let json = serde_json::from_slice::<Value>(&res.body)?;
    assert_eq!(json, json!({"hello": "world"}));
    Ok(())
}

#[test]
fn chunked_small_keep_alive() -> std::result::Result<(), Box<dyn std::error::Error + Send + Sync>> {
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
    let json = serde_json::from_slice::<Value>(&res.body)?;
    assert_eq!(json, json!({"hello": "world"}));

    let res = call(&mut req)?;
    let json = serde_json::from_slice::<Value>(&res.body)?;
    assert_eq!(json, json!({"hello": "world"}));
    Ok(())
}

#[test]
fn chunked_small_discards_unread(
) -> std::result::Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let port: u32 = run_app();
    let uri = format!("127.0.0.1:{}", port);
    let mut req = connect(&uri)?;
    req.write_all(
        b"\
            POST /baz HTTP/1.1\r\n\
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
    assert_eq!(res.status, Status::Ok);

    let res = call(&mut req)?;
    let json = serde_json::from_slice::<Value>(&res.body)?;
    assert_eq!(json, json!({"hello": "world"}));
    Ok(())
}

#[test]
fn chunked_large() -> std::result::Result<(), Box<dyn std::error::Error + Send + Sync>> {
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
        [\r\n\
        28d\r\n\
        {\"text\":\"Two households, both alike in dignity,\\nIn fair Verona, where we lay our scene,\\nFrom ancient grudge break to new mutiny,\\nWhere civil blood makes civil hands unclean.\\nFrom forth the fatal loins of these two foes\\nA pair of star-cross\'d lovers take their life;\\nWhose misadventured piteous overthrows\\nDo with their death bury their parents\' strife.\\nThe fearful passage of their death-mark\'d love,\\nAnd the continuance of their parents\' rage,\\nWhich, but their children\'s end, nought could remove,\\nIs now the two hours\' traffic of our stage;\\nThe which if you with patient ears attend,\\nWhat here shall miss, our toil shall strive to mend.\"}\r\n\
        2\r\n ,\r\n\
        28d\r\n\
        {\"text\":\"Two households, both alike in dignity,\\nIn fair Verona, where we lay our scene,\\nFrom ancient grudge break to new mutiny,\\nWhere civil blood makes civil hands unclean.\\nFrom forth the fatal loins of these two foes\\nA pair of star-cross\'d lovers take their life;\\nWhose misadventured piteous overthrows\\nDo with their death bury their parents\' strife.\\nThe fearful passage of their death-mark\'d love,\\nAnd the continuance of their parents\' rage,\\nWhich, but their children\'s end, nought could remove,\\nIs now the two hours\' traffic of our stage;\\nThe which if you with patient ears attend,\\nWhat here shall miss, our toil shall strive to mend.\"}\r\n\
        1\r\n\
        ]\r\n\
        0\r\n\
        \r\n\
        ",
    )
    .unwrap();

    let res = call(&mut req)?;
    let json = serde_json::from_slice::<Value>(&res.body)?;
    assert_eq!(json, Value::Array(vec![large_body(), large_body()]));
    Ok(())
}

#[test]
fn chunked_large_keep_alive() -> std::result::Result<(), Box<dyn std::error::Error + Send + Sync>> {
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
        [\r\n\
        28d\r\n\
        {\"text\":\"Two households, both alike in dignity,\\nIn fair Verona, where we lay our scene,\\nFrom ancient grudge break to new mutiny,\\nWhere civil blood makes civil hands unclean.\\nFrom forth the fatal loins of these two foes\\nA pair of star-cross\'d lovers take their life;\\nWhose misadventured piteous overthrows\\nDo with their death bury their parents\' strife.\\nThe fearful passage of their death-mark\'d love,\\nAnd the continuance of their parents\' rage,\\nWhich, but their children\'s end, nought could remove,\\nIs now the two hours\' traffic of our stage;\\nThe which if you with patient ears attend,\\nWhat here shall miss, our toil shall strive to mend.\"}\r\n\
        2\r\n ,\r\n\
        28d\r\n\
        {\"text\":\"Two households, both alike in dignity,\\nIn fair Verona, where we lay our scene,\\nFrom ancient grudge break to new mutiny,\\nWhere civil blood makes civil hands unclean.\\nFrom forth the fatal loins of these two foes\\nA pair of star-cross\'d lovers take their life;\\nWhose misadventured piteous overthrows\\nDo with their death bury their parents\' strife.\\nThe fearful passage of their death-mark\'d love,\\nAnd the continuance of their parents\' rage,\\nWhich, but their children\'s end, nought could remove,\\nIs now the two hours\' traffic of our stage;\\nThe which if you with patient ears attend,\\nWhat here shall miss, our toil shall strive to mend.\"}\r\n\
        1\r\n\
        ]\r\n\
        0\r\n\
        \r\n\
        POST /foo HTTP/1.1\r\n\
        Host: localhost:8000\r\n\
        Transfer-Encoding: chunked\r\n\
        \r\n\
        1\r\n\
        [\r\n\
        28d\r\n\
        {\"text\":\"Two households, both alike in dignity,\\nIn fair Verona, where we lay our scene,\\nFrom ancient grudge break to new mutiny,\\nWhere civil blood makes civil hands unclean.\\nFrom forth the fatal loins of these two foes\\nA pair of star-cross\'d lovers take their life;\\nWhose misadventured piteous overthrows\\nDo with their death bury their parents\' strife.\\nThe fearful passage of their death-mark\'d love,\\nAnd the continuance of their parents\' rage,\\nWhich, but their children\'s end, nought could remove,\\nIs now the two hours\' traffic of our stage;\\nThe which if you with patient ears attend,\\nWhat here shall miss, our toil shall strive to mend.\"}\r\n\
        2\r\n ,\r\n\
        28d\r\n\
        {\"text\":\"Two households, both alike in dignity,\\nIn fair Verona, where we lay our scene,\\nFrom ancient grudge break to new mutiny,\\nWhere civil blood makes civil hands unclean.\\nFrom forth the fatal loins of these two foes\\nA pair of star-cross\'d lovers take their life;\\nWhose misadventured piteous overthrows\\nDo with their death bury their parents\' strife.\\nThe fearful passage of their death-mark\'d love,\\nAnd the continuance of their parents\' rage,\\nWhich, but their children\'s end, nought could remove,\\nIs now the two hours\' traffic of our stage;\\nThe which if you with patient ears attend,\\nWhat here shall miss, our toil shall strive to mend.\"}\r\n\
        1\r\n\
        ]\r\n\
        0\r\n\
        \r\n\
        ",
    )
    .unwrap();

    let res = call(&mut req)?;
    let json = serde_json::from_slice::<Value>(&res.body)?;
    assert_eq!(json, Value::Array(vec![large_body(), large_body()]));

    let res = call(&mut req)?;
    let json = serde_json::from_slice::<Value>(&res.body)?;
    assert_eq!(json, Value::Array(vec![large_body(), large_body()]));
    Ok(())
}

#[test]
fn chunked_large_discards_unread(
) -> std::result::Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let port: u32 = run_app();
    let uri = format!("127.0.0.1:{}", port);
    let mut req = connect(&uri)?;
    req.write_all(
        b"\
        POST /baz HTTP/1.1\r\n\
        Host: localhost:8000\r\n\
        Transfer-Encoding: chunked\r\n\
        \r\n\
        1\r\n\
        [\r\n\
        28d\r\n\
        {\"text\":\"Two households, both alike in dignity,\\nIn fair Verona, where we lay our scene,\\nFrom ancient grudge break to new mutiny,\\nWhere civil blood makes civil hands unclean.\\nFrom forth the fatal loins of these two foes\\nA pair of star-cross\'d lovers take their life;\\nWhose misadventured piteous overthrows\\nDo with their death bury their parents\' strife.\\nThe fearful passage of their death-mark\'d love,\\nAnd the continuance of their parents\' rage,\\nWhich, but their children\'s end, nought could remove,\\nIs now the two hours\' traffic of our stage;\\nThe which if you with patient ears attend,\\nWhat here shall miss, our toil shall strive to mend.\"}\r\n\
        2\r\n ,\r\n\
        28d\r\n\
        {\"text\":\"Two households, both alike in dignity,\\nIn fair Verona, where we lay our scene,\\nFrom ancient grudge break to new mutiny,\\nWhere civil blood makes civil hands unclean.\\nFrom forth the fatal loins of these two foes\\nA pair of star-cross\'d lovers take their life;\\nWhose misadventured piteous overthrows\\nDo with their death bury their parents\' strife.\\nThe fearful passage of their death-mark\'d love,\\nAnd the continuance of their parents\' rage,\\nWhich, but their children\'s end, nought could remove,\\nIs now the two hours\' traffic of our stage;\\nThe which if you with patient ears attend,\\nWhat here shall miss, our toil shall strive to mend.\"}\r\n\
        1\r\n\
        ]\r\n\
        0\r\n\
        \r\n\
        POST /foo HTTP/1.1\r\n\
        Host: localhost:8000\r\n\
        Transfer-Encoding: chunked\r\n\
        \r\n\
        1\r\n\
        [\r\n\
        28d\r\n\
        {\"text\":\"Two households, both alike in dignity,\\nIn fair Verona, where we lay our scene,\\nFrom ancient grudge break to new mutiny,\\nWhere civil blood makes civil hands unclean.\\nFrom forth the fatal loins of these two foes\\nA pair of star-cross\'d lovers take their life;\\nWhose misadventured piteous overthrows\\nDo with their death bury their parents\' strife.\\nThe fearful passage of their death-mark\'d love,\\nAnd the continuance of their parents\' rage,\\nWhich, but their children\'s end, nought could remove,\\nIs now the two hours\' traffic of our stage;\\nThe which if you with patient ears attend,\\nWhat here shall miss, our toil shall strive to mend.\"}\r\n\
        2\r\n ,\r\n\
        28d\r\n\
        {\"text\":\"Two households, both alike in dignity,\\nIn fair Verona, where we lay our scene,\\nFrom ancient grudge break to new mutiny,\\nWhere civil blood makes civil hands unclean.\\nFrom forth the fatal loins of these two foes\\nA pair of star-cross\'d lovers take their life;\\nWhose misadventured piteous overthrows\\nDo with their death bury their parents\' strife.\\nThe fearful passage of their death-mark\'d love,\\nAnd the continuance of their parents\' rage,\\nWhich, but their children\'s end, nought could remove,\\nIs now the two hours\' traffic of our stage;\\nThe which if you with patient ears attend,\\nWhat here shall miss, our toil shall strive to mend.\"}\r\n\
        1\r\n\
        ]\r\n\
        0\r\n\
        \r\n\
        ",
    )
    .unwrap();

    let res = call(&mut req)?;
    assert_eq!(res.status, Status::Ok);

    let res = call(&mut req)?;
    let json = serde_json::from_slice::<Value>(&res.body)?;
    assert_eq!(json, Value::Array(vec![large_body(), large_body()]));
    Ok(())
}

#[test]
fn trailers() -> std::result::Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let port: u32 = run_app();
    let uri = format!("127.0.0.1:{}", port);
    let mut req = connect(&uri)?;
    req.write_all(
        b"\
            POST /trailers HTTP/1.1\r\n\
            Host: localhost:8000\r\n\
            Transfer-Encoding: chunked\r\n\
            Trailer: Expires\r\n\
            \r\n\
            1\r\n\
            {\r\n\
            7\r\n\
            \"hello\"\r\n\
            1\r\n\
            :\r\n\
            9\r\n \"world\"}\r\n\
            0\r\n\
            Expires: Fri, 01 Nov 2019 07:28:00 GMT\r\n\
            \r\n\
            ",
    )
    .unwrap();

    let res = call(&mut req)?;
    assert_eq!(res.status, Status::Ok);
    assert_eq!(
        res.headers.get("expires").unwrap(),
        "Fri, 01 Nov 2019 07:28:00 GMT",
    );
    Ok(())
}

#[test]
fn missing_trailers() -> std::result::Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let port: u32 = run_app();
    let uri = format!("127.0.0.1:{}", port);
    let mut req = connect(&uri)?;
    req.write_all(
        b"\
            POST /foo HTTP/1.1\r\n\
            Host: localhost:8000\r\n\
            Transfer-Encoding: chunked\r\n\
            Trailer: Expires\r\n\
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
    assert_eq!(res.status, Status::Ok);
    assert_eq!(res.headers.get("expires"), None);
    Ok(())
}

#[test]
fn stream() -> std::result::Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let port: u32 = run_app();
    let uri = format!("127.0.0.1:{}", port);
    let mut req = connect(&uri)?;
    req.write_all(
        b"\
            POST /stream HTTP/1.1\r\n\
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
    assert_eq!(res.status, Status::Ok);
    let json = serde_json::from_slice::<Value>(&res.body)?;
    assert_eq!(json, json!({"hello": "world"}));
    Ok(())
}

fn call<'a>(req: &'a mut TcpStream) -> Result<Response, Box<dyn std::error::Error + Send + Sync>> {
    pub const BUF_LEN: usize = 256;
    let mut total_bytes_read = 0;
    loop {
        let mut buf = [0u8; BUF_LEN];
        let mut buf_read_len = req.read(&mut buf)?;
        if buf_read_len == 0 {
            return Err(Box::new(std::io::Error::new(
                std::io::ErrorKind::ConnectionReset,
                "client disconnected",
            )));
        }
        let mut headers = [httparse::EMPTY_HEADER; 16];
        let mut parser = httparse::Response::new(&mut headers);
        let parse_res = parser.parse(&buf)?;
        if parse_res.is_partial() {
            total_bytes_read += buf_read_len;
        } else {
            let header_len = parse_res.unwrap();
            let mut res = Response::from_parser(parser)?;
            let buf_header_len: usize = header_len - total_bytes_read;
            rotate_buf(&mut buf, buf_header_len);
            buf_read_len = buf_read_len - buf_header_len;
            if res.transfer_endcoding() == Encoding::Chunked {
                let body = read_chunked(req, &mut buf, buf_read_len)?;
                res.body = body;
            } else {
                let content_len = res.content_len().unwrap();
                let mut body = vec![];
                body.extend_from_slice(&buf[..cmp::min(content_len, buf_read_len)]);
                let mut take = req.take((content_len - body.len()) as u64);
                take.read_to_end(&mut body)?;
                res.body = body;
            }
            return Ok(res);
        }
    }
}

fn read_chunked<'a>(
    reader: &mut TcpStream,
    buf: &'a mut [u8],
    mut buf_read_len: usize,
) -> Result<Vec<u8>, Box<dyn std::error::Error + Send + Sync>> {
    let mut body = vec![];
    let mut chunk_size = vec![];
    loop {
        let (index, chunk_len, skip_len) = loop {
            if buf_read_len == 0 {
                buf_read_len = not_zero(reader.read(buf)?)?;
            }
            let mut skip_len = 0;
            if chunk_size.len() == 0 {
                match &buf[..2] {
                    [b'\r', b'\n'] => {
                        skip_len = 2;
                    }
                    [b'\r', ..] => {
                        skip_len = 1;
                    }
                    [b'\n', ..] => {
                        skip_len = 1;
                    }
                    _ => {}
                }
            }
            buf_read_len -= skip_len;
            if buf_read_len > 0 {
                let parse_res = if chunk_size.len() == 0 {
                    httparse::parse_chunk_size(&buf[skip_len..(buf_read_len + skip_len)])
                } else {
                    chunk_size.extend_from_slice(&buf[skip_len..(buf_read_len + skip_len)]);
                    httparse::parse_chunk_size(&chunk_size)
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
                    if chunk_size.len() == 0 {
                        chunk_size.extend_from_slice(&buf[skip_len..(buf_read_len + skip_len)]);
                    }
                } else {
                    //buf_read_len;
                    let (index, size) = parse_res.unwrap();
                    break (index, size, skip_len);
                }
            }
            buf_read_len = 0;
        };

        let chunk_len = chunk_len as usize;
        let offset = cmp::min(
            index,
            index - (cmp::max(chunk_size.len(), buf_read_len) - buf_read_len),
        );

        buf_read_len -= offset;
        rotate_buf(buf, offset + skip_len);
        if chunk_len == 0 {
            break;
        }

        let mut buf_chunk_len: usize = cmp::min(chunk_len, buf_read_len);
        let mut total_chunk_len = 0;
        loop {
            // here we need to check that we actually have enough of the body
            if buf_read_len == 0 {
                // if no unparsed bytes in the current buffer read from the socket
                buf_read_len += not_zero(reader.read(&mut buf[buf_read_len..])?)?;
                buf_chunk_len = cmp::min(chunk_len - total_chunk_len, buf_read_len);
            }
            body.extend_from_slice(&buf[..buf_chunk_len]);
            total_chunk_len += buf_chunk_len;
            buf_read_len -= buf_chunk_len;
            if total_chunk_len == chunk_len {
                rotate_buf(buf, buf_chunk_len);
                // entire chunk has been read
                break;
            }
            buf_chunk_len = 0;
        }

        chunk_size = vec![];
    }
    Ok(body)
}

fn not_zero(len: usize) -> Result<usize, Box<dyn std::error::Error + Send + Sync>> {
    if len == 0 {
        return Err(Box::new(std::io::Error::new(
            std::io::ErrorKind::ConnectionReset,
            "disconnected",
        )));
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

fn run_app() -> u32 {
    let mut rng = thread_rng();
    let port: u32 = rng.gen_range(10000, 20000);
    thread::spawn(move || {
        async fn stream(mut req: Request) -> Response {
            let mut res = Response::status(Status::Ok);
            res.set_body(Body::new(req.take_body().unwrap().unwrap()));
            res
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

        fn logger(mut ctx: Ctx) -> BoxFuture<Response> {
            let now = Instant::now();
            let path = ctx.req.path.clone();
            ctx.req.headers.insert("foo".to_owned(), "bar".to_owned());
            let method = ctx.req.method.clone();
            let fut = ctx.next();
            Box::pin(async move {
                let res = fut.await;
                println!(
                    "request {} {} took {:?} ({})",
                    method,
                    path,
                    Instant::now().duration_since(now),
                    res.status as u32
                );
                res
            })
        }

        let mut app = App::new(());
        app.post("/foo", handler);
        app.post("/stream", stream);
        app.post("/bar", async move |_req| {
            let mut res = Response::status(Status::Ok);
            res.json(json!({"foo": "bar"}));
            res
        });
        app.post("/baz", async move |_req| Response::status(Status::Ok));
        app.post("/trailers", async move |mut req: Request| {
            let mut res = Response::status(Status::Ok);
            match req.trailers().await {
                Some(trailers) => {
                    res.headers.insert(
                        "expires".to_owned(),
                        trailers.get("expires").unwrap().to_owned(),
                    );
                }
                None => {
                    return Response::status(Status::BadRequest);
                }
            }
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
