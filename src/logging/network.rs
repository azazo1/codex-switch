//! 出站网络请求的透明记录包装.
//!
//! [`HttpClient`] 包装 reqwest::Client 并提供一致的调用接口, 每个请求在发送时
//! 自动写入 HAR 记录 (见 [`super::har`]), 仅在完整调试日志开启时生效;
//! 关闭时行为与裸 reqwest::Client 一致, 不缓冲响应体, 没有额外开销.

use super::har::PendingHar;
use bytes::Bytes;
use futures_util::Stream;
use std::{
    future::Future,
    pin::Pin,
    task::{Context, Poll},
    time::Duration,
};

const SSE_CONTENT_TYPE: &str = "text/event-stream";

/// reqwest::Client 的透明包装, 调用接口与 reqwest 保持一致.
#[derive(Clone)]
pub(crate) struct HttpClient {
    inner: reqwest::Client,
}

impl HttpClient {
    /// 仅测试使用; 生产环境统一经 app::http::build_client 构造.
    #[cfg(test)]
    pub(crate) fn new() -> Self {
        Self {
            inner: reqwest::Client::new(),
        }
    }

    pub(crate) fn from_client(inner: reqwest::Client) -> Self {
        Self { inner }
    }

    pub(crate) fn get(&self, url: impl reqwest::IntoUrl) -> RequestBuilder {
        self.wrap(self.inner.get(url))
    }

    pub(crate) fn post(&self, url: impl reqwest::IntoUrl) -> RequestBuilder {
        self.wrap(self.inner.post(url))
    }

    pub(crate) fn request(
        &self,
        method: reqwest::Method,
        url: impl reqwest::IntoUrl,
    ) -> RequestBuilder {
        self.wrap(self.inner.request(method, url))
    }

    /// 执行一个已构建的请求, 与 reqwest::Client::execute 一致.
    pub(crate) fn execute(&self, request: reqwest::Request) -> SendFuture {
        let pending = PendingHar::from_request(&request);
        SendFuture {
            inner: Box::pin(drive(self.inner.clone(), request, pending)),
        }
    }

    fn wrap(&self, inner: reqwest::RequestBuilder) -> RequestBuilder {
        RequestBuilder {
            client: self.inner.clone(),
            inner,
        }
    }
}

/// reqwest::RequestBuilder 的透明包装, send 时自动记录 HAR.
pub(crate) struct RequestBuilder {
    client: reqwest::Client,
    inner: reqwest::RequestBuilder,
}

impl RequestBuilder {
    pub(crate) fn header<K, V>(self, key: K, value: V) -> Self
    where
        reqwest::header::HeaderName: TryFrom<K>,
        reqwest::header::HeaderValue: TryFrom<V>,
        axum::http::Error: From<<reqwest::header::HeaderName as TryFrom<K>>::Error>,
        axum::http::Error: From<<reqwest::header::HeaderValue as TryFrom<V>>::Error>,
    {
        Self {
            client: self.client,
            inner: self.inner.header(key, value),
        }
    }

    pub(crate) fn bearer_auth(self, token: impl std::fmt::Display) -> Self {
        Self {
            client: self.client,
            inner: self.inner.bearer_auth(token),
        }
    }

    pub(crate) fn query<T: serde::Serialize + ?Sized>(self, query: &T) -> Self {
        Self {
            client: self.client,
            inner: self.inner.query(query),
        }
    }

    pub(crate) fn json<T: serde::Serialize + ?Sized>(self, json: &T) -> Self {
        Self {
            client: self.client,
            inner: self.inner.json(json),
        }
    }

    pub(crate) fn form<T: serde::Serialize + ?Sized>(self, form: &T) -> Self {
        Self {
            client: self.client,
            inner: self.inner.form(form),
        }
    }

    pub(crate) fn body(self, body: impl Into<reqwest::Body>) -> Self {
        Self {
            client: self.client,
            inner: self.inner.body(body),
        }
    }

    pub(crate) fn timeout(self, timeout: Duration) -> Self {
        Self {
            client: self.client,
            inner: self.inner.timeout(timeout),
        }
    }

    /// 构建请求, 与 reqwest::RequestBuilder::build 一致.
    pub(crate) fn build(self) -> reqwest::Result<reqwest::Request> {
        self.inner.build()
    }

    pub(crate) fn send(self) -> SendFuture {
        match self.inner.build() {
            Ok(request) => {
                let pending = PendingHar::from_request(&request);
                SendFuture {
                    inner: Box::pin(drive(self.client, request, pending)),
                }
            }
            Err(err) => SendFuture {
                inner: Box::pin(std::future::ready(Err(err))),
            },
        }
    }
}

/// send/execute 的返回 future; 被提前丢弃 (例如用户终止流式请求) 时,
/// 内部的 PendingHar 会以未完成状态落盘已捕获的部分.
pub(crate) struct SendFuture {
    inner: Pin<Box<dyn Future<Output = reqwest::Result<reqwest::Response>> + Send>>,
}

impl Future for SendFuture {
    type Output = reqwest::Result<reqwest::Response>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        self.inner.as_mut().poll(cx)
    }
}

async fn drive(
    client: reqwest::Client,
    request: reqwest::Request,
    pending: Option<PendingHar>,
) -> reqwest::Result<reqwest::Response> {
    let response = match client.execute(request).await {
        Ok(response) => response,
        Err(err) => {
            if let Some(pending) = &pending {
                pending.clone().fail(&err);
            }
            return Err(err);
        }
    };
    let Some(pending) = pending else {
        return Ok(response);
    };
    let status = response.status();
    let headers = response.headers().clone();
    let version = response.version();
    pending.on_response(status, version, &headers);
    if mime_is_sse(&headers) {
        let tee = TeeStream::new(response.bytes_stream(), Some(pending));
        rebuild_response(status, headers, version, reqwest::Body::wrap_stream(tee))
    } else {
        match response.bytes().await {
            Ok(bytes) => {
                pending.finish_body(&bytes);
                rebuild_response(status, headers, version, reqwest::Body::from(bytes))
            }
            Err(err) => {
                pending.fail(&err);
                Err(err)
            }
        }
    }
}

