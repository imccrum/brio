use crate::{request::Stream, Receiver};
use async_std::io::Read;
use async_std::prelude::*;
use futures::future::select;
use futures::{select, FutureExt, Stream as OtherStream, StreamExt};
use std::{
    io::Error,
    pin::Pin,
    task::{Context, Poll},
};

pub struct Body {
    receiver: Receiver<Stream>,
}

impl Body {
    pub fn new(receiver: Receiver<Stream>) -> Body {
        Body { receiver }
    }
}

impl Read for Body {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context,
        buf: &mut [u8],
    ) -> Poll<Result<usize, Error>> {
        let blah = futures::ready!(Pin::new(&mut self.receiver).as_mut().poll_next(cx));
        let n = match blah {
            Some(res) => {}
            None => {}
        };
        //self.receiver.as_mut().poll_next(cx)
        //let read = futures::ready!(self.receiver.next())?;
        return Poll::Pending;
    }
}
