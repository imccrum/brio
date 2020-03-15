use crate::{Path, Receiver, Result};
use async_std::prelude::*;
use futures::{select, FutureExt};
use std::{
    collections::hash_map::HashMap,
    sync::{Arc, Mutex},
};

pub struct Request {
    pub bytes: Vec<u8>,
    pub version: u8,
    pub method: Method,
    pub path: String,
    pub headers: HashMap<String, String>,
    pub content_len: u64,
    pub body_rx: Arc<Mutex<Option<Receiver<Event>>>>,
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
        let content_len: usize = match headers.get("content-length") {
            Some(cl) => cl.parse()?,
            None => 0,
        };
        Ok(Request {
            bytes: vec![],
            version: parser.version.unwrap(),
            path: parser.path.unwrap().to_owned(),
            method: parser.method.unwrap().to_lowercase().parse().unwrap(),
            headers: headers,
            content_len: content_len as u64,
            body_rx: Arc::new(Mutex::new(None)),
        })
    }

    pub fn set_body(&mut self, receiver: Receiver<Event>) {
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

    pub async fn json(&mut self) -> Result<serde_json::Value> {
        self.body().await?;
        if let Some(cl) = self.headers.get("content-length") {
            println!("content-length: {}", cl);
        }
        println!("utf8 {:?}", std::str::from_utf8(&self.bytes));
        Ok(serde_json::from_slice(&self.bytes.as_slice())?)
    }

    pub async fn bytes(&mut self) -> Result<&Vec<u8>> {
        self.body().await?;
        Ok(&self.bytes)
    }

    async fn body(&mut self) -> Result<()> {
        let take = self
            .body_rx
            .clone()
            .lock()
            .or(Err(Box::new(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "",
            ))))?
            .take();

        match take {
            Some(take) => {
                let mut stream = take.fuse();
                if self.transfer_endcoding() == Encoding::Chunked {
                    loop {
                        let chunk = select! {
                            chunk = stream.next().fuse() => match chunk {
                                None => {
                                    break;
                                },
                                Some(chunk) => chunk,
                            },
                        };
                        match chunk {
                            Event::Message { msg, size } => {
                                self.bytes.extend_from_slice(&msg[..size]);
                            }
                        }
                    }
                } else {
                    while self.bytes.len() < self.content_len as usize {
                        let chunk = select! {
                            chunk = stream.next().fuse() => match chunk {
                                None => break,
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

#[derive(Debug, Hash, Eq, PartialEq, Copy, Clone)]
pub enum Encoding {
    Chunked,
    Identity,
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
    Message {
        msg: [u8; crate::BUF_LEN],
        size: usize,
    },
}
