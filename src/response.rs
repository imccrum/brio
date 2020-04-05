use crate::body::ChunkedBody;
use crate::{request::Encoding, Result};
use mime;
use std::{collections::hash_map::HashMap, pin::Pin};

pub struct Response {
    pub status: Status,
    pub headers: HashMap<String, String>,
    pub buf: Vec<u8>,
    pub stream: Option<Pin<Box<dyn futures::io::AsyncRead + Send>>>,
}

impl Response {
    pub fn status(status: Status) -> Response {
        Response {
            status,
            headers: HashMap::new(),
            buf: vec![],
            stream: None,
        }
    }

    pub fn from_parser(parser: httparse::Response) -> Result<Response> {
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
        Ok(Response {
            status: Status::new(parser.code.unwrap()).unwrap(),
            headers,
            buf: vec![],
            stream: None,
        })
    }

    pub fn json(&mut self, json: serde_json::Value) {
        self.headers.insert(
            "content-type".to_owned(),
            mime::APPLICATION_JSON.to_string(),
        );
        self.buf = json.to_string().into_bytes();
    }

    pub fn bytes(&mut self, buf: Vec<u8>, mime: mime::Mime) {
        self.headers
            .insert("content-type".to_owned(), mime.to_string());
        self.buf = buf;
    }

    pub fn stream(&mut self, receiver: ChunkedBody, mime: Option<mime::Mime>) {
        self.headers.insert(
            "transfer-encoding".to_owned(),
            Encoding::Chunked.to_string().to_ascii_lowercase(),
        );
        if let Some(mime) = mime {
            self.headers
                .insert("content-type".to_owned(), mime.to_string());
        }
        self.stream = Some(Box::pin(receiver))
    }

    pub fn into_bytes(mut self) -> Vec<u8> {
        let mut bytes = self.head_as_bytes();
        bytes.append(&mut self.buf);
        bytes
    }

    pub fn content_len(&self) -> Option<usize> {
        self.headers
            .get("content-length")
            .and_then(|cl: &String| cl.parse().ok())
    }

    pub fn content_type(&self) -> Option<mime::Mime> {
        self.headers
            .get("content-type")
            .and_then(|ct: &String| ct.parse().ok())
    }

    pub fn transfer_endcoding(&self) -> Encoding {
        match self.headers.get("transfer-encoding") {
            Some(encoding) => encoding.parse().unwrap_or(Encoding::Identity),
            None => Encoding::Identity,
        }
    }

    pub fn head_as_bytes(&mut self) -> Vec<u8> {
        let mut bytes: Vec<u8> = vec![];
        if self.transfer_endcoding() != Encoding::Chunked {
            self.headers
                .insert("content-length".to_owned(), self.buf.len().to_string());
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
        bytes
    }
}

#[derive(Debug, Eq, PartialEq, Copy, Clone)]
pub enum Status {
    Continue = 100,
    SwitchingProtocol = 101,
    Ok = 200,
    BadRequest = 400,
    NotFound = 404,
    RequestTimeout = 408,
    ImATeapot = 418,
}

impl Status {
    pub fn new(code: u16) -> Option<Status> {
        match code {
            100 => Some(Status::Continue),
            101 => Some(Status::SwitchingProtocol),
            200 => Some(Status::Ok),
            400 => Some(Status::BadRequest),
            404 => Some(Status::NotFound),
            408 => Some(Status::RequestTimeout),
            418 => Some(Status::ImATeapot),
            _ => None,
        }
    }
    pub fn bytes(&self) -> &[u8] {
        match *self {
            Status::Continue => b"100 Continue\r\n",
            Status::SwitchingProtocol => b"101 Switching Protocol\r\n",
            Status::Ok => b"200 OK\r\n",
            Status::BadRequest => b"400 Bad Request\r\n",
            Status::NotFound => b"404 Not Found\r\n",
            Status::RequestTimeout => b"408 Request Timeout\r\n",
            Status::ImATeapot => b"418 I'm a teapot\r\n",
        }
    }
}
