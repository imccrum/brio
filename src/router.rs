use crate::{not_found, BoxFuture, Method, Request, Response};
use std::collections::hash_map::HashMap;
use std::future::Future;
use std::sync::Arc;

pub struct Router<Routes> {
    pub routes: HashMap<Path, Box<dyn Route>>,
    pub middleware: Vec<(Path, Arc<dyn Middleware>)>,
    pub config: Routes,
    pub not_found: Box<dyn Route + 'static>,
}

impl<Routes: Send + Sync + Copy + Clone + 'static> Router<Routes> {
    pub fn new(config: Routes) -> Router<Routes> {
        Router {
            routes: HashMap::new(),
            middleware: vec![],
            config,
            not_found: Box::new(not_found),
        }
    }
}

#[derive(Hash, Eq, PartialEq, Debug)]
pub struct Path {
    method: Option<Method>,
    pub(crate) path: String,
}

impl Path {
    pub fn new(method: Method, path: String) -> Path {
        Path {
            method: Some(method),
            path,
        }
    }
    pub fn all(path: String) -> Path {
        Path { method: None, path }
    }
    pub fn contains(&self, middleware: &Path) -> bool {
        if let (Some(req), Some(middleware)) = (&self.method, &middleware.method) {
            if req != middleware {
                return false;
            }
        };
        middleware.path == "*" || self.path.starts_with(&middleware.path)
    }
}

pub trait Route: Send + Sync + 'static {
    fn run<'a>(&'a self, req: Request) -> BoxFuture<'a, Response>;
}

impl<F: Send + Sync + 'static, Fut> Route for F
where
    F: Fn(Request) -> Fut,
    Fut: Future<Output = Response> + Send + 'static,
{
    fn run<'a>(&'a self, req: Request) -> BoxFuture<'a, Response> {
        let fut = (self)(req);
        Box::pin(async move { fut.await })
    }
}

pub trait Middleware: Send + Sync + 'static {
    fn handle<'a>(&'a self, ctx: Ctx<'a>) -> BoxFuture<'a, Response>;
}

impl<F> Middleware for F
where
    F: Send + Sync + 'static + Fn(Ctx) -> BoxFuture<Response>,
{
    fn handle<'a>(&'a self, ctx: Ctx<'a>) -> BoxFuture<'a, Response> {
        (self)(ctx)
    }
}

pub struct Ctx<'a> {
    pub req: Request,
    pub(crate) route: &'a Box<dyn Route>,
    pub(crate) next_middleware: &'a [(Path, Arc<dyn Middleware>)],
}

impl<'a> Ctx<'a> {
    pub fn next(mut self) -> BoxFuture<'a, Response> {
        let in_path = loop {
            if let Some((current, next)) = self.next_middleware.split_first() {
                self.next_middleware = next;
                if self.req.path().contains(&current.0) {
                    break Some(current);
                }
            } else {
                break None;
            }
        };
        if let Some(middleware) = in_path {
            middleware.1.handle(self)
        } else {
            self.route.run(self.req)
        }
    }
}
