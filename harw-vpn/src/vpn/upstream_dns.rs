//! Optional public DNS resolution via Tor. App DNS remains local and synthetic.
use std::collections::HashMap;
use std::io;
use std::net::IpAddr;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use bytes::{Bytes, BytesMut};
use harw_onion::{OnionStream, ProtectedOnionClient};
use hickory_proto::op::{Message, MessageType, Query, ResponseCode};
use hickory_proto::rr::{Name, RData, RecordType};
use http_body_util::{BodyExt, Full};
use hyper::header::{ACCEPT, CONTENT_TYPE, HOST};
use hyper::{Request, Response, StatusCode, Uri, body::Incoming};
use hyper_util::rt::{TokioExecutor, TokioIo};
use rustls::pki_types::ServerName;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::task::JoinHandle;
use tokio_rustls::{TlsConnector, client::TlsStream};

use super::router::public_ip;

const MEDIA_TYPE: &str = "application/dns-message";
const MAX_MESSAGE: usize = 4096;
const MAX_CACHE: usize = 256;

pub struct DnsUpstream {
    endpoint: Endpoint,
    cache: Mutex<HashMap<(String, bool), Cached>>,
}

struct Cached {
    ip: IpAddr,
    expires: Instant,
}

#[derive(Clone, Debug)]
enum Endpoint {
    Https(Uri),
    Tls { host: String, port: u16 },
}

impl DnsUpstream {
    /// An empty value keeps Tor's normal exit-side hostname resolution.
    pub fn parse(value: &str) -> io::Result<Option<Self>> {
        if value.is_empty() {
            return Ok(None);
        }
        if value.len() > 512 || value.contains('#') {
            return Err(invalid_configuration());
        }
        let uri: Uri = value.parse().map_err(|_| invalid_configuration())?;
        let authority = uri.authority().ok_or_else(invalid_configuration)?;
        if authority.as_str().contains('@') {
            return Err(invalid_configuration());
        }
        let host = authority.host().trim_matches(['[', ']']);
        if host.is_empty()
            || host
                .trim_end_matches('.')
                .to_ascii_lowercase()
                .ends_with(".onion")
            || ServerName::try_from(host.to_owned()).is_err()
            || host.parse::<IpAddr>().is_ok_and(|ip| !public_ip(ip))
            || (host.parse::<IpAddr>().is_err() && !host.trim_end_matches('.').contains('.'))
            || authority.port_u16() == Some(0)
        {
            return Err(invalid_configuration());
        }
        let endpoint = match uri.scheme_str() {
            Some("https") if uri.path().len() > 1 && uri.query().is_none() => Endpoint::Https(uri),
            Some("tls") if uri.path() == "/" && uri.query().is_none() => Endpoint::Tls {
                host: host.to_owned(),
                port: authority.port_u16().unwrap_or(853),
            },
            _ => return Err(invalid_configuration()),
        };
        Ok(Some(Self {
            endpoint,
            cache: Mutex::new(HashMap::new()),
        }))
    }

    pub async fn resolve(
        &self,
        client: &ProtectedOnionClient,
        host: &str,
        ipv6: bool,
        timeout: Duration,
    ) -> io::Result<IpAddr> {
        let key = (host.to_owned(), ipv6);
        let now = Instant::now();
        if let Some(ip) = self.cache.lock().ok().and_then(|cache| {
            cache
                .get(&key)
                .filter(|record| record.expires > now)
                .map(|record| record.ip)
        }) {
            return Ok(ip);
        }

        let name = Name::from_ascii(format!("{host}.")).map_err(io::Error::other)?;
        let record_type = if ipv6 {
            RecordType::AAAA
        } else {
            RecordType::A
        };
        let question = Query::query(name, record_type);
        let mut query = Message::new();
        query
            .set_id(0)
            .set_recursion_desired(true)
            .add_query(question.clone());
        let request = Bytes::from(query.to_vec().map_err(io::Error::other)?);
        let packet = tokio::time::timeout(timeout, self.exchange(client, request, timeout))
            .await
            .map_err(|_| io::ErrorKind::TimedOut)??;
        let (ip, ttl) = parse_answer(&question, &packet)?;
        if ttl > 0
            && let Ok(mut cache) = self.cache.lock()
        {
            let now = Instant::now();
            cache.retain(|_, record| record.expires > now);
            if cache.len() < MAX_CACHE {
                cache.insert(
                    key,
                    Cached {
                        ip,
                        expires: now + Duration::from_secs(u64::from(ttl.min(300))),
                    },
                );
            }
        }
        Ok(ip)
    }

