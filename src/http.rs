//! A deliberately small HTTP/1.1 server for a localhost-only GUI.
//!
//! One thread per connection, `Connection: close` on every response, and hard
//! caps on header and body size. That is all a single-user local app needs,
//! and it keeps the whole server auditable in one sitting.

use std::collections::BTreeMap;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

const MAX_HEADER_BYTES: usize = 64 * 1024;
const MAX_BODY_BYTES: usize = 8 * 1024 * 1024;
const MAX_CONNECTIONS: usize = 64;

pub struct Request {
    pub method: String,
    pub path: String,
    pub query: BTreeMap<String, String>,
    pub headers: BTreeMap<String, String>,
    pub body: Vec<u8>,
}

impl Request {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .get(&name.to_ascii_lowercase())
            .map(String::as_str)
    }
    pub fn cookie(&self, name: &str) -> Option<&str> {
        self.header("cookie")?.split(';').find_map(|pair| {
            let (k, v) = pair.trim().split_once('=')?;
            (k == name).then_some(v)
        })
    }
    pub fn body_text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }
}

pub struct Response {
    pub status: u16,
    pub content_type: &'static str,
    pub body: Vec<u8>,
    pub headers: Vec<(String, String)>,
}

impl Response {
    pub fn new(status: u16, content_type: &'static str, body: impl Into<Vec<u8>>) -> Response {
        Response {
            status,
            content_type,
            body: body.into(),
            headers: Vec::new(),
        }
    }
    pub fn json(status: u16, value: &crate::json::Json) -> Response {
        Response::new(status, "application/json; charset=utf-8", value.to_string())
    }
    pub fn error(status: u16, message: &str) -> Response {
        Response::json(
            status,
            &crate::json::Json::obj(vec![("error", crate::json::Json::s(message))]),
        )
    }
    pub fn text(status: u16, body: impl Into<Vec<u8>>) -> Response {
        Response::new(status, "text/plain; charset=utf-8", body)
    }
    pub fn redirect(location: &str) -> Response {
        let mut r = Response::new(303, "text/plain; charset=utf-8", "");
        r.headers.push(("Location".into(), location.into()));
        r
    }
    pub fn with_header(mut self, name: &str, value: &str) -> Response {
        self.headers.push((name.to_string(), value.to_string()));
        self
    }
}

/// A handler either answers in full, or takes over the socket to stream.
pub enum Reply {
    Full(Response),
    Stream(Box<dyn FnOnce(TcpStream) + Send>),
}

fn reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        303 => "See Other",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        409 => "Conflict",
        413 => "Payload Too Large",
        _ => "Internal Server Error",
    }
}

pub fn write_response(stream: &mut TcpStream, r: &Response) -> io::Result<()> {
    let mut head = format!(
        "HTTP/1.1 {} {}\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\n\
         Cache-Control: no-store\r\nX-Content-Type-Options: nosniff\r\nReferrer-Policy: no-referrer\r\n\
         X-Frame-Options: DENY\r\n\
         Content-Security-Policy: default-src 'self'; frame-ancestors 'none'; base-uri 'none'; form-action 'self'\r\n",
        r.status,
        reason(r.status),
        r.content_type,
        r.body.len()
    );
    for (k, v) in &r.headers {
        // Header injection guard: values never carry line breaks.
        if !k.contains(['\r', '\n']) && !v.contains(['\r', '\n']) {
            head.push_str(&format!("{k}: {v}\r\n"));
        }
    }
    head.push_str("\r\n");
    stream.write_all(head.as_bytes())?;
    stream.write_all(&r.body)?;
    stream.flush()
}

/// Headers for a server-sent-events stream; the caller then writes events.
pub fn write_sse_head(stream: &mut TcpStream) -> io::Result<()> {
    stream.write_all(
        b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nCache-Control: no-store\r\n\
          Connection: close\r\nX-Content-Type-Options: nosniff\r\n\r\n",
    )?;
    stream.flush()
}

pub fn write_sse_event(stream: &mut TcpStream, event: &str, data: &str) -> io::Result<()> {
    let mut out = format!("event: {event}\n");
    for line in data.lines() {
        out.push_str("data: ");
        out.push_str(line);
        out.push('\n');
    }
    out.push('\n');
    stream.write_all(out.as_bytes())?;
    stream.flush()
}

