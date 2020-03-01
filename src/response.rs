use crate::Result;
use std::collections::hash_map::HashMap;

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

    pub fn from_parts(
        parsed: httparse::Response,
        bytes: Vec<u8>,
        content_length: usize,
        headers: HashMap<String, String>,
    ) -> Result<Response> {
        Ok(Response {
            status: Status::new(parsed.code.unwrap()).unwrap(),
            headers: headers,
            body: vec![],
            bytes: bytes,
            content_length: content_length,
        })
    }
}

#[derive(Debug)]
pub enum Status {
    Continue,
    SwitchingProtocol,
    Ok,
    BadRequest,
    NotFound,
    RequestTimeout,
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
        }
    }
}
