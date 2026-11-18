use super::*;

#[test]
fn parses_destinations_rewrites_http_and_preserves_read_ahead() {
    for (target, host, destination, path) in [
        (
            "http://example.com",
            "ignored.example",
            "example.com:80",
            "/",
        ),
        (
            "http://example.com:8080/a?q=1",
            "ignored.example",
            "example.com:8080",
            "/a?q=1",
        ),
        (
            "http://example.com?q=1",
            "example.com",
            "example.com:80",
            "/?q=1",
        ),
        (
            "http://[::1]:8080/path",
            "[::1]:8080",
            "[::1]:8080",
            "/path",
        ),
        ("/origin", "example.com", "example.com:80", "/origin"),
        ("/origin", "[::1]", "[::1]:80", "/origin"),
    ] {
        let header = format!(
            "POST {target} HTTP/1.1\r\nHost: {host}\r\nContent-Length: 4\r\nProxy-Connection: keep-alive\r\nProxy-Authorization: Basic secret\r\n\r\n"
        );
        let mut buffer = header.as_bytes().to_vec();
        buffer.extend_from_slice(b"body");
        let request = parse_request(&buffer, header.len())
            .unwrap_or_else(|error| panic!("{}", error.message));
        assert_eq!(request.destination, destination);
        let initial = std::str::from_utf8(&request.initial).unwrap();
        assert!(
            initial.starts_with(&format!("POST {path} HTTP/1.1\r\n")),
            "{initial}"
        );
        assert!(!initial.contains("Proxy-"));
        assert!(initial.ends_with("Connection: close\r\n\r\nbody"));
        assert!(!request.connect);
    }
    for destination in ["example.com:443", "[::1]:443"] {
        let header = format!("CONNECT {destination} HTTP/1.1\r\n\r\n");
        let mut buffer = header.as_bytes().to_vec();
        buffer.extend_from_slice(b"\x16\x03\x03\0\x04data");
        let request = parse_request(&buffer, header.len())
            .unwrap_or_else(|error| panic!("{}", error.message));
        assert_eq!(request.destination, destination);
        assert!(request.connect);
        assert_eq!(&request.initial[..], b"\x16\x03\x03\0\x04data");
    }
    for host in [
        "example.com:65536",
        "example.com:abc",
        "example.com:",
        "example.com:0",
        "user@example.com",
        "::1",
    ] {
        assert!(authority(host, false).is_err(), "accepted {host}");
    }
    for request in [
        "CONNECT example.com HTTP/1.1\r\n\r\n",
        "GET / HTTP/1.1\r\n\r\n",
        "GET / HTTP/1.1\r\nHost: a\r\nHost: b\r\n\r\n",
        "GET https://example.com/ HTTP/1.1\r\n\r\n",
    ] {
        assert!(
            parse_request(request.as_bytes(), request.len()).is_err(),
            "accepted {request}"
        );
    }
}
