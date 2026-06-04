//! 独立下载器对外使用的下载请求模型。
//!
//! 一个请求可以带多个候选地址，这样官方地址、镜像地址或重定向后的地址
//! 可以按固定顺序依次尝试，调用方不用自己写兜底逻辑。

use reqwest::Method;
use std::path::PathBuf;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DownloadProtocol {
    Http,
    Future(String),
}

#[derive(Clone, Debug)]
pub struct DownloadCandidate {
    pub url: String,
    pub label: Option<String>,
}

impl DownloadCandidate {
    pub fn new(url: impl Into<String>) -> Self {
        Self {
            url: url.into(),
            label: None,
        }
    }

    pub fn labeled(url: impl Into<String>, label: impl Into<String>) -> Self {
        Self {
            url: url.into(),
            label: Some(label.into()),
        }
    }
}

#[derive(Clone, Debug)]
pub struct DownloadRequest {
    pub id: String,
    pub protocol: DownloadProtocol,
    pub candidates: Vec<DownloadCandidate>,
    pub dest: PathBuf,
    pub method: Method,
    pub headers: Vec<(String, String)>,
    pub body: Option<Vec<u8>>,
    pub expected_size: Option<u64>,
    pub expected_sha1: Option<String>,
    pub allow_resume: bool,
}

impl DownloadRequest {
    pub fn new(
        id: impl Into<String>,
        candidates: Vec<DownloadCandidate>,
        dest: impl Into<PathBuf>,
    ) -> Self {
        Self {
            id: id.into(),
            protocol: DownloadProtocol::Http,
            candidates,
            dest: dest.into(),
            method: Method::GET,
            headers: Vec::new(),
            body: None,
            expected_size: None,
            expected_sha1: None,
            allow_resume: true,
        }
    }

    pub fn with_protocol(mut self, protocol: DownloadProtocol) -> Self {
        self.protocol = protocol;
        self
    }

    pub fn with_method(mut self, method: Method) -> Self {
        self.method = method;
        self
    }

    pub fn with_header(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.headers.push((name.into(), value.into()));
        self
    }

    pub fn with_body(mut self, body: impl Into<Vec<u8>>) -> Self {
        self.body = Some(body.into());
        self
    }

    pub fn with_expected_size(mut self, size: u64) -> Self {
        self.expected_size = Some(size);
        self
    }

    pub fn with_expected_sha1(mut self, sha1: impl Into<String>) -> Self {
        self.expected_sha1 = Some(sha1.into());
        self
    }

    pub fn without_resume(mut self) -> Self {
        self.allow_resume = false;
        self
    }
}
