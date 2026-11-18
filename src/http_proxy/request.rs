//! Reads bounded HTTP headers and prepares CONNECT or origin-form requests.
use http::uri::{Authority, Uri};
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio_util::bytes::Bytes;

pub(super) const MAX_HEADERS: usize = 16 * 1024;

pub(super) struct Request {
    pub(super) destination: String,
    pub(super) connect: bool,
    pub(super) initial: Bytes,
}

pub(super) struct ProxyError {
    pub(super) status: &'static str,
    pub(super) message: String,
}

impl ProxyError {
    fn bad_request(message: impl Into<String>) -> Self {
        Self {
            status: "400 Bad Request",
            message: message.into(),
        }
    }
}

pub(super) async fn read_request<R: AsyncRead + Unpin>(
    socket: &mut R,
) -> Result<Request, ProxyError> {
    let mut buffer = Vec::with_capacity(4096);
    let header_end = loop {
        if let Some(position) = buffer.windows(4).position(|window| window == b"\r\n\r\n") {
            break position + 4;
        }
        if buffer.len() == MAX_HEADERS {
            return Err(ProxyError {
                status: "431 Request Header Fields Too Large",
                message: "proxy headers exceed 16384 bytes".into(),
            });
        }
        let mut chunk = [0; 4096];
        let capacity = chunk.len().min(MAX_HEADERS - buffer.len());
        let size = socket
            .read(&mut chunk[..capacity])
            .await
            .map_err(|error| ProxyError::bad_request(error.to_string()))?;
        if size == 0 {
            return Err(ProxyError::bad_request("incomplete proxy headers"));
        }
        buffer.extend_from_slice(&chunk[..size]);
    };
    parse_request(&buffer, header_end)
}

fn authority(value: &str, require_port: bool) -> Result<String, ProxyError> {
    let parsed: Authority = value
        .parse()
        .map_err(|_| ProxyError::bad_request("invalid destination authority"))?;
    if parsed.host().is_empty()
        || value.contains('@')
        || value.len() != parsed.host().len() && parsed.port_u16().is_none()
        || parsed.port_u16() == Some(0)
        || require_port && parsed.port_u16().is_none()
    {
        return Err(ProxyError::bad_request("invalid destination host or port"));
    }
    Ok(if parsed.port().is_none() {
        format!("{parsed}:80")
    } else {
        parsed.to_string()
    })
}

fn parse_request(buffer: &[u8], header_end: usize) -> Result<Request, ProxyError> {
    let mut headers = [httparse::EMPTY_HEADER; 64];
    let mut request = httparse::Request::new(&mut headers);
    match request.parse(&buffer[..header_end]) {
        Ok(httparse::Status::Complete(_)) => {}
        _ => return Err(ProxyError::bad_request("invalid HTTP request headers")),
    }
    let method = request
        .method
        .ok_or_else(|| ProxyError::bad_request("missing method"))?;
    let target = request
        .path
        .ok_or_else(|| ProxyError::bad_request("missing request target"))?;
    if method == "CONNECT" {
        return Ok(Request {
            destination: authority(target, true)?,
            connect: true,
            // A browser may coalesce the first TLS bytes with CONNECT headers.
            initial: Bytes::copy_from_slice(&buffer[header_end..]),
        });
    }

    let uri: Uri = target
        .parse()
        .map_err(|_| ProxyError::bad_request("invalid request URI"))?;
    let (destination, host, path) = if let Some(scheme) = uri.scheme_str() {
        if scheme != "http" {
            return Err(ProxyError::bad_request(
                "use CONNECT for HTTPS destinations",
            ));
        }
        let host = uri
            .authority()
            .ok_or_else(|| ProxyError::bad_request("missing URI authority"))?
            .as_str();
        (
            authority(host, false)?,
            host,
            uri.path_and_query().map_or("/", |path| path.as_str()),
        )
    } else {
        if !target.starts_with('/') && target != "*" {
            return Err(ProxyError::bad_request(
                "expected an absolute HTTP URI or origin path",
            ));
        }
        let hosts: Vec<_> = request
            .headers
            .iter()
            .filter(|header| header.name.eq_ignore_ascii_case("host"))
            .collect();
        if hosts.len() != 1 {
            return Err(ProxyError::bad_request("expected exactly one Host header"));
        }
        let host = std::str::from_utf8(hosts[0].value)
            .map_err(|_| ProxyError::bad_request("invalid Host header"))?;
        (authority(host, false)?, host, target)
    };
    let path = if path.starts_with('?') {
        format!("/{path}")
    } else {
        path.to_string()
    };

    // Plain HTTP uses one request per connection. Streaming the body directly
    // supports large uploads, chunked framing, and Expect: 100-continue.
    let mut initial = format!(
        "{method} {path} HTTP/1.{}\r\nHost: {host}\r\n",
        request.version.unwrap_or(1)
    )
    .into_bytes();
    for header in request.headers.iter() {
        if [
            "host",
            "connection",
            "proxy-connection",
            "proxy-authorization",
        ]
        .iter()
        .any(|name| header.name.eq_ignore_ascii_case(name))
        {
            continue;
        }
        initial.extend_from_slice(header.name.as_bytes());
        initial.extend_from_slice(b": ");
        initial.extend_from_slice(header.value);
        initial.extend_from_slice(b"\r\n");
    }
    initial.extend_from_slice(b"Connection: close\r\n\r\n");
    initial.extend_from_slice(&buffer[header_end..]);
    Ok(Request {
        destination,
        connect: false,
        initial: initial.into(),
    })
}

#[cfg(test)]
mod tests;
