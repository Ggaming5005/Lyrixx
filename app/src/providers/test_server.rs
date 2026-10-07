//! A tiny HTTP server for testing the online providers against canned answers.

use serde_json::Value;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

/// One request the mock server received.
#[derive(Debug, Clone)]
pub(crate) struct Seen {
    pub path: String,
    /// Decoded query pairs, in order.
    pub query: Vec<(String, String)>,
    /// The raw request target (path and encoded query).
    pub target: String,
    /// Header names lowercased.
    pub headers: Vec<(String, String)>,
}

impl Seen {
    pub fn param(&self, name: &str) -> Option<&str> {
        self.query
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    }

    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    }
}

pub(crate) enum Reply {
    Json(u16, String),
    /// Bytes written as they are, then the connection is closed.
    Raw(Vec<u8>),
    /// Read the request, then never answer.
    Hang,
}

pub(crate) fn ok(body: Value) -> Reply {
    Reply::Json(200, body.to_string())
}

type Handler = dyn Fn(&Seen) -> Reply + Send + Sync;

pub(crate) struct MockServer {
    /// `http://127.0.0.1:<port>`.
    pub base: String,
    seen: Arc<Mutex<Vec<Seen>>>,
    task: tokio::task::JoinHandle<()>,
}

impl MockServer {
    pub async fn start(handler: impl Fn(&Seen) -> Reply + Send + Sync + 'static) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let handler: Arc<Handler> = Arc::new(handler);
        let task = tokio::spawn({
            let seen = Arc::clone(&seen);
            async move {
                while let Ok((stream, _)) = listener.accept().await {
                    tokio::spawn(serve(stream, Arc::clone(&seen), Arc::clone(&handler)));
                }
            }
        });
        Self {
            base: format!("http://{addr}"),
            seen,
            task,
        }
    }

    pub fn seen(&self) -> Vec<Seen> {
        self.seen.lock().unwrap().clone()
    }

    pub fn paths(&self) -> Vec<String> {
        self.seen().into_iter().map(|s| s.path).collect()
    }
}

impl Drop for MockServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn serve(mut stream: TcpStream, seen: Arc<Mutex<Vec<Seen>>>, handler: Arc<Handler>) {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    while !buf.windows(4).any(|w| w == b"\r\n\r\n") {
        match stream.read(&mut chunk).await {
            Ok(0) | Err(_) => return,
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
        }
    }
    let head = String::from_utf8_lossy(&buf).into_owned();
    let mut lines = head.split("\r\n");
    let request_line = lines.next().unwrap_or_default();
    let target = request_line
        .split(' ')
        .nth(1)
        .unwrap_or_default()
        .to_string();
    let headers = lines
        .take_while(|line| !line.is_empty())
        .filter_map(|line| line.split_once(':'))
        .map(|(k, v)| (k.trim().to_ascii_lowercase(), v.trim().to_string()))
        .collect();
    let absolute = if target.starts_with("http") {
        target.clone()
    } else {
        format!("http://mock{target}")
    };
    let url = reqwest::Url::parse(&absolute).unwrap();
    let request = Seen {
        path: url.path().to_string(),
        query: url
            .query_pairs()
            .map(|(k, v)| (k.into_owned(), v.into_owned()))
            .collect(),
        target,
        headers,
    };
    seen.lock().unwrap().push(request.clone());
    match handler(&request) {
        Reply::Json(status, body) => {
            let reason = match status {
                200 => "OK",
                404 => "Not Found",
                500 => "Internal Server Error",
                503 => "Service Unavailable",
                _ => "Whatever",
            };
            let response = format!(
                "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = stream.write_all(response.as_bytes()).await;
            let _ = stream.shutdown().await;
        }
        Reply::Raw(bytes) => {
            let _ = stream.write_all(&bytes).await;
            let _ = stream.shutdown().await;
        }
        Reply::Hang => {
            tokio::time::sleep(Duration::from_secs(3600)).await;
        }
    }
}