fn mime_is_sse(headers: &reqwest::header::HeaderMap) -> bool {
    headers
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.contains(SSE_CONTENT_TYPE))
}

fn rebuild_response(
    status: reqwest::StatusCode,
    headers: reqwest::header::HeaderMap,
    version: reqwest::Version,
    body: reqwest::Body,
) -> reqwest::Result<reqwest::Response> {
    let mut builder = axum::http::Response::builder()
        .status(status)
        .version(version);
    for (name, value) in headers.iter() {
        builder = builder.header(name, value);
    }
    // 头和版本均来自真实响应, 重组不可能失败.
    let response = builder
        .body(body)
        .expect("failed to rebuild upstream response");
    Ok(reqwest::Response::from(response))
}

/// 响应体透传流, 逐块累计内容供 HAR 记录; 提前丢弃时落盘已捕获的部分.
pub(crate) struct TeeStream<S> {
    inner: Pin<Box<S>>,
    pending: Option<PendingHar>,
}

impl<S> TeeStream<S> {
    pub(crate) fn new(inner: S, pending: Option<PendingHar>) -> Self {
        Self {
            inner: Box::pin(inner),
            pending,
        }
    }
}

impl<S, E> Stream for TeeStream<S>
where
    S: Stream<Item = Result<Bytes, E>>,
{
    type Item = Result<Bytes, E>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        match this.inner.as_mut().poll_next(cx) {
            Poll::Ready(Some(Ok(chunk))) => {
                if let Some(pending) = &this.pending {
                    pending.append_body(&chunk);
                }
                Poll::Ready(Some(Ok(chunk)))
            }
            Poll::Ready(item) => {
                if let Some(pending) = this.pending.take() {
                    pending.finish();
                }
                Poll::Ready(item)
            }
            Poll::Pending => Poll::Pending,
        }
    }
}

impl<S> Drop for TeeStream<S> {
    fn drop(&mut self) {
        // 响应流未读完就被丢弃, 落盘已捕获的部分.
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{Json, Router, routing::get as axum_get};
    use futures_util::StreamExt;

    /// 端到端验证: 经 HttpClient 的普通和 SSE 请求都会完整写入 HAR.
    // 持锁方都是同步测试代码, 不存在跨 await 死锁, 允许该 lint.
    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn http_client_records_requests_to_har() {
        // 与其他触碰全局调试开关的测试串行, 避免开关被并发翻转.
        let _flag_guard = crate::logging::tests::BODY_FLAG_TEST_LOCK
            .lock()
            .unwrap_or_else(|err| err.into_inner());
        // 仅本测试会触发 HAR 全局写入, 用唯一路径隔离.
        let har_path =
            std::env::temp_dir().join(format!("cs-har-e2e-{}.har", uuid::Uuid::new_v4()));
        // SAFETY: 测试进程内单线程设置, 且只有本测试读取该路径写入 HAR.
        unsafe {
            std::env::set_var("CODEX_SWITCH_LOG_FILE", &har_path);
        }
        crate::logging::set_body_logging_enabled(true);

        let app = Router::new()
            .route(
                "/json",
                axum_get(|| async { Json(serde_json::json!({"answer": 42})) }),
            )
            .route(
                "/sse",
                axum_get(|| async {
                    let body = "data: one\n\ndata: two\n\n";
                    (
                        [(reqwest::header::CONTENT_TYPE, "text/event-stream")],
                        body.to_string(),
                    )
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let base = format!("http://{address}");

        let client = HttpClient::from_client(reqwest::Client::new());
        let json = client
            .get(format!("{base}/json"))
            .send()
            .await
            .unwrap()
            .json::<serde_json::Value>()
            .await
            .unwrap();
        assert_eq!(json["answer"], 42);

        let response = client.get(format!("{base}/sse")).send().await.unwrap();
        assert_eq!(response.status(), reqwest::StatusCode::OK);
        let mut stream = response.bytes_stream();
        let mut sse_body = String::new();
        while let Some(chunk) = stream.next().await {
            sse_body.push_str(&String::from_utf8_lossy(&chunk.unwrap()));
        }
        assert!(sse_body.contains("data: one"));
        assert!(sse_body.contains("data: two"));

        // entry 在请求完成时同步落盘, 无需等待. 并行测试的请求也可能写入该全局
        // 重定向文件, 因此只断言本测试自己发出的 entry.
        let har_path = crate::logging::network_har_file_path().unwrap();
        let content = std::fs::read_to_string(&har_path).unwrap();
        let log: serde_json::Value = serde_json::from_str(&content).unwrap();
        let entries = log["log"]["entries"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|entry| {
                entry["request"]["url"]
                    .as_str()
                    .is_some_and(|url| url.starts_with(&base))
            })
            .collect::<Vec<_>>();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0]["request"]["url"], format!("{base}/json"));
        assert_eq!(entries[0]["response"]["content"]["text"], "{\"answer\":42}");
        assert_eq!(entries[1]["request"]["url"], format!("{base}/sse"));
        let sse_text = entries[1]["response"]["content"]["text"].as_str().unwrap();
        assert!(sse_text.contains("data: one") && sse_text.contains("data: two"));
        crate::logging::set_body_logging_enabled(false);
        std::fs::remove_file(&har_path).ok();
    }
}
