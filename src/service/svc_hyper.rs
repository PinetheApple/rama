use std::{convert::Infallible, pin::Pin};

use futures_util::Future;

use super::{Context, Service};

/// Wrapper service that implements [`hyper::service::Service`].
///
/// ## Performance
///
/// Currently we require a clone of the service for each request.
/// This is because we need to be able to Box the future returned by the service.
/// Once we can specify such associated types using `impl Trait` we can skip this.
#[derive(Debug)]
pub struct HyperService<S, T> {
    ctx: Context<S>,
    inner: T,
}

impl<S, T> HyperService<S, T> {
    /// Create a new [`HyperService`] from a [`Context`] and a [`Service`].
    pub fn new(ctx: Context<S>, inner: T) -> Self {
        Self { ctx, inner }
    }
}

impl<S, T> hyper::service::Service<HyperRequest> for HyperService<S, T>
where
    S: Clone + Send + 'static,
    T: Service<S, HyperRequest, Response = hyper::Response<crate::http::Body>, Error = Infallible>
        + Clone
        + Send
        + 'static,
{
    type Response = hyper::Response<crate::http::Body>;
    type Error = std::convert::Infallible;
    type Future =
        Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send + 'static>>;

    fn call(&self, req: hyper::Request<hyper::body::Incoming>) -> Self::Future {
        let ctx = self.ctx.clone();
        let inner = self.inner.clone();
        Box::pin(async move { inner.serve(ctx, req).await })
    }
}

impl<S, T> Clone for HyperService<S, T>
where
    S: Clone,
    T: Clone,
{
    fn clone(&self) -> Self {
        Self {
            ctx: self.ctx.clone(),
            inner: self.inner.clone(),
        }
    }
}

type HyperRequest = hyper::Request<hyper::body::Incoming>;
