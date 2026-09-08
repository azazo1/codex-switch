//! HAR 1.2 网络请求记录文件.
//!
//! 完整记录每个出站请求和响应的头与体, 写入独立的 codex-switch-network.har.
//! 文件采用流式追加结构, 每写入一条 entry 都保持文件尾部闭合,
//! 因此进程在任意时刻退出后文件仍然是合法的 HAR, 可直接用 Chrome DevTools 打开.

use serde::Serialize;
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Instant;

/// 单个请求或响应体的记录上限, 超出后截断并标记 `_truncated`.
const MAX_BODY_BYTES: usize = 10 * 1024 * 1024;
const HAR_TAIL: &str = "\n]}}";

static HAR_STATE: OnceLock<Mutex<HarFileWriter>> = OnceLock::new();
static ROTATION: OnceLock<HarRotation> = OnceLock::new();

#[derive(Clone, Copy)]
struct HarRotation {
    max_size_bytes: u64,
    max_files: usize,
}

/// 设置 HAR 文件轮转参数, 与主日志轮转共享同一份配置.
pub(crate) fn set_rotation_config(size_mb: u64, max_files: usize) {
    let _ = ROTATION.set(HarRotation {
        max_size_bytes: size_mb.max(1).saturating_mul(1024).saturating_mul(1024),
        max_files: max_files.max(1),
    });
}

/// 追加一条 HAR entry, 写入失败静默忽略, 调试功能不应影响主流程.
fn write_entry(entry: &HarEntry) {
    let Ok(json) = serde_json::to_string(entry) else {
        return;
    };
    let path = match crate::logging::network_har_file_path() {
        Ok(path) => path,
        Err(err) => {
            tracing::debug!(target: "codex_switch::network", error = %err, "failed to resolve har file path");
            return;
        }
    };
    let state = HAR_STATE.get_or_init(|| {
        let rotation = ROTATION
            .get()
            .copied()
            .unwrap_or(HarRotation {
                max_size_bytes: 20 * 1024 * 1024,
                max_files: 10,
            });
        Mutex::new(HarFileWriter::new(path, rotation))
    });
    let Ok(mut writer) = state.lock() else {
        return;
    };
    if let Err(err) = writer.append_entry(&json) {
        tracing::debug!(target: "codex_switch::network", error = %err, "failed to write har entry");
    }
}

struct HarFileWriter {
    path: PathBuf,
    rotation: HarRotation,
    writer: Option<BufWriter<File>>,
    /// 当前文件中 entries 区结束的绝对偏移, 不含闭合尾.
    entry_end: u64,
    has_entries: bool,
}

impl HarFileWriter {
    fn new(path: PathBuf, rotation: HarRotation) -> Self {
        Self {
            path,
            rotation,
            writer: None,
            entry_end: 0,
            has_entries: false,
        }
    }

    fn filename_for(&self, index: usize) -> PathBuf {
        if index == 0 {
            return self.path.clone();
        }
        let stem = self
            .path
            .file_stem()
            .map(|stem| {
                let mut name = stem.to_os_string();
                name.push(format!(".{index}.har"));
                name
            })
            .unwrap_or_else(|| format!("codex-switch-network.{index}.har").into());
        self.path.with_file_name(stem)
    }

    fn append_entry(&mut self, json: &str) -> io::Result<()> {
        self.ensure_open()?;
        // 覆盖上一次写入的闭合尾, 在 entries 数组内追加新 entry 后重新闭合.
        let file = self.writer.as_mut().expect("har file is open").get_mut();
        file.seek(SeekFrom::Start(self.entry_end))?;
        file.set_len(self.entry_end)?;
        let separator = if self.has_entries { ",\n" } else { "\n" };
        file.write_all(separator.as_bytes())?;
        file.write_all(json.as_bytes())?;
        self.entry_end += separator.len() as u64 + json.len() as u64;
        self.has_entries = true;
        file.write_all(HAR_TAIL.as_bytes())?;
        file.flush()?;
        if self.entry_end >= self.rotation.max_size_bytes {
            self.rotate()?;
        }
        Ok(())
    }

