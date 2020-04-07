use crate::{request::Chunk, Receiver};
use async_std::io::Read;
use futures::Stream;
use std::{
    io::Error,
    pin::Pin,
    task::{Context, Poll},
};

const CRLF: &[u8; 2] = b"\
\r\n\
";
const LAST_BIT: &[u8; 5] = b"\
0\r\n\r\n\
";

pub struct ChunkedBody {
    receiver: Receiver<Chunk>,
    last_bit: Option<[u8; 5]>,
    extend: Vec<u8>,
}

impl ChunkedBody {
    pub fn new(receiver: Receiver<Chunk>) -> ChunkedBody {
        ChunkedBody {
            receiver,
            last_bit: Some(LAST_BIT.to_owned()),
            extend: vec![],
        }
    }

    fn copy_to_buf(&mut self, poll_buf: &mut [u8], buf: &[u8], offset: usize) -> usize {
        // todo save to extend if buffer is too small
        poll_buf[offset..(offset + buf.len())].copy_from_slice(buf);
        buf.len()
    }
}

impl Read for ChunkedBody {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context,
        poll_buf: &mut [u8],
    ) -> Poll<Result<usize, Error>> {
        if self.last_bit.is_none() {
            return Poll::Ready(Ok(0));
        }
        let chunk = futures::ready!(Pin::new(&mut self.receiver).as_mut().poll_next(cx));
        let buf_read = match chunk {
            Some(chunk) => match chunk {
                Chunk::Body { buf, size } => {
                    let mut offset = 0;
                    let sz = format!("{:x}", size).into_bytes();
                    offset += self.copy_to_buf(poll_buf, sz.as_slice(), offset);
                    offset += self.copy_to_buf(poll_buf, CRLF, offset);
                    offset += self.copy_to_buf(poll_buf, &buf[..size], offset);
                    offset += self.copy_to_buf(poll_buf, CRLF, offset);
                    offset
                }
                Chunk::Trailers {
                    trailers: _trailers,
                } => {
                    println!("ignoring trailers? {:?}", _trailers);
                    self.last_bit
                        .take()
                        .map(|last_bit| {
                            poll_buf[..last_bit.len()].copy_from_slice(&last_bit);
                            last_bit.len()
                        })
                        .unwrap_or(0)
                }
            },
            None => self
                .last_bit
                .take()
                .map(|last_bit| {
                    poll_buf[..last_bit.len()].copy_from_slice(&last_bit);
                    last_bit.len()
                })
                .unwrap_or(0),
        };
        Poll::Ready(Ok(buf_read))
    }
}
