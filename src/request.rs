use crate::{Path, Receiver, Result};
use async_std::prelude::*;
use futures::{select, FutureExt};
use std::{
    collections::hash_map::HashMap,
    fmt,
    sync::{Arc, Mutex},
};

pub struct Request {
    pub bytes: Vec<u8>,
    pub version: u8,
    pub method: Method,
    pub path: String,
    pub headers: HashMap<String, String>,
    pub trailers: Option<HashMap<String, String>>,
    pub body_rx: Arc<Mutex<Option<Receiver<Chunk>>>>,
}

impl Request {
    pub(crate) fn from_parser(parser: httparse::Request) -> Result<Request> {
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
        Ok(Request {
            bytes: vec![],
            version: parser.version.unwrap(),
            path: parser.path.unwrap().to_owned(),
            method: parser.method.unwrap().to_lowercase().parse().unwrap(),
            headers,
            trailers: None,
            body_rx: Arc::new(Mutex::new(None)),
        })
    }

    pub fn set_body(&mut self, receiver: Receiver<Chunk>) {
        self.body_rx = Arc::new(Mutex::new(Some(receiver)))
    }

    pub fn is_keep_alive(&self) -> bool {
        match self.headers.get("connection") {
            Some(connection) => "keep-alive" == connection,
            None => self.version == 1,
        }
    }

    pub fn transfer_endcoding(&self) -> Encoding {
        match self.headers.get("transfer-encoding") {
            Some(encoding) => encoding.parse().unwrap_or(Encoding::Identity),
            None => Encoding::Identity,
        }
    }

    pub fn path(&self) -> Path {
        Path::new(self.method, self.path.clone())
    }

    pub fn content_len(&self) -> Option<usize> {
        self.headers
            .get("content-length")
            .and_then(|cl: &String| cl.parse().ok())
    }

    pub async fn json(&mut self) -> Result<serde_json::Value> {
        self.body().await?;
        if let Some(cl) = self.headers.get("content-length") {
            println!("content-length: {}", cl);
        }
        println!("utf8 {:?}", std::str::from_utf8(&self.bytes));
        Ok(serde_json::from_slice(&self.bytes.as_slice())?)
    }

    pub fn check_trailers(&self) -> Vec<String> {
        match self.headers.get("trailer") {
            Some(trailers) => trailers
                .chars()
                .filter(|c| !c.is_whitespace())
                .collect::<String>()
                .split(',')
                .map(|s| s.to_owned())
                .collect::<Vec<String>>(),
            None => vec![],
        }
    }

    pub async fn trailers(&mut self) -> &Option<HashMap<String, String>> {
        self.body().await.ok();
        &self.trailers
    }

    pub async fn bytes(&mut self) -> Result<&Vec<u8>> {
        self.body().await?;
        Ok(&self.bytes)
    }

    pub fn take_body(&mut self) -> std::io::Result<Option<Receiver<Chunk>>> {
        let body = self
            .body_rx
            .clone()
            .lock()
            .or_else(|_| {
                Err(std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    "body error",
                ))
            })?
            .take();
        Ok(body)
    }

    async fn body(&mut self) -> Result<()> {
        let body = self.take_body()?;
        match body {
            Some(body) => {
                let mut stream = body.fuse();
                if self.transfer_endcoding() == Encoding::Chunked {
                    loop {
                        let chunk = select! {
                            chunk = stream.next().fuse() => match chunk {
                                None => break,
                                Some(chunk) => chunk,
                            },
                        };
                        match chunk {
                            Chunk::Body { buf, size } => {
                                self.bytes.extend_from_slice(&buf[..size]);
                            }
                            Chunk::Trailers { trailers } => {
                                self.trailers = Some(trailers);
                                break;
                            }
                        }
                    }
                } else {
                    let cl = self.content_len().unwrap();
                    while self.bytes.len() < cl as usize {
                        let chunk = select! {
                            chunk = stream.next().fuse() => match chunk {
                                None => break,
                                Some(chunk) => chunk,
                            },
                        };
                        match chunk {
                            Chunk::Body { buf, size } => {
                                self.bytes.extend_from_slice(&buf[..size]);
                            }
                            _ => {
                                return Err(Box::new(std::io::Error::new(
                                    std::io::ErrorKind::InvalidData,
                                    "trailers for identity",
                                )));
                            }
                        }
                    }
                }
            }
            None => {}
        }
        Ok(())
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

impl fmt::Display for Method {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        fmt::Debug::fmt(self, f)
    }
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

#[derive(Debug, Hash, Eq, PartialEq, Copy, Clone)]
pub enum Encoding {
    Chunked,
    Identity,
}

impl fmt::Display for Encoding {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        fmt::Debug::fmt(self, f)
    }
}

impl std::str::FromStr for Encoding {
    type Err = ();
    fn from_str(s: &str) -> std::result::Result<Self, ()> {
        match s {
            "identity" => Ok(Encoding::Identity),
            "chunked" => Ok(Encoding::Chunked),
            _ => Err(()),
        }
    }
}

pub enum Chunk {
    Body {
        buf: [u8; crate::BUF_LEN],
        size: usize,
    },
    Trailers {
        trailers: HashMap<String, String>,
    },
}