    async fn exchange(
        &self,
        client: &ProtectedOnionClient,
        message: Bytes,
        timeout: Duration,
    ) -> io::Result<Bytes> {
        match &self.endpoint {
            Endpoint::Https(uri) => {
                let authority = uri.authority().ok_or_else(invalid_configuration)?;
                let tls = connect_tls(
                    client,
                    authority.host().trim_matches(['[', ']']),
                    authority.port_u16().unwrap_or(443),
                    true,
                    timeout,
                )
                .await?;
                let http2 = tls.get_ref().1.alpn_protocol() == Some(b"h2");
                request_https(tls, http2, uri, message).await
            }
            Endpoint::Tls { host, port } => {
                let mut tls = connect_tls(client, host, *port, false, timeout).await?;
                request_tls_dns(&mut tls, message).await
            }
        }
    }
}

async fn connect_tls(
    client: &ProtectedOnionClient,
    host: &str,
    port: u16,
    https: bool,
    timeout: Duration,
) -> io::Result<TlsStream<OnionStream>> {
    static HTTPS: OnceLock<Arc<rustls::ClientConfig>> = OnceLock::new();
    static TLS: OnceLock<Arc<rustls::ClientConfig>> = OnceLock::new();
    let config = (if https { &HTTPS } else { &TLS }).get_or_init(|| {
        let roots =
            rustls::RootCertStore::from_iter(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
        let mut config = rustls::ClientConfig::builder()
            .with_root_certificates(roots)
            .with_no_client_auth();
        if https {
            config.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
        }
        Arc::new(config)
    });
    let server_name = ServerName::try_from(host.to_owned()).map_err(|_| invalid_configuration())?;
    let stream = client
        .dial_app_exit(host, port, timeout)
        .await
        .map_err(io::Error::other)?;
    TlsConnector::from(config.clone())
        .connect(server_name, stream)
        .await
}

struct AbortOnDrop<T>(JoinHandle<T>);

impl<T> Drop for AbortOnDrop<T> {
    fn drop(&mut self) {
        self.0.abort();
    }
}

async fn request_tls_dns(
    stream: &mut (impl AsyncRead + AsyncWrite + Unpin),
    message: Bytes,
) -> io::Result<Bytes> {
    stream.write_u16(message.len() as u16).await?;
    stream.write_all(&message).await?;
    stream.flush().await?;
    let length = usize::from(stream.read_u16().await?);
    if !(12..=MAX_MESSAGE).contains(&length) {
        return Err(invalid_response());
    }
    let mut response = BytesMut::zeroed(length);
    stream.read_exact(&mut response).await?;
    Ok(response.freeze())
}

async fn request_https(
    tls: impl AsyncRead + AsyncWrite + Unpin + Send + 'static,
    using_http2: bool,
    uri: &Uri,
    body: Bytes,
) -> io::Result<Bytes> {
    let target = if using_http2 {
        uri.clone()
    } else {
        uri.path().parse().map_err(|_| invalid_configuration())?
    };
    let request = Request::post(target)
        .header(
            HOST,
            uri.authority().ok_or_else(invalid_configuration)?.as_str(),
        )
        .header(ACCEPT, MEDIA_TYPE)
        .header(CONTENT_TYPE, MEDIA_TYPE)
        .body(Full::new(body))
        .map_err(io::Error::other)?;
    if using_http2 {
        let (mut sender, connection) =
            hyper::client::conn::http2::handshake(TokioExecutor::new(), TokioIo::new(tls))
                .await
                .map_err(io::Error::other)?;
        let _worker = AbortOnDrop(tokio::spawn(connection));
        read_https(
            sender
                .send_request(request)
                .await
                .map_err(io::Error::other)?,
        )
        .await
    } else {
        let (mut sender, connection) = hyper::client::conn::http1::handshake(TokioIo::new(tls))
            .await
            .map_err(io::Error::other)?;
        let _worker = AbortOnDrop(tokio::spawn(connection));
        read_https(
            sender
                .send_request(request)
                .await
                .map_err(io::Error::other)?,
        )
        .await
    }
}

async fn read_https(response: Response<Incoming>) -> io::Result<Bytes> {
    let content_type = response
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok());
    if response.status() != StatusCode::OK || content_type != Some(MEDIA_TYPE) {
        return Err(invalid_response());
    }
    let mut body = response.into_body();
    let mut bytes = BytesMut::new();
    while let Some(frame) = body.frame().await {
        if let Ok(data) = frame.map_err(io::Error::other)?.into_data() {
            if data.len() > MAX_MESSAGE - bytes.len() {
                return Err(invalid_response());
            }
            bytes.extend_from_slice(&data);
        }
    }
    Ok(bytes.freeze())
}

