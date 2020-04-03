use crate::{request::Chunk, Receiver};
use async_std::io::Read;
use futures::Stream;
use std::{
    cmp,
    io::Error,
    pin::Pin,
    task::{Context, Poll},
};

const LE: &[u8; 2] = b"\
\r\n\
";

pub struct Body {
    receiver: Receiver<Chunk>,
    last_bit: Option<[u8; 5]>,
}

impl Body {
    pub fn new(receiver: Receiver<Chunk>) -> Body {
        Body {
            receiver,
            last_bit: Some(
                b"\
            0\r\n\r\n\
            "
                .to_owned(),
            ),
        }
    }
}

impl Read for Body {
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
                    let max = cmp::min(size, poll_buf.len());
                    let mut offset = 0;
                    let sz = format!("{:x}", max).into_bytes();
                    offset += sz.len();
                    poll_buf[..sz.len()].copy_from_slice(sz.as_slice());
                    poll_buf[offset..(offset + LE.len())].copy_from_slice(LE);
                    offset += LE.len();
                    poll_buf[offset..(offset + max)].copy_from_slice(&buf[..max]);
                    offset += max;
                    poll_buf[offset..(offset + LE.len())].copy_from_slice(LE);
                    offset += LE.len();
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
