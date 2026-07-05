use axum::{
    body::Body,
    http::{HeaderMap, HeaderName, HeaderValue, StatusCode},
    response::Response,
};
use bytes::Bytes;
use futures_util::Stream;
use std::io;

pub(super) fn build_response(
    status: StatusCode,
    headers: reqwest::header::HeaderMap,
    body: Vec<u8>,
) -> Response {
    let mut builder = Response::builder().status(status);
    for (name, value) in headers {
        if let Some(name) = name
            && should_return_header(name.as_str())
        {
            builder = builder.header(name.as_str(), value.as_bytes());
        }
    }
    builder.body(axum::body::Body::from(body)).unwrap()
}

pub(super) fn build_stream_response<S>(
    status: StatusCode,
    headers: reqwest::header::HeaderMap,
    stream: S,
) -> Response
where
    S: Stream<Item = Result<Bytes, io::Error>> + Send + 'static,
{
    let mut builder = Response::builder().status(status);
    for (name, value) in headers {
        if let Some(name) = name
            && should_return_header(name.as_str())
        {
            builder = builder.header(name.as_str(), value.as_bytes());
        }
    }
    builder.body(Body::from_stream(stream)).unwrap()
}

pub(super) fn to_axum_headers(headers: &reqwest::header::HeaderMap) -> HeaderMap {
    let mut result = HeaderMap::new();
    for (name, value) in headers {
        if let (Ok(name), Ok(value)) = (
            HeaderName::from_bytes(name.as_str().as_bytes()),
            HeaderValue::from_bytes(value.as_bytes()),
        ) {
            result.insert(name, value);
        }
    }
    result
}

fn should_return_header(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "content-type" | "cache-control" | "x-request-id"
    )
}