fn parse_answer(query: &Query, packet: &[u8]) -> io::Result<(IpAddr, u32)> {
    let response = Message::from_vec(packet).map_err(io::Error::other)?;
    if response.id() != 0
        || response.message_type() != MessageType::Response
        || response.truncated()
        || response.response_code() != ResponseCode::NoError
        || response.queries() != std::slice::from_ref(query)
    {
        return Err(invalid_response());
    }
    let mut name = query.name();
    let mut ttl = u32::MAX;
    for _ in 0..8 {
        let mut target = None;
        for record in response
            .answers()
            .iter()
            .filter(|record| record.name() == name && record.dns_class() == query.query_class())
        {
            let address = match record.data() {
                RData::A(address) if query.query_type() == RecordType::A => {
                    Some(IpAddr::V4(address.0))
                }
                RData::AAAA(address) if query.query_type() == RecordType::AAAA => {
                    Some(IpAddr::V6(address.0))
                }
                RData::CNAME(alias) => {
                    target = Some((&alias.0, record.ttl()));
                    None
                }
                _ => None,
            };
            if let Some(address) = address.filter(|ip| public_ip(*ip)) {
                return Ok((address, ttl.min(record.ttl())));
            }
        }
        let Some((next, alias_ttl)) = target else {
            break;
        };
        name = next;
        ttl = ttl.min(alias_ttl);
    }
    Err(invalid_response())
}

fn invalid_configuration() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, "invalid VPN DNS endpoint")
}

fn invalid_response() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, "unusable VPN DNS response")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn dns_over_tls_uses_length_prefixed_messages() {
        let (mut client, mut server) = tokio::io::duplex(128);
        let message = Bytes::from_static(b"query-bytes!");
        let response = b"answer-bytes";
        let peer = async {
            assert_eq!(server.read_u16().await.unwrap(), 12);
            let mut received = [0; 12];
            server.read_exact(&mut received).await.unwrap();
            assert_eq!(&received, b"query-bytes!");
            server.write_u16(12).await.unwrap();
            server.write_all(response).await.unwrap();
        };
        let (result, ()) = tokio::join!(request_tls_dns(&mut client, message), peer);
        assert_eq!(&result.unwrap()[..], response);
    }

    #[tokio::test]
    async fn dns_over_https_posts_wire_format_and_reads_response() {
        let (client, mut server) = tokio::io::duplex(1024);
        let uri = "https://dns.example.com/dns-query".parse::<Uri>().unwrap();
        let peer = async {
            let mut headers = Vec::new();
            while !headers.ends_with(b"\r\n\r\n") {
                headers.push(server.read_u8().await.unwrap());
            }
            let headers = String::from_utf8(headers).unwrap().to_ascii_lowercase();
            assert!(headers.starts_with("post /dns-query http/1.1\r\n"));
            assert!(headers.contains("host: dns.example.com\r\n"));
            assert!(headers.contains("content-type: application/dns-message\r\n"));
            let mut body = [0; 5];
            server.read_exact(&mut body).await.unwrap();
            assert_eq!(&body, b"query");
            server.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: application/dns-message\r\nContent-Length: 6\r\n\r\nanswer").await.unwrap();
        };
        let result = tokio::time::timeout(Duration::from_secs(2), async {
            tokio::join!(
                request_https(client, false, &uri, Bytes::from_static(b"query")),
                peer
            )
            .0
        })
        .await
        .unwrap();
        assert_eq!(&result.unwrap()[..], b"answer");
    }

    #[test]
    fn accepts_only_public_answers_for_the_exact_question() {
        use hickory_proto::rr::{RData, Record, rdata::A};

        let name = Name::from_ascii("www.example.com.").unwrap();
        let query = Query::query(name.clone(), RecordType::A);
        let mut response = Message::new();
        response
            .set_message_type(MessageType::Response)
            .add_query(query.clone());
        response.add_answer(Record::from_rdata(
            name.clone(),
            15,
            RData::A(A(std::net::Ipv4Addr::new(192, 168, 1, 1))),
        ));
        assert!(parse_answer(&query, &response.to_vec().unwrap()).is_err());

        response.add_answer(Record::from_rdata(
            name,
            60,
            RData::A(A(std::net::Ipv4Addr::new(9, 9, 9, 9))),
        ));
        assert_eq!(
            parse_answer(&query, &response.to_vec().unwrap()).unwrap(),
            ("9.9.9.9".parse().unwrap(), 60)
        );

        response.set_id(1);
        assert!(parse_answer(&query, &response.to_vec().unwrap()).is_err());
    }

    #[test]
    fn accepts_encrypted_resolvers_and_rejects_local_or_credential_endpoints() {
        assert!(DnsUpstream::parse("").unwrap().is_none());
        assert!(
            DnsUpstream::parse("https://dns.google/dns-query")
                .unwrap()
                .is_some()
        );
        assert!(
            DnsUpstream::parse("tls://dns.quad9.net:853")
                .unwrap()
                .is_some()
        );
        for bad in [
            "http://dns.example.com/dns-query",
            "https://127.0.0.1/dns-query",
            "https://user@dns.example.com/dns-query",
            "https://dns.example.com/dns-query?token=one",
            "https://example.onion/dns-query",
            "tls://dns.example.com/dns-query",
        ] {
            assert!(DnsUpstream::parse(bad).is_err(), "{bad}");
        }
    }
}
