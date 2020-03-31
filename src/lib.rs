#![feature(async_closure)]
#![feature(or_patterns)]
#![feature(exclusive_range_pattern)]

mod body;
mod request;
mod response;
mod router;
mod server;

pub use body::Body;
pub use request::{Encoding, Method, Request};
pub use response::{Response, Status};
pub use router::Ctx;

use async_std::task;
use router::{Middleware, Path, Route, Router};
use std::{future::Future, pin::Pin, str, sync::Arc};

pub(crate) const BUF_LEN: usize = 1024;
pub(crate) const KEEP_ALIVE_TIMEOUT: u64 = 10;

pub(crate) type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;
pub(crate) type Sender<T> = futures::channel::mpsc::Sender<T>;
pub(crate) type Receiver<T> = futures::channel::mpsc::Receiver<T>;
pub(crate) type BoxFuture<'a, Response> = Pin<Box<dyn Future<Output = Response> + Send + 'static>>;

pub struct App<Routes> {
    router: Router<Routes>,
}

impl App<()> {
    pub fn new() -> App<()> {
        Self::with_routes(())
    }
    pub fn run(self, port: u32) -> Result<()> {
        let router = Arc::new(self.router);
        task::block_on(server::accept_loop("127.0.0.1", port, router))
    }
    pub fn get(&mut self, path: &'static str, handler: impl Route) -> &Self {
        self.add_route(Path::new(Method::Get, path.to_owned()), handler);
        self
    }
    pub fn post(&mut self, path: &'static str, handler: impl Route) -> &Self {
        self.add_route(Path::new(Method::Post, path.to_owned()), handler);
        self
    }
    pub fn put(&mut self, path: &'static str, handler: impl Route) -> &Self {
        self.add_route(Path::new(Method::Put, path.to_owned()), handler);
        self
    }
    pub fn delete(&mut self, path: &'static str, handler: impl Route) -> &Self {
        self.add_route(Path::new(Method::Delete, path.to_owned()), handler);
        self
    }
    pub fn middleware(&mut self, path: &'static str, middleware: impl Middleware) -> &Self {
        self.add_middleware(Path::all(path.to_owned()), middleware);
        self
    }
    pub fn get_middleware(&mut self, path: &'static str, middleware: impl Middleware) -> &Self {
        self.add_middleware(Path::new(Method::Get, path.to_owned()), middleware);
        self
    }
    pub fn post_middleware(&mut self, path: &'static str, middleware: impl Middleware) -> &Self {
        self.add_middleware(Path::new(Method::Post, path.to_owned()), middleware);
        self
    }
    pub fn put_middleware(&mut self, path: &'static str, middleware: impl Middleware) -> &Self {
        self.add_middleware(Path::new(Method::Put, path.to_owned()), middleware);
        self
    }
    pub fn delete_middleware(&mut self, path: &'static str, middleware: impl Middleware) -> &Self {
        self.add_middleware(Path::new(Method::Delete, path.to_owned()), middleware);
        self
    }
    fn add_route(&mut self, route: Path, handler: impl Route) {
        self.router.routes.insert(route, Box::new(handler));
    }
    fn add_middleware(&mut self, route: Path, middleware: impl Middleware) {
        self.router.middleware.push((route, Arc::new(middleware)));
    }
}

impl<Routes: Send + Sync + Copy + 'static> App<Routes> {
    pub fn with_routes(routes: Routes) -> App<Routes> {
        App {
            router: Router::new(routes),
        }
    }
}

async fn not_found(_req: Request) -> Response {
    Response::status(Status::NotFound)
}
