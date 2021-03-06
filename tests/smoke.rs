#![feature(async_closure)]
#[cfg(test)]
use brio::{App, ChunkedBody, Ctx, Encoding, Request, Response, Status};
use futures::Future;
use httparse;
use log::info;
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
    let json = serde_json::from_slice::<Value>(&res.bytes())?;
    assert_eq!(json, json!({"hello": "world"}));
    assert_eq!(
        res.headers().get("content-type").unwrap(),
        "application/json"
    );
    Ok(())
}

#[test]
fn math() -> std::result::Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let port: u32 = run_app();
    let uri = format!("127.0.0.1:{}", port);
    let mut req = connect(&uri)?;
    // nothing
    req.write_all(
        b"\
            GET /echo HTTP/1.1\r\n\
            Host: localhost:8000\r\n\
            Content-Type: application/json\r\n\
            Math: 1\r\n\
            Content-Length: 0\r\n\
            \r\n\
        ",
    )
    .unwrap();
    let res = call(&mut req)?;
    assert_eq!(
        res.headers().get(&"math".to_owned()).unwrap().to_owned(),
        "1".to_owned()
    );
    assert_eq!(res.headers().get("content-type"), None);

    // add
    req.write_all(
        b"\
            GET /add/echo HTTP/1.1\r\n\
            Host: localhost:8000\r\n\
            Content-Type: application/json\r\n\
            Math: 1\r\n\
            Content-Length: 0\r\n\
            \r\n\
        ",
    )
    .unwrap();
    let res = call(&mut req)?;
    assert_eq!(
        res.headers().get(&"math".to_owned()).unwrap().to_owned(),
        "5".to_owned()
    );
    assert_eq!(res.headers().get("content-type"), None);

    // mutiply
    req.write_all(
        b"\
                GET /multiply/echo HTTP/1.1\r\n\
                Host: localhost:8000\r\n\
                Content-Type: application/json\r\n\
                Math: 1\r\n\
                Content-Length: 0\r\n\
                \r\n\
            ",
    )
    .unwrap();
    let res = call(&mut req)?;
    assert_eq!(
        res.headers().get(&"math".to_owned()).unwrap().to_owned(),
        "4".to_owned()
    );
    assert_eq!(res.headers().get("content-type"), None);

    // add, multiply
    req.write_all(
        b"\
            GET /math/echo HTTP/1.1\r\n\
            Host: localhost:8000\r\n\
            Content-Type: application/json\r\n\
            Math: 1\r\n\
            Content-Length: 0\r\n\
            \r\n\
        ",
    )
    .unwrap();
    let res = call(&mut req)?;
    assert_eq!(
        res.headers().get(&"math".to_owned()).unwrap().to_owned(),
        "20".to_owned()
    );
    assert_eq!(res.headers().get("content-type"), None);

    // add, multiply, exponentiate
    req.write_all(
        b"\
                POST /math/echo HTTP/1.1\r\n\
                Host: localhost:8000\r\n\
                Content-Type: application/json\r\n\
                Math: 1\r\n\
                Content-Length: 0\r\n\
                \r\n\
            ",
    )
    .unwrap();
    let res = call(&mut req)?;
    assert_eq!(
        res.headers().get(&"math".to_owned()).unwrap().to_owned(),
        "160000".to_owned()
    );
    assert_eq!(res.headers().get("content-type"), None);

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
    let json = serde_json::from_slice::<Value>(res.bytes())?;
    assert_eq!(json, json!({"foo": "bar"}));
    assert_eq!(
        res.headers().get("content-type").unwrap(),
        "application/json"
    );

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
    let json = serde_json::from_slice::<Value>(res.bytes())?;
    assert_eq!(json, json!({"foo": "bar"}));
    assert_eq!(
        res.headers().get("content-type").unwrap(),
        "application/json"
    );

    let res = call(&mut req)?;
    let json = serde_json::from_slice::<Value>(res.bytes())?;
    assert_eq!(json, json!({"hello": "world"}));
    assert_eq!(
        res.headers().get("content-type").unwrap(),
        "application/json"
    );

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
    let json = serde_json::from_slice::<Value>(res.bytes())?;
    assert_eq!(json, json!({"foo": "bar"}));
    assert_eq!(
        res.headers().get("content-type").unwrap(),
        "application/json"
    );

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
    let json = serde_json::from_slice::<Value>(res.bytes())?;
    assert_eq!(json, json!({"hello": "world"}));
    assert_eq!(
        res.headers().get("content-type").unwrap(),
        "application/json"
    );

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
            Content-Type: application/json\r\n\
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
    let json = serde_json::from_slice::<Value>(res.bytes())?;
    assert_eq!(json, json!({"hello": "world"}));
    assert_eq!(
        res.headers().get("content-type").unwrap(),
        "application/json"
    );

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
            Content-Type: application/json\r\n\
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
            Content-Type: application/json\r\n\
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
    let json = serde_json::from_slice::<Value>(res.bytes())?;
    assert_eq!(json, json!({"hello": "world"}));
    assert_eq!(
        res.headers().get("content-type").unwrap(),
        "application/json"
    );

    let res = call(&mut req)?;
    let json = serde_json::from_slice::<Value>(res.bytes())?;
    assert_eq!(json, json!({"hello": "world"}));
    assert_eq!(
        res.headers().get("content-type").unwrap(),
        "application/json"
    );

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
            Content-Type: application/json\r\n\
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
            Content-Type: application/json\r\n\
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
    assert_eq!(res.status(), Status::Ok);
    assert_eq!(res.headers().get("content-type"), None);

    let res = call(&mut req)?;
    let json = serde_json::from_slice::<Value>(&res.bytes())?;
    assert_eq!(json, json!({"hello": "world"}));
    assert_eq!(
        res.headers().get("content-type").unwrap(),
        "application/json"
    );

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
        Content-Type: application/json\r\n\
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
    let json = serde_json::from_slice::<Value>(&res.bytes())?;
    assert_eq!(json, Value::Array(vec![large_body(), large_body()]));
    assert_eq!(
        res.headers().get("content-type").unwrap(),
        "application/json"
    );

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
        Content-Type: application/json\r\n\
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
        Content-Type: application/json\r\n\
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
    let json = serde_json::from_slice::<Value>(res.bytes())?;
    assert_eq!(json, Value::Array(vec![large_body(), large_body()]));
    assert_eq!(
        res.headers().get("content-type").unwrap(),
        "application/json"
    );

    let res = call(&mut req)?;
    let json = serde_json::from_slice::<Value>(&res.bytes())?;
    assert_eq!(json, Value::Array(vec![large_body(), large_body()]));
    assert_eq!(
        res.headers().get("content-type").unwrap(),
        "application/json"
    );

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
        Content-Type: application/json\r\n\
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
        Content-Type: application/json\r\n\
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
    assert_eq!(res.status(), Status::Ok);
    assert_eq!(res.headers().get("content-type"), None);

    let res = call(&mut req)?;
    let json = serde_json::from_slice::<Value>(res.bytes())?;
    assert_eq!(json, Value::Array(vec![large_body(), large_body()]));
    assert_eq!(
        res.headers().get("content-type").unwrap(),
        "application/json"
    );

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
            Content-Type: application/json\r\n\
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
    assert_eq!(res.status(), Status::Ok);
    assert_eq!(
        res.headers().get("expires").unwrap(),
        "Fri, 01 Nov 2019 07:28:00 GMT",
    );
    assert_eq!(res.headers().get("content-type"), None);

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
            Content-Type: application/json\r\n\
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
    assert_eq!(res.status(), Status::Ok);
    assert_eq!(res.headers().get("expires"), None);
    assert_eq!(
        res.headers().get("content-type").unwrap(),
        "application/json"
    );

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
            Content-Type: application/json\r\n\
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
    assert_eq!(res.status(), Status::Ok);
    let json = serde_json::from_slice::<Value>(res.bytes())?;
    assert_eq!(json, json!({"hello": "world"}));
    assert_eq!(
        res.headers().get("content-type").unwrap(),
        "application/json"
    );

    Ok(())
}

