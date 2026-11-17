//! 仅供测试的读宿主（#9）：为状态文件里指定的图库上下文提供核心读路由（`http_read::read`），
//! 响应编码与应用宿主共用。读路由语料测试借此在任意状态的合成数据库上把这些字节与固定期望比较，
//! 包括应用宿主会先升级的旧版数据库。
//! 只在显式开启 `test-read-host` feature 时构建；从不进入发布的 CLI。
use crate::http::LibraryContext;
use crate::http_json::Json;
use crate::{Error, Result};
use std::io::{self, Read};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::thread;
use tiny_http::{Header, Request, Response, Server, StatusCode};

/// 状态文件：`{"db", "generation", "source"?, "library"?, "tags"?, "token"?, "read_only"?}`，
/// 每个请求都重新读取。
pub fn current_library(state: &Path) -> Result<LibraryContext> {
    let value: serde_json::Value = serde_json::from_slice(&std::fs::read(state)?)
        .map_err(|e| Error::Invalid(format!("HTTP 图库状态损坏：{e}")))?;
    let field = |key: &str| {
        value[key].as_str().filter(|v| !v.is_empty()).ok_or_else(|| Error::Invalid(format!("HTTP 图库状态缺少 {key}")))
    };
    Ok(LibraryContext {
        examples: None,
        db: PathBuf::from(field("db")?),
        source: value["source"].as_str().map(PathBuf::from),
        token: value["token"].as_str().map(str::to_owned),
        read_only: value["read_only"].as_str().map(str::to_owned),
        generation: field("generation")?.into(),
        library: value["library"].as_str().map(PathBuf::from),
        tags: value["tags"].as_str().map(PathBuf::from).unwrap_or_default(),
    })
}

fn refused(status: u16, message: &str, route: &str) -> Response<Box<dyn Read + Send>> {
    let body = Json::object([("error", message.into())]).dump_reply().into_bytes();
    let n = body.len();
    Response::new(
        StatusCode(status),
        vec![
            Header::from_bytes("Content-Type", "application/json; charset=utf-8").expect("static header"),
            Header::from_bytes("X-Illustash-Route", route).expect("static header"),
        ],
        Box::new(io::Cursor::new(body)) as Box<dyn Read + Send>,
        Some(n),
        None,
    )
}

fn handle(request: &Request, state: &Path, origin: &str) -> io::Result<Response<Box<dyn Read + Send>>> {
    let header = |name: &str| {
        request
            .headers()
            .iter()
            .find(|h| h.field.as_str().as_str().eq_ignore_ascii_case(name))
            .map(|h| h.value.as_str())
    };
    if request.method().as_str() != "GET"
        || header("Host") != Some(origin.trim_start_matches("http://"))
        || header("Origin").is_some_and(|v| v != origin)
    {
        return Ok(refused(403, "测试读宿主只服务本机 GET 读路由", "refused"));
    }
    let Ok(context) = current_library(state) else {
        return Ok(refused(503, "测试读宿主没有图库状态", "unavailable"));
    };
    let (payload, status) = match crate::http_read::read(&context, request.url()) {
        Ok(Some(v)) => v,
        Ok(None) => return Ok(refused(404, "不是核心读路由", "unserved")),
        Err(e) => {
            eprintln!("HTTP core: {e}");
            (Json::object([("error", "读取失败，请重试；详情请查看启动终端".into())]), 500)
        }
    };
    // 状态被重新发布（换了图库或上下文）时，与之并发的读取作废。
    if !current_library(state).is_ok_and(|c| c.generation == context.generation) {
        return Ok(refused(503, "图库状态在读取期间改变", "unavailable"));
    }
    crate::http::json_response(request, &payload, status, "core")
}

/// `read-http --port PORT --state FILE`；一直运行到进程被停止。
pub fn run(args: &[String]) -> Result<u8> {
    let mut port = None;
    let mut state = None;
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        let value = args.next().ok_or_else(|| Error::Invalid(format!("{arg} 缺少值")))?;
        match arg.as_str() {
            "--port" => port = value.parse::<u16>().ok().filter(|n| *n > 0),
            "--state" => state = Some(PathBuf::from(value)),
            _ => return Err(Error::Invalid(format!("测试读宿主不支持参数：{arg}"))),
        }
    }
    let port = port.ok_or_else(|| Error::Invalid("需要 --port 1–65535".into()))?;
    let state = Arc::new(state.ok_or_else(|| Error::Invalid("需要 --state".into()))?);
    let origin = Arc::new(format!("http://127.0.0.1:{port}"));
    let listener = TcpListener::bind(("127.0.0.1", port))?;
    let server = Server::from_listener(listener, None).map_err(|e| Error::Invalid(e.to_string()))?;
    println!("测试读宿主已启动：{origin}");
    // 每个请求一个 worker 线程，与应用宿主相同。
    for request in server.incoming_requests() {
        let (state, origin) = (state.clone(), origin.clone());
        thread::spawn(move || {
            let response = handle(&request, &state, &origin);
            match response {
                Ok(response) => {
                    let _ = request.respond(response);
                }
                Err(e) => eprintln!("HTTP read: {e}"),
            }
        });
    }
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn library_context_is_read_anew() {
        let dir = std::env::temp_dir().join(format!("illustash-read-harness-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let state = dir.join("state.json");
        assert!(current_library(&state).is_err());
        for (db, generation) in [("/tmp/a.sqlite", "one"), ("/tmp/b.sqlite", "two")] {
            std::fs::write(&state, serde_json::json!({"db": db, "generation": generation}).to_string()).unwrap();
            let current = current_library(&state).unwrap();
            assert_eq!(current.db, PathBuf::from(db));
            assert_eq!(current.generation, generation);
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
