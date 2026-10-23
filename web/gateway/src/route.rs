//! Both the game tunnel and account HTTP client are constructed from the same
//! selected route. SOCKS5h and HTTP CONNECT leave target DNS to the proxy.
use base64::Engine;
use std::time::Duration;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
};
use url::Url;

#[derive(Clone, Default, serde::Deserialize)]
pub struct Route {
    #[serde(default)]
    pub proxy: String,
}

impl Route {
    pub fn parsed(&self) -> Result<Option<Url>, String> {
        if self.proxy.is_empty() {
            return Ok(None);
        }
        let u = Url::parse(&self.proxy).map_err(|_| "Invalid proxy URL")?;
        if !matches!(u.scheme(), "socks5h" | "http")
            || u.host_str().is_none()
            || u.port_or_known_default() == Some(0)
        {
            return Err("Use socks5h://host:port or http://host:port for the egress proxy.".into());
        }
        if u.query().is_some() || u.fragment().is_some() || !matches!(u.path(), "" | "/") {
            return Err("Proxy URLs cannot contain a path, query or fragment.".into());
        }
        Ok(Some(u))
    }

    pub fn http(&self) -> Result<reqwest::Client, String> {
        // Environment HTTP_PROXY must never silently override the selected route.
        let mut b = reqwest::Client::builder()
            .no_proxy()
            .timeout(Duration::from_secs(20))
            .redirect(reqwest::redirect::Policy::none());
        if let Some(u) = self.parsed()? {
            b = b.proxy(reqwest::Proxy::all(u.as_str()).map_err(|_| "Invalid HTTP proxy")?);
        }
        b.build()
            .map_err(|_| "Could not initialize account networking".into())
    }

    pub async fn connect(&self, host: &str, port: u16) -> Result<TcpStream, String> {
        tokio::time::timeout(Duration::from_secs(12), self.connect_inner(host, port))
            .await
            .map_err(|_| "The server or proxy connection timed out.".to_owned())?
    }

    async fn connect_inner(&self, host: &str, port: u16) -> Result<TcpStream, String> {
        let Some(u) = self.parsed()? else {
            let s = TcpStream::connect((host, port))
                .await
                .map_err(|_| "Could not connect to the Minecraft server.")?;
            let _ = s.set_nodelay(true);
            return Ok(s);
        };
        let proxy = (
            u.host_str().unwrap(),
            u.port_or_known_default().unwrap_or(1080),
        );
        let username = percent_encoding::percent_decode_str(u.username())
            .decode_utf8()
            .map_err(|_| "Invalid proxy credentials.")?;
        let password = percent_encoding::percent_decode_str(u.password().unwrap_or(""))
            .decode_utf8()
            .map_err(|_| "Invalid proxy credentials.")?;
        let s = if u.scheme() == "socks5h" {
            if u.username().is_empty() {
                tokio_socks::tcp::Socks5Stream::connect(proxy, (host, port)).await
            } else {
                tokio_socks::tcp::Socks5Stream::connect_with_password(
                    proxy,
                    (host, port),
                    &username,
                    &password,
                )
                .await
            }
            .map_err(|_| "The SOCKS5 proxy could not connect to the server.")?
            .into_inner()
        } else {
            let mut s = TcpStream::connect(proxy)
                .await
                .map_err(|_| "Could not reach the HTTP proxy.")?;
            let target = if host.contains(':') {
                format!("[{host}]:{port}")
            } else {
                format!("{host}:{port}")
            };
            let mut headers = format!("CONNECT {target} HTTP/1.1\r\nHost: {target}\r\n");
            if !u.username().is_empty() {
                headers.push_str(&format!(
                    "Proxy-Authorization: Basic {}\r\n",
                    base64::engine::general_purpose::STANDARD
                        .encode(format!("{username}:{password}"))
                ));
            }
            headers.push_str("\r\n");
            s.write_all(headers.as_bytes())
                .await
                .map_err(|_| "The HTTP proxy disconnected.")?;
            // Read exactly the header, leaving all following Minecraft bytes in TCP.
            let mut response = vec![];
            while !response.ends_with(b"\r\n\r\n") {
                if response.len() >= 8192 {
                    return Err("The HTTP proxy response is too large.".into());
                }
                response.push(
                    s.read_u8()
                        .await
                        .map_err(|_| "The HTTP proxy disconnected.")?,
                );
            }
            let first = String::from_utf8_lossy(&response);
            if first
                .lines()
                .next()
                .and_then(|l| l.split_whitespace().nth(1))
                != Some("200")
            {
                return Err("The HTTP proxy refused the tunnel.".into());
            }
            s
        };
        let _ = s.set_nodelay(true);
        Ok(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn validates_routes_without_echoing_credentials() {
        assert!(
            Route {
                proxy: "socks5h://proxy.example:1080".into()
            }
            .parsed()
            .is_ok()
        );
        assert!(
            Route {
                proxy: "http://user:secret@proxy.example:8080".into()
            }
            .parsed()
            .is_ok()
        );
        let err = Route {
            proxy: "ftp://user:secret@proxy.example:21".into(),
        }
        .parsed()
        .err()
        .unwrap();
        assert!(!err.contains("secret"));
        assert!(
            Route {
                proxy: "http://proxy.example:80".into()
            }
            .parsed()
            .is_ok()
        );
    }
}