#[test]
fn file() -> std::result::Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let port: u32 = run_app();
    let uri = format!("127.0.0.1:{}", port);
    let mut req = connect(&uri)?;
    req.write_all(
        b"\
            GET /public/index.html HTTP/1.1\r\n\
            Host: localhost:8000\r\n\
            Content-Length: 0\r\n\
            \r\n\
            ",
    )
    .unwrap();

    let res = call(&mut req)?;
    assert_eq!(res.status(), Status::Ok);
    let html = std::str::from_utf8(res.bytes().as_slice())
        .unwrap()
        .to_owned();
    assert_eq!(html.contains("<h1>hello world</h1>"), true);
    assert_eq!(res.headers().get("content-type").unwrap(), "text/html");

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
                res.set_buf(body);
            } else {
                let content_len = res.content_len().unwrap();
                let mut body = vec![];
                body.extend_from_slice(&buf[..cmp::min(content_len, buf_read_len)]);
                let mut take = req.take((content_len - body.len()) as u64);
                take.read_to_end(&mut body)?;
                res.set_buf(body);
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
    std::env::set_var("RUST_LOG", "brio=info,smoke=debug");
    let _ = env_logger::builder().is_test(true).try_init();

    let mut rng = thread_rng();
    let port: u32 = rng.gen_range(10000, 20000);
    thread::spawn(move || {
        async fn stream(mut req: Request) -> Response {
            let mut res = Response::with_status(Status::Ok);
            res.set_stream(
                ChunkedBody::new(req.take_stream().unwrap()),
                req.content_type(),
            );
            res
        }
        async fn math(mut req: Request) -> Response {
            let mut res = Response::with_status(Status::Ok);
            if let Some(add) = req.headers().get("math") {
                res.headers_mut().insert("math".to_owned(), add.to_owned());
            }
            res
        }
        async fn handler(mut req: Request) -> Response {
            let json = match req.json().await {
                Ok(json) => json,
                Err(_err) => {
                    println!("err {}", _err);
                    return Response::with_status(Status::BadRequest);
                }
            };
            let mut res = Response::with_status(Status::Ok);
            res.set_json(json);
            res
        }

        fn logger(mut ctx: Ctx) -> BoxFuture<Response> {
            let now = Instant::now();
            let path = ctx.req().route().path().clone();
            ctx.req_mut()
                .headers_mut()
                .insert("foo".to_owned(), "bar".to_owned());
            let method = ctx.req().route().method().unwrap().clone();
            let fut = ctx.next();
            Box::pin(async move {
                let res = fut.await;
                info!(
                    "request {} {} took {:?} ({})",
                    method,
                    path,
                    Instant::now().duration_since(now),
                    res.status() as u32
                );
                res
            })
        }

        fn add(mut ctx: Ctx) -> BoxFuture<Response> {
            if let Some(add) = ctx.req_mut().headers().get("math") {
                if let Ok(mut val) = add.parse::<u32>() {
                    val += 4;
                    ctx.req_mut()
                        .headers_mut()
                        .insert("math".to_owned(), val.to_string());
                }
            }
            ctx.next()
        }

        fn multiply(mut ctx: Ctx) -> BoxFuture<Response> {
            if let Some(add) = ctx.req_mut().headers().get("math") {
                if let Ok(mut val) = add.parse::<u32>() {
                    val *= 4;
                    ctx.req_mut()
                        .headers_mut()
                        .insert("math".to_owned(), val.to_string());
                }
            }
            ctx.next()
        }

        fn exponentiate(mut ctx: Ctx) -> BoxFuture<Response> {
            if let Some(add) = ctx.req_mut().headers().get("math") {
                if let Ok(mut val) = add.parse::<u32>() {
                    val = val.pow(4);
                    ctx.req_mut()
                        .headers_mut()
                        .insert("math".to_owned(), val.to_string());
                }
            }
            ctx.next()
        }

        let mut app = App::new();
        app.post("/foo", handler);
        app.post("/stream", stream);
        app.post("/bar", async move |_req| {
            let mut res = Response::with_status(Status::Ok);
            res.set_json(json!({"foo": "bar"}));
            res
        });
        app.post("/baz", async move |_req| Response::with_status(Status::Ok));
        app.post("/trailers", async move |mut req: Request| {
            let mut res = Response::with_status(Status::Ok);
            match req.trailers().await {
                Some(trailers) => {
                    res.headers_mut().insert(
                        "expires".to_owned(),
                        trailers.get("expires").unwrap().to_owned(),
                    );
                }
                None => {
                    return Response::with_status(Status::BadRequest);
                }
            }
            res
        });
        app.get("/add/echo", math);
        app.get("/multiply/echo", math);
        app.get("/math/echo", math);
        app.post("/math/echo", math);
        app.get("/echo", math);
        app.middleware("*", logger);
        app.files("/public/", "./examples/static/");
        app.middleware("/math", add);
        app.middleware("/math", multiply);
        app.post_middleware("/math", exponentiate);
        app.middleware("/add", add);
        app.middleware("/multiply", multiply);
        app.middleware("/exponentiate", exponentiate);
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