    fn ensure_open(&mut self) -> io::Result<()> {
        if self.writer.is_some() {
            return Ok(());
        }
        if self.path.exists() {
            match read_tail_state(&self.path)? {
                TailState::Closed(entry_end) => {
                    // 上次会话正常结束, 去掉闭合尾继续追加.
                    self.open_file()?;
                    self.entry_end = entry_end;
                    self.has_entries = true;
                    return Ok(());
                }
                TailState::Broken => {
                    // 上次会话写入一半崩溃, 残留内容无法保证合法, 原样轮转保留供人工分析.
                    self.rotate()?;
                    return Ok(());
                }
            }
        }
        self.start_new_file()
    }

    fn open_file(&mut self) -> io::Result<()> {
        if let Some(parent) = self.path.parent().filter(|p| !p.as_os_str().is_empty()) {
            fs::create_dir_all(parent)?;
        }
        let file = OpenOptions::new()
            .create(true)
            .write(true)
            .read(true)
            .truncate(false)
            .open(&self.path)?;
        self.writer = Some(BufWriter::new(file));
        Ok(())
    }

    fn rotate(&mut self) -> io::Result<()> {
        self.writer = None;
        let _ = fs::remove_file(self.filename_for(self.rotation.max_files));
        let mut result = Ok(());
        for index in (0..self.rotation.max_files).rev() {
            let from = self.filename_for(index);
            let to = self.filename_for(index + 1);
            if let Err(err) = fs::rename(&from, &to).or_else(|err| match err.kind() {
                io::ErrorKind::NotFound => Ok(()),
                _ => Err(err),
            }) && result.is_ok()
            {
                result = Err(err);
            }
        }
        result?;
        self.start_new_file()
    }

    fn start_new_file(&mut self) -> io::Result<()> {
        self.open_file()?;
        let header = har_header();
        let file = self.writer.as_mut().expect("har file is open").get_mut();
        file.write_all(header.as_bytes())?;
        file.flush()?;
        self.entry_end = header.len() as u64;
        self.has_entries = false;
        Ok(())
    }
}

enum TailState {
    /// 文件以闭合尾结束, 返回 entries 区结束偏移.
    Closed(u64),
    Broken,
}

fn read_tail_state(path: &Path) -> io::Result<TailState> {
    let mut file = File::open(path)?;
    let len = file.metadata()?.len();
    let tail_len = HAR_TAIL.len() as u64;
    if len < tail_len {
        return Ok(TailState::Broken);
    }
    file.seek(SeekFrom::Start(len - tail_len))?;
    let mut tail = vec![0u8; tail_len as usize];
    file.read_exact(&mut tail)?;
    if tail == HAR_TAIL.as_bytes() {
        Ok(TailState::Closed(len - tail_len))
    } else {
        Ok(TailState::Broken)
    }
}

