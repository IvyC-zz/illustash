//! Native HTTP entry, request context and shared response/security encoding.
use crate::Result;
use std::{
    collections::HashMap,
    io::{self, Read, Write},
    path::PathBuf,
};
use tiny_http::{Header, Request, Response, StatusCode};

/// Request-local context from the native session. Never persisted as a readiness file.
#[derive(Debug)]
pub struct LibraryContext {
    pub db: PathBuf,
    pub generation: String,
    pub library: Option<PathBuf>,
    pub tags: PathBuf,
    pub source: Option<PathBuf>,
    pub token: Option<String>,
    pub read_only: Option<String>,
}
fn accepts_gzip(value: &str) -> bool {
    let mut accepted = HashMap::new();
    for part in value.split(',') {
        let lowered = part.trim().to_lowercase();
        let mut pieces = lowered.split(';');
        let name = pieces.next().unwrap_or("").trim();
        let q =
            pieces.find_map(|p| p.trim().strip_prefix("q=")).map(|v| v.parse::<f64>().unwrap_or(0.0)).unwrap_or(1.0);
        accepted.insert(name.to_owned(), q);
    }
    accepted.get("gzip").or_else(|| accepted.get("*")).copied().unwrap_or(0.0) > 0.0
}

/// Identical JSON serialization, compression and security headers for both application hosts.
pub(crate) fn json_response(
    request: &Request,
    payload: &crate::http_json::Json,
    status: u16,
    route: &str,
) -> io::Result<Response<Box<dyn Read + Send>>> {
    let accepted =
        request.headers().iter().find(|h| h.field.equiv("Accept-Encoding")).map(|h| h.value.as_str()).unwrap_or("");
    let mut bytes = payload.dump().into_bytes();
    let mut encoding = None;
    if bytes.len() >= 1024 && accepts_gzip(accepted) {
        let mtime =
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs() as u32;
        let mut gz =
            flate2::GzBuilder::new().mtime(mtime).operating_system(255).write(Vec::new(), flate2::Compression::new(1));
        gz.write_all(&bytes)?;
        let compressed = gz.finish()?;
        if compressed.len() < bytes.len() {
            bytes = compressed;
            encoding = Some("gzip");
        }
    }
    let n = bytes.len();
    let mut headers:Vec<Header>=[("Content-Type","application/json; charset=utf-8"),("Cache-Control","no-store"),("Vary","Accept-Encoding"),("X-Content-Type-Options","nosniff"),("Referrer-Policy","no-referrer"),("Content-Security-Policy","default-src 'self'; img-src 'self' data:; style-src 'self' 'unsafe-inline'; script-src 'self'; connect-src 'self'; frame-ancestors 'none'; base-uri 'none'; form-action 'none'"),("X-Illustash-Route",route)].into_iter().map(|(k,v)|Header::from_bytes(k,v).expect("http internal invariant")).collect();
    if let Some(encoding) = encoding {
        headers.push(Header::from_bytes("Content-Encoding", encoding).expect("http internal invariant"));
    }
    Ok(Response::new(
        StatusCode(status),
        headers,
        Box::new(io::Cursor::new(bytes)) as Box<dyn Read + Send>,
        Some(n),
        None,
    )
    .with_chunked_threshold(usize::MAX))
}

pub(crate) use crate::platform_process::{install_signals, request_stop, stopping};

pub fn run(args: &[String]) -> Result<u8> {
    crate::app_http::run(args)
}
