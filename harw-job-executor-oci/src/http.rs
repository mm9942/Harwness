//! A minimal blocking HTTP/1.1 client for the engine's Unix socket.
//!
//! Only what the Docker-compatible API needs: one request per connection
//! (`Connection: close`), JSON bodies, `Content-Length`, chunked and
//! until-EOF response bodies. Every size is capped.

use std::io::{self, BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::Duration;

use crate::config::PeerPolicy;
use crate::error::OciError;

const MAX_LINE: u64 = 8 * 1024;
const MAX_HEADERS: usize = 64;
/// API version prefix understood by Docker and Podman's compat service.
pub(crate) const API: &str = "/v1.41";

/// A buffered response.
#[derive(Debug)]
pub(crate) struct Response {
    pub(crate) status: u16,
    pub(crate) body: Vec<u8>,
}

/// Connects and verifies the peer.
pub(crate) fn connect(
    socket: &Path,
    policy: PeerPolicy,
    timeout: Option<Duration>,
) -> Result<UnixStream, OciError> {
    let stream = UnixStream::connect(socket)
        .map_err(|e| OciError::Io(format!("connect {}: {e}", socket.display())))?;
    let cred = rustix::net::sockopt::socket_peercred(&stream)
        .map_err(|e| OciError::UntrustedSocket(format!("cannot read peer credentials: {e}")))?;
    let expected = match policy {
        PeerPolicy::SameUser => rustix::process::getuid().as_raw(),
        PeerPolicy::Uid(uid) => uid,
    };
    if cred.uid.as_raw() != expected {
        return Err(OciError::UntrustedSocket(format!(
            "socket peer runs as uid {} but uid {expected} is required",
            cred.uid.as_raw()
        )));
    }
    stream
        .set_read_timeout(timeout)
        .and_then(|()| stream.set_write_timeout(timeout))
        .map_err(|e| OciError::Io(e.to_string()))?;
    Ok(stream)
}

/// Body framing of a response.
enum Framing {
    Length(u64),
    Chunked { left: u64, done: bool },
    Eof,
}

/// A streaming response body.
pub(crate) struct Body {
    reader: BufReader<UnixStream>,
    framing: Framing,
}

impl Read for Body {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        match &mut self.framing {
            Framing::Eof => self.reader.read(buf),
            Framing::Length(left) => {
                if *left == 0 {
                    return Ok(0);
                }
                let take = usize::try_from(*left).unwrap_or(usize::MAX).min(buf.len());
                let n = self.reader.read(&mut buf[..take])?;
                if n == 0 {
                    return Err(io::ErrorKind::UnexpectedEof.into());
                }
                *left -= n as u64;
                Ok(n)
            }
            Framing::Chunked { left, done } => {
                if *done {
                    return Ok(0);
                }
                if *left == 0 {
                    let line = read_line(&mut self.reader)?;
                    let size = line.split(';').next().unwrap_or("").trim();
                    let size = u64::from_str_radix(size, 16).map_err(|_| {
                        io::Error::new(io::ErrorKind::InvalidData, "bad chunk size")
                    })?;
                    if size == 0 {
                        *done = true;
                        // Trailers are ignored; the connection is closed anyway.
                        return Ok(0);
                    }
                    *left = size;
                }
                let take = usize::try_from(*left).unwrap_or(usize::MAX).min(buf.len());
                let n = self.reader.read(&mut buf[..take])?;
                if n == 0 {
                    return Err(io::ErrorKind::UnexpectedEof.into());
                }
                *left -= n as u64;
                if *left == 0 {
                    let mut crlf = [0u8; 2];
                    self.reader.read_exact(&mut crlf)?;
                    if &crlf != b"\r\n" {
                        return Err(io::Error::new(io::ErrorKind::InvalidData, "bad chunk end"));
                    }
                }
                Ok(n)
            }
        }
    }
}

fn read_line<R: BufRead>(reader: &mut R) -> io::Result<String> {
    let mut line = Vec::new();
    let n = reader
        .by_ref()
        .take(MAX_LINE)
        .read_until(b'\n', &mut line)?;
    if n == 0 {
        return Err(io::ErrorKind::UnexpectedEof.into());
    }
    if line.last() != Some(&b'\n') {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "line too long"));
    }
    while matches!(line.last(), Some(b'\n' | b'\r')) {
        line.pop();
    }
    String::from_utf8(line).map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "non-utf8 line"))
}

/// Sends one request and returns the status plus a streaming body.
pub(crate) fn open(
    mut stream: UnixStream,
    method: &str,
    path: &str,
    body: Option<&[u8]>,
) -> Result<(u16, Body), OciError> {
    let mut head = format!("{method} {path} HTTP/1.1\r\nHost: engine\r\nConnection: close\r\n");
    match body {
        Some(body) => {
            head.push_str(&format!(
                "Content-Type: application/json\r\nContent-Length: {}\r\n\r\n",
                body.len()
            ));
        }
        None => head.push_str("Content-Length: 0\r\n\r\n"),
    }
    stream
        .write_all(head.as_bytes())
        .and_then(|()| body.map_or(Ok(()), |b| stream.write_all(b)))
        .and_then(|()| stream.flush())
        .map_err(|e| OciError::Io(format!("write request: {e}")))?;
    let mut reader = BufReader::new(stream);
    let io_err = |e: io::Error| OciError::Io(format!("read response: {e}"));
    let status_line = read_line(&mut reader).map_err(io_err)?;
    let mut parts = status_line.splitn(3, ' ');
    let status = match (parts.next(), parts.next()) {
        (Some(version), Some(code)) if version.starts_with("HTTP/1.") => code
            .parse::<u16>()
            .map_err(|_| OciError::Protocol("bad status code".into()))?,
        _ => return Err(OciError::Protocol("bad status line".into())),
    };
    let mut length = None;
    let mut chunked = false;
    for _ in 0..=MAX_HEADERS {
        let line = read_line(&mut reader).map_err(io_err)?;
        if line.is_empty() {
            let framing = if chunked {
                Framing::Chunked {
                    left: 0,
                    done: false,
                }
            } else if let Some(n) = length {
                Framing::Length(n)
            } else if status == 204 || status == 304 || (100..200).contains(&status) {
                Framing::Length(0)
            } else {
                Framing::Eof
            };
            return Ok((status, Body { reader, framing }));
        }
        let Some((name, value)) = line.split_once(':') else {
            return Err(OciError::Protocol("bad header".into()));
        };
        let value = value.trim();
        if name.eq_ignore_ascii_case("content-length") {
            length = Some(
                value
                    .parse::<u64>()
                    .map_err(|_| OciError::Protocol("bad content-length".into()))?,
            );
        } else if name.eq_ignore_ascii_case("transfer-encoding")
            && value.to_ascii_lowercase().contains("chunked")
        {
            chunked = true;
        }
    }
    Err(OciError::Protocol("too many headers".into()))
}

/// Sends one request and buffers at most `max_body` response bytes.
pub(crate) fn call(
    stream: UnixStream,
    method: &str,
    path: &str,
    body: Option<&[u8]>,
    max_body: usize,
) -> Result<Response, OciError> {
    let (status, mut reader) = open(stream, method, path, body)?;
    let mut data = Vec::new();
    let cap = max_body as u64;
    reader
        .by_ref()
        .take(cap + 1)
        .read_to_end(&mut data)
        .map_err(|e| OciError::Io(format!("read body: {e}")))?;
    if data.len() as u64 > cap {
        return Err(OciError::Protocol(format!(
            "response body larger than {max_body} bytes"
        )));
    }
    Ok(Response { status, body: data })
}