fn har_header() -> String {
    format!(
        "{{\"log\":{{\"version\":\"1.2\",\"creator\":{{\"name\":\"codex-switch\",\"version\":\"{}\"}},\"entries\":[",
        env!("CARGO_PKG_VERSION")
    )
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct HarEntry {
    started_date_time: String,
    time: u64,
    request: HarRequest,
    response: HarResponse,
    cache: serde_json::Value,
    timings: HarTimings,
    #[serde(rename = "_error", skip_serializing_if = "Option::is_none")]
    error: Option<String>,
    #[serde(rename = "_truncated", skip_serializing_if = "Option::is_none")]
    truncated: Option<bool>,
}

#[derive(Clone, Serialize)]
struct HarRequest {
    method: String,
    url: String,
    #[serde(rename = "httpVersion")]
    http_version: String,
    headers: Vec<HarHeader>,
    #[serde(rename = "headersSize")]
    headers_size: i64,
    #[serde(rename = "bodySize")]
    body_size: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    post_data: Option<HarPostData>,
}

#[derive(Clone, Serialize)]
struct HarPostData {
    #[serde(rename = "mimeType")]
    mime_type: String,
    text: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct HarResponse {
    status: u16,
    status_text: String,
    http_version: String,
    headers: Vec<HarHeader>,
    headers_size: i64,
    body_size: i64,
    content: HarContent,
    redirect_url: String,
}

#[derive(Serialize)]
struct HarContent {
    size: i64,
    #[serde(rename = "mimeType")]
    mime_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    text: Option<String>,
}

#[derive(Clone, Serialize)]
struct HarHeader {
    name: String,
    value: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct HarTimings {
    send: i64,
    wait: u64,
    receive: i64,
}

/// 敏感请求头在 HAR 中只记录占位符, 沿用 "API Key 不输出" 的既有承诺.
fn redact_header(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "authorization" | "x-api-key" | "proxy-authorization" | "cookie" | "set-cookie"
    )
}

const REDACTED_TEXT: &str = "[REDACTED]";

/// OAuth 认证服务器的请求体和响应体包含 refresh_token, code, access_token
/// 等凭据, 这些域名的 body 整体脱敏.
fn is_sensitive_url(url: &str) -> bool {
    url::Url::parse(url)
        .ok()
        .and_then(|parsed| parsed.host_str().map(str::to_ascii_lowercase))
        .is_some_and(|host| host == "auth.openai.com")
}

/// 计算落盘的 body 文本, 敏感域名整体替换为占位符.
fn final_body_text(sensitive: bool, bytes: &[u8]) -> Option<String> {
    (!bytes.is_empty()).then(|| {
        if sensitive {
            REDACTED_TEXT.to_string()
        } else {
            String::from_utf8_lossy(bytes).into_owned()
        }
    })
}

fn har_headers(headers: &reqwest::header::HeaderMap) -> Vec<HarHeader> {
    headers
        .iter()
        .map(|(name, value)| HarHeader {
            name: name.as_str().to_string(),
            value: if redact_header(name.as_str()) {
                "[REDACTED]".to_string()
            } else {
                String::from_utf8_lossy(value.as_bytes()).into_owned()
            },
        })
        .collect()
}

fn har_headers_hyper(headers: &hyper::HeaderMap) -> Vec<HarHeader> {
    headers
        .iter()
        .map(|(name, value)| HarHeader {
            name: name.as_str().to_string(),
            value: if redact_header(name.as_str()) {
                "[REDACTED]".to_string()
            } else {
                String::from_utf8_lossy(value.as_bytes()).into_owned()
            },
        })
        .collect()
}

fn http_version_str(version: reqwest::Version) -> String {
    match version {
        reqwest::Version::HTTP_09 => "HTTP/0.9".to_string(),
        reqwest::Version::HTTP_10 => "HTTP/1.0".to_string(),
        reqwest::Version::HTTP_11 => "HTTP/1.1".to_string(),
        reqwest::Version::HTTP_2 => "HTTP/2.0".to_string(),
        reqwest::Version::HTTP_3 => "HTTP/3.0".to_string(),
        _ => "HTTP/1.1".to_string(),
    }
}

fn mime_of(headers: &reqwest::header::HeaderMap) -> String {
    headers
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_string()
}

/// 一次出站请求的待写入记录, 请求开始时创建, 响应结束后落盘.
/// 未显式结束就丢弃 (例如客户端中途断开) 时, Drop 会写入已捕获的部分.
#[derive(Clone)]
pub(crate) struct PendingHar {
    state: Arc<Mutex<PendingHarState>>,
}

struct PendingHarState {
    started: Instant,
    started_at: String,
    request: HarRequest,
    request_body_truncated: bool,
    response: Option<HarResponse>,
    response_body: Vec<u8>,
    response_body_truncated: bool,
    error: Option<String>,
    done: bool,
}

impl PendingHar {
    /// 从已构建的 reqwest::Request 创建记录, 调试日志关闭时返回 None (不记录).
    pub(crate) fn from_request(request: &reqwest::Request) -> Option<Self> {
        if !crate::logging::body_logging_enabled() {
            return None;
        }
        let body = request.body().and_then(reqwest::Body::as_bytes);
        Some(Self::start_inner(
            request.method().as_str(),
            request.url().as_str(),
            har_headers(request.headers()),
            mime_of(request.headers()),
            body,
        ))
    }

    /// peer 客户端请求入口, 调试日志关闭时返回 None (不记录).
    pub(crate) fn start_hyper(
        method: &str,
        url: &str,
        headers: &hyper::HeaderMap,
        body: &[u8],
    ) -> Option<Self> {
        if !crate::logging::body_logging_enabled() {
            return None;
        }
        let mime = headers
            .get(hyper::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_string();
        Some(Self::start_inner(
            method,
            url,
            har_headers_hyper(headers),
            mime,
            Some(body),
        ))
    }

    fn start_inner(
        method: &str,
        url: &str,
        headers: Vec<HarHeader>,
        mime_type: String,
        body: Option<&[u8]>,
    ) -> Self {
        let truncated = body.is_some_and(|bytes| bytes.len() > MAX_BODY_BYTES);
        let recorded_body =
            body.map(|bytes| &bytes[..bytes.len().min(MAX_BODY_BYTES)]).unwrap_or_default();
        let body_size = i64::try_from(body.map(<[u8]>::len).unwrap_or_default()).unwrap_or(-1);
        let post_data = (!recorded_body.is_empty()).then(|| HarPostData {
            mime_type,
            text: String::from_utf8_lossy(recorded_body).into_owned(),
        });
        Self {
            state: Arc::new(Mutex::new(PendingHarState {
                started: Instant::now(),
                started_at: chrono::Local::now().to_rfc3339(),
                request: HarRequest {
                    method: method.to_string(),
                    url: url.to_string(),
                    http_version: "HTTP/1.1".to_string(),
                    headers_size: -1,
                    headers,
                    body_size,
                    post_data,
                },
                request_body_truncated: truncated,
                response: None,
                response_body: Vec::new(),
                response_body_truncated: false,
                error: None,
                done: false,
            })),
        }
    }

    /// 记录响应头到达, body 稍后由 [`PendingHar::append_body`] 或 [`PendingHar::finish_body`] 补充.
    pub(crate) fn on_response(
        &self,
        status: reqwest::StatusCode,
        version: reqwest::Version,
        headers: &reqwest::header::HeaderMap,
    ) {
        self.set_response(
            status.as_u16(),
            status.canonical_reason().unwrap_or_default().to_string(),
            http_version_str(version),
            har_headers(headers),
            mime_of(headers),
        );
    }

    pub(crate) fn on_response_hyper(
        &self,
        status: hyper::StatusCode,
        headers: &hyper::HeaderMap,
    ) {
        self.set_response(
            status.as_u16(),
            status.canonical_reason().unwrap_or_default().to_string(),
            "HTTP/1.1".to_string(),
            har_headers_hyper(headers),
            headers
                .get(hyper::header::CONTENT_TYPE)
                .and_then(|value| value.to_str().ok())
                .unwrap_or_default()
                .to_string(),
        );
    }

    fn set_response(
        &self,
        status: u16,
        status_text: String,
        http_version: String,
        headers: Vec<HarHeader>,
        mime_type: String,
    ) {
        let Ok(mut state) = self.state.lock() else {
            return;
        };
        state.response = Some(HarResponse {
            status,
            status_text,
            http_version,
            headers_size: -1,
            headers,
            body_size: -1,
            content: HarContent {
                size: -1,
                mime_type,
                text: None,
            },
            redirect_url: String::new(),
        });
    }

    /// 流式响应逐块累计响应体.
    pub(crate) fn append_body(&self, chunk: &[u8]) {
        let Ok(mut state) = self.state.lock() else {
            return;
        };
        if state.response_body.len() < MAX_BODY_BYTES {
            let remaining = MAX_BODY_BYTES - state.response_body.len();
            let take = chunk.len().min(remaining);
            state.response_body.extend_from_slice(&chunk[..take]);
            if take < chunk.len() {
                state.response_body_truncated = true;
            }
        } else {
            state.response_body_truncated = true;
        }
    }

    /// 记录完整响应体并落盘 (非流式响应).
    pub(crate) fn finish_body(self, body: &[u8]) {
        let Ok(mut state) = self.state.lock() else {
            return;
        };
        if body.len() > MAX_BODY_BYTES {
            state.response_body = body[..MAX_BODY_BYTES].to_vec();
            state.response_body_truncated = true;
        } else {
            state.response_body = body.to_vec();
        }
        state.done = true;
        write_locked(&mut state);
    }

    /// 请求失败 (未收到响应) 时落盘.
    pub(crate) fn fail(self, error: impl std::fmt::Display) {
        let Ok(mut state) = self.state.lock() else {
            return;
        };
        state.error = Some(error.to_string());
        state.done = true;
        write_locked(&mut state);
    }

    /// 流式响应自然结束后落盘.
    pub(crate) fn finish(self) {
        let Ok(mut state) = self.state.lock() else {
            return;
        };
        state.done = true;
        write_locked(&mut state);
    }
}

impl Drop for PendingHar {
    fn drop(&mut self) {
        let Ok(mut state) = self.state.lock() else {
            return;
        };
        if state.done {
            return;
        }
        // 请求被提前丢弃 (例如流式请求被用户终止), 写入已捕获的部分.
        state.done = true;
        if state.response.is_none() && state.error.is_none() {
            state.error = Some("request cancelled before response".to_string());
        }
        write_locked(&mut state);
    }
}

fn write_locked(state: &mut PendingHarState) {
    let time = state.started.elapsed().as_millis() as u64;
    let mut response = state.response.take().unwrap_or(HarResponse {
        status: 0,
        status_text: String::new(),
        http_version: "HTTP/1.1".to_string(),
        headers_size: -1,
        headers: Vec::new(),
        body_size: -1,
        content: HarContent {
            size: -1,
            mime_type: String::new(),
            text: None,
        },
        redirect_url: String::new(),
    });
    let body_size = i64::try_from(state.response_body.len()).unwrap_or(-1);
    response.body_size = body_size;
    response.content.size = body_size;
    response.content.text =
        final_body_text(is_sensitive_url(&state.request.url), &state.response_body);
    let entry = HarEntry {
        started_date_time: state.started_at.clone(),
        time,
        request: HarRequest {
            method: state.request.method.clone(),
            url: state.request.url.clone(),
            http_version: state.request.http_version.clone(),
            headers: state.request.headers.clone(),
            headers_size: state.request.headers_size,
            body_size: state.request.body_size,
            post_data: state
                .request
                .post_data
                .clone()
                .map(|data| HarPostData {
                    mime_type: data.mime_type,
                    text: if is_sensitive_url(&state.request.url) {
                        REDACTED_TEXT.to_string()
                    } else {
                        data.text
                    },
                }),
        },
        response,
        cache: serde_json::json!({}),
        timings: HarTimings {
            send: 0,
            wait: time,
            receive: 0,
        },
        error: state.error.clone(),
        truncated: (state.request_body_truncated || state.response_body_truncated)
            .then_some(true),
    };
    write_entry(&entry);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn oauth_bodies_are_redacted() {
        assert!(is_sensitive_url("https://auth.openai.com/oauth/token"));
        assert!(is_sensitive_url("https://auth.openai.com/api/accounts/deviceauth/usercode"));
        assert!(!is_sensitive_url("https://api.deepseek.com/user/balance"));
        assert!(!is_sensitive_url("https://chatgpt.com/backend-api/codex/responses"));
        assert_eq!(
            final_body_text(true, b"grant_type=refresh_token&refresh_token=secret"),
            Some(REDACTED_TEXT.to_string())
        );
        assert_eq!(
            final_body_text(false, b"{\"ok\":true}"),
            Some("{\"ok\":true}".to_string())
        );
        assert_eq!(final_body_text(true, b""), None);
    }

    fn test_writer(path: PathBuf, max_files: usize) -> HarFileWriter {
        HarFileWriter::new(
            path,
            HarRotation {
                max_size_bytes: 1024 * 1024,
                max_files,
            },
        )
    }

    fn sample_entry(url: &str) -> String {
        serde_json::to_string(&serde_json::json!({
            "startedDateTime": "2026-09-08T12:00:00+08:00",
            "time": 12,
            "request": {"method": "GET", "url": url, "httpVersion": "HTTP/1.1",
                "headers": [], "headersSize": -1, "bodySize": 0},
            "response": {"status": 200, "statusText": "OK", "httpVersion": "HTTP/1.1",
                "headers": [], "headersSize": -1, "bodySize": 0,
                "content": {"size": 0, "mimeType": ""}, "redirectURL": ""},
            "cache": {},
            "timings": {"send": 0, "wait": 12, "receive": 0}
        }))
        .unwrap()
    }

    fn parse_log(path: &Path) -> serde_json::Value {
        let content = fs::read_to_string(path).unwrap();
        serde_json::from_str(&content)
            .unwrap_or_else(|err| panic!("har file is not valid json: {err}\n{content}"))
    }

    #[test]
    fn appends_entries_and_stays_valid_json() {
        let path = std::env::temp_dir().join(format!("cs-har-{}.har", uuid::Uuid::new_v4()));
        let mut writer = test_writer(path.clone(), 3);
        writer.append_entry(&sample_entry("https://a.example/models")).unwrap();
        writer.append_entry(&sample_entry("https://b.example/user/balance")).unwrap();

        let log = parse_log(&path);
        assert_eq!(log["log"]["version"], "1.2");
        let entries = log["log"]["entries"].as_array().unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0]["request"]["url"], "https://a.example/models");
        assert_eq!(entries[1]["request"]["url"], "https://b.example/user/balance");
        fs::remove_file(&path).ok();
    }

    #[test]
    fn reopens_existing_har_and_appends() {
        let path = std::env::temp_dir().join(format!("cs-har-{}.har", uuid::Uuid::new_v4()));
        {
            let mut writer = test_writer(path.clone(), 3);
            writer.append_entry(&sample_entry("https://a.example/first")).unwrap();
        }
        let mut writer = test_writer(path.clone(), 3);
        writer.append_entry(&sample_entry("https://a.example/second")).unwrap();

        let entries = parse_log(&path)["log"]["entries"].as_array().unwrap().clone();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0]["request"]["url"], "https://a.example/first");
        assert_eq!(entries[1]["request"]["url"], "https://a.example/second");
        fs::remove_file(&path).ok();
    }

    #[test]
    fn repairs_broken_tail_into_rotated_backup() {
        let path = std::env::temp_dir().join(format!("cs-har-{}.har", uuid::Uuid::new_v4()));
        // 模拟上次会话写了一半崩溃: entries 未闭合, 最后一条 json 残缺.
        fs::write(
            &path,
            format!("{}{}broken", har_header(), sample_entry("https://a.example/old")),
        )
        .unwrap();

        let mut writer = test_writer(path.clone(), 3);
        writer.append_entry(&sample_entry("https://a.example/new")).unwrap();

        // 新 entry 写入主文件.
        let entries = parse_log(&path)["log"]["entries"].as_array().unwrap().clone();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0]["request"]["url"], "https://a.example/new");
        // 损坏会话原样轮转保留, 内容可人工检索.
        let backup = fs::read_to_string(writer.filename_for(1)).unwrap();
        assert!(backup.contains("https://a.example/old"));
        assert!(backup.contains("broken"));
        fs::remove_file(&path).ok();
        fs::remove_file(writer.filename_for(1)).ok();
    }
}