pub fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => {
                let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or("");
                match u8::from_str_radix(hex, 16) {
                    Ok(b) => {
                        out.push(b);
                        i += 3;
                    }
                    Err(_) => {
                        out.push(b'%');
                        i += 1;
                    }
                }
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn parse_query(q: &str) -> BTreeMap<String, String> {
    let mut map = BTreeMap::new();
    for pair in q.split('&').filter(|p| !p.is_empty()) {
        let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
        map.insert(percent_decode(k), percent_decode(v));
    }
    map
}

/// Parse one request off the wire. Errors carry the status to answer with.
pub fn read_request(stream: &mut TcpStream) -> Result<Request, (u16, &'static str)> {
    let mut reader = BufReader::new(stream);
    let mut head = Vec::new();
    loop {
        let mut line = Vec::new();
        let n = reader
            .by_ref()
            .take((MAX_HEADER_BYTES - head.len()) as u64 + 1)
            .read_until(b'\n', &mut line)
            .map_err(|_| (400, "could not read request"))?;
        if n == 0 {
            return Err((400, "connection closed mid-request"));
        }
        head.extend_from_slice(&line);
        if head.len() > MAX_HEADER_BYTES {
            return Err((413, "request headers too large"));
        }
        if line == b"\r\n" || line == b"\n" {
            break;
        }
    }
    let head = String::from_utf8_lossy(&head);
    let mut lines = head.lines();
    let request_line = lines.next().ok_or((400, "empty request"))?;
    let mut parts = request_line.split_whitespace();
    let method = parts
        .next()
        .ok_or((400, "malformed request line"))?
        .to_string();
    let target = parts.next().ok_or((400, "malformed request line"))?;
    if !target.starts_with('/') {
        return Err((400, "only origin-form request targets are supported"));
    }
    let (raw_path, raw_query) = target.split_once('?').unwrap_or((target, ""));

    let mut headers = BTreeMap::new();
    for line in lines {
        if let Some((k, v)) = line.split_once(':') {
            headers.insert(k.trim().to_ascii_lowercase(), v.trim().to_string());
        }
    }

    let length = match headers.get("content-length") {
        Some(v) => v
            .parse::<usize>()
            .map_err(|_| (400, "bad content-length"))?,
        None => 0,
    };
    if length > MAX_BODY_BYTES {
        return Err((413, "request body too large"));
    }
    if headers
        .get("transfer-encoding")
        .is_some_and(|v| !v.eq_ignore_ascii_case("identity"))
    {
        return Err((400, "chunked request bodies are not supported"));
    }
    let mut body = vec![0u8; length];
    reader
        .read_exact(&mut body)
        .map_err(|_| (400, "request body shorter than content-length"))?;

    Ok(Request {
        method,
        path: percent_decode(raw_path),
        query: parse_query(raw_query),
        headers,
        body,
    })
}

/// Serve until the process exits. Each connection gets its own thread.
pub fn serve<H>(listener: TcpListener, handler: H) -> io::Result<()>
where
    H: Fn(&Request) -> Reply + Send + Sync + 'static,
{
    let handler = Arc::new(handler);
    let active = Arc::new(AtomicUsize::new(0));
    for conn in listener.incoming() {
        let Ok(mut stream) = conn else { continue };
        if active.load(Ordering::Relaxed) >= MAX_CONNECTIONS {
            let _ = write_response(&mut stream, &Response::error(503, "too many connections"));
            continue;
        }
        let handler = handler.clone();
        let active = active.clone();
        active.fetch_add(1, Ordering::Relaxed);
        thread::spawn(move || {
            let _ = stream.set_read_timeout(Some(Duration::from_secs(30)));
            let _ = stream.set_write_timeout(Some(Duration::from_secs(30)));
            match read_request(&mut stream) {
                Ok(req) => match handler(&req) {
                    Reply::Full(resp) => {
                        let _ = write_response(&mut stream, &resp);
                    }
                    Reply::Stream(f) => f(stream),
                },
                Err((status, msg)) => {
                    let _ = write_response(&mut stream, &Response::error(status, msg));
                }
            }
            active.fetch_sub(1, Ordering::Relaxed);
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percent_decoding() {
        assert_eq!(percent_decode("a%20b+c"), "a b c");
        assert_eq!(percent_decode("%2Fhome%2Fme"), "/home/me");
        assert_eq!(percent_decode("100%"), "100%");
        assert_eq!(percent_decode("%zz"), "%zz");
        assert_eq!(percent_decode("%e2%9c%93"), "✓");
    }

    #[test]
    fn query_parsing() {
        let q = parse_query("from=12&name=a%20b&flag");
        assert_eq!(q["from"], "12");
        assert_eq!(q["name"], "a b");
        assert_eq!(q["flag"], "");
    }

    fn roundtrip(raw: &[u8]) -> Result<Request, (u16, &'static str)> {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let raw = raw.to_vec();
        let client = thread::spawn(move || {
            let mut s = TcpStream::connect(addr).unwrap();
            s.write_all(&raw).unwrap();
            s.shutdown(std::net::Shutdown::Write).ok();
            thread::sleep(Duration::from_millis(50));
        });
        let (mut stream, _) = listener.accept().unwrap();
        let r = read_request(&mut stream);
        client.join().unwrap();
        r
    }

    #[test]
    fn parses_a_post_with_body_and_cookies() {
        let req = roundtrip(
            b"POST /api/build?x=1 HTTP/1.1\r\nHost: localhost:7878\r\nCookie: a=1; kt=secret\r\nContent-Length: 7\r\n\r\n{\"a\":1}",
        )
        .ok()
        .unwrap();
        assert_eq!(req.method, "POST");
        assert_eq!(req.path, "/api/build");
        assert_eq!(req.query["x"], "1");
        assert_eq!(req.header("HOST"), Some("localhost:7878"));
        assert_eq!(req.cookie("kt"), Some("secret"));
        assert_eq!(req.cookie("nope"), None);
        assert_eq!(req.body_text(), "{\"a\":1}");
    }

    #[test]
    fn rejects_oversized_and_malformed_requests() {
        let huge = format!(
            "POST / HTTP/1.1\r\nContent-Length: {}\r\n\r\n",
            MAX_BODY_BYTES + 1
        );
        assert_eq!(roundtrip(huge.as_bytes()).err().unwrap().0, 413);
        assert_eq!(
            roundtrip(b"GET http://evil/ HTTP/1.1\r\n\r\n")
                .err()
                .unwrap()
                .0,
            400
        );
        assert_eq!(
            roundtrip(b"POST / HTTP/1.1\r\nContent-Length: 50\r\n\r\nshort")
                .err()
                .unwrap()
                .0,
            400
        );
        let long_header = format!(
            "GET / HTTP/1.1\r\nX: {}\r\n\r\n",
            "a".repeat(MAX_HEADER_BYTES)
        );
        assert_eq!(roundtrip(long_header.as_bytes()).err().unwrap().0, 413);
    }
}
