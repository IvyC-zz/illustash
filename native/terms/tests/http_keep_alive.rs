#![allow(clippy::unwrap_used)]

use std::{
    io::{BufRead, BufReader, Read, Write},
    net::{TcpListener, TcpStream},
    time::Duration,
};
use tiny_http::{Limits, Response, Server};

#[path = "common/wait.rs"]
mod wait;

#[test]
fn concurrent_connections_keep_serving_before_keep_alive_expires() {
    // 与 app_http 的默认限制一致，不用测试环境变量缩短服务端超时。
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let address = listener.local_addr().unwrap();
    let server = Server::from_listener_with_limits(
        listener,
        None,
        Limits {
            read_timeout: Some(Duration::from_secs(30)),
            write_timeout: Some(Duration::from_secs(30)),
            max_connections: Some(64),
        },
    )
    .unwrap();
    // 连接数固定且小于入口上限；先全部连接，再发请求，覆盖真实 accept 和任务池。
    let mut connections: Vec<_> = (0..8)
        .map(|_| {
            let stream = TcpStream::connect(address).unwrap();
            stream.set_read_timeout(Some(wait::limit())).unwrap();
            stream.set_write_timeout(Some(wait::limit())).unwrap();
            BufReader::new(stream)
        })
        .collect();

    for round in 0..3 {
        for (index, connection) in connections.iter_mut().enumerate() {
            let persistence = if round == 2 { "close" } else { "keep-alive" };
            write!(
                connection.get_mut(),
                "GET /synthetic/{round}/{index} HTTP/1.1\r\nHost: {address}\r\nConnection: {persistence}\r\n\r\n"
            )
            .unwrap();
        }
        for _ in 0..connections.len() {
            let request = server
                .recv_timeout(wait::limit())
                .unwrap()
                .expect("其他连接仍保持 keep-alive 时，后续连接必须得到服务");
            let body = request.url().to_owned();
            request.respond(Response::from_string(body)).unwrap();
        }
        // 读完每个响应再复用原连接，证明通过不依赖关闭先前连接或等待读超时。
        for (index, connection) in connections.iter_mut().enumerate() {
            let mut line = String::new();
            connection.read_line(&mut line).unwrap();
            assert!(line.starts_with("HTTP/1.1 200 "), "{line}");
            let mut length = None;
            loop {
                line.clear();
                assert_ne!(connection.read_line(&mut line).unwrap(), 0, "响应头提前结束");
                if line == "\r\n" {
                    break;
                }
                if let Some((name, value)) = line.split_once(':') {
                    if name.eq_ignore_ascii_case("content-length") {
                        length = Some(value.trim().parse::<usize>().unwrap());
                    }
                }
            }
            let expected = format!("/synthetic/{round}/{index}");
            assert_eq!(length, Some(expected.len()));
            let mut body = vec![0; expected.len()];
            connection.read_exact(&mut body).unwrap();
            assert_eq!(body, expected.as_bytes());
        }
    }
}
