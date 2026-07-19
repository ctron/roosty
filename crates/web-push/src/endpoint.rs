use std::{
    borrow::Cow,
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
};

use url::{Host, Url};

use crate::WebPushError;

pub(crate) fn validate_url(url: &Url) -> Result<(), WebPushError> {
    if url.scheme() != "https" {
        return Err(WebPushError::InvalidEndpoint("HTTPS is required".into()));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(WebPushError::InvalidEndpoint(
            "credentials are not permitted".into(),
        ));
    }
    match url.host() {
        Some(Host::Domain(_)) => Ok(()),
        Some(Host::Ipv4(ip)) if is_public(IpAddr::V4(ip)) => Ok(()),
        Some(Host::Ipv6(ip)) if is_public(IpAddr::V6(ip)) => Ok(()),
        Some(_) => Err(WebPushError::UnsafeEndpoint),
        None => Err(WebPushError::InvalidEndpoint(Cow::Borrowed(
            "host is missing",
        ))),
    }
}

pub(crate) async fn resolve_public(url: &Url) -> Result<Vec<SocketAddr>, WebPushError> {
    validate_url(url)?;
    let host = url
        .host_str()
        .ok_or(WebPushError::InvalidEndpoint("host is missing".into()))?;
    let port = url.port_or_known_default().unwrap_or(443);
    let addresses: Vec<_> = tokio::net::lookup_host((host, port))
        .await
        .map_err(|_| WebPushError::UnresolvedEndpoint)?
        .collect();
    validate_resolved(&addresses)?;
    Ok(addresses)
}

fn validate_resolved(addresses: &[SocketAddr]) -> Result<(), WebPushError> {
    if addresses.is_empty() {
        return Err(WebPushError::UnresolvedEndpoint);
    }
    if addresses.iter().any(|address| !is_public(address.ip())) {
        return Err(WebPushError::UnsafeEndpoint);
    }
    Ok(())
}

fn is_public(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => is_public_v4(ip),
        IpAddr::V6(ip) => is_public_v6(ip),
    }
}

fn is_public_v4(ip: Ipv4Addr) -> bool {
    let [a, b, c, _] = ip.octets();
    !(a == 0
        || a == 10
        || a == 127
        || (a == 100 && (64..=127).contains(&b))
        || (a == 169 && b == 254)
        || (a == 172 && (16..=31).contains(&b))
        || (a == 192 && b == 0 && c == 0)
        || (a == 192 && b == 0 && c == 2)
        || (a == 192 && b == 168)
        || (a == 198 && (b == 18 || b == 19))
        || (a == 198 && b == 51 && c == 100)
        || (a == 203 && b == 0 && c == 113)
        || a >= 224)
}

fn is_public_v6(ip: Ipv6Addr) -> bool {
    let segments = ip.segments();
    if let Some(v4) = ip.to_ipv4_mapped() {
        return is_public_v4(v4);
    }
    !(ip.is_unspecified()
        || ip.is_loopback()
        || (segments[0] & 0xfe00) == 0xfc00
        || (segments[0] & 0xffc0) == 0xfe80
        || (segments[0] & 0xff00) == 0xff00
        || (segments[0] == 0x2001 && segments[1] == 0x0db8))
}

#[cfg(test)]
mod tests {
    use super::{is_public, validate_resolved, validate_url};
    use crate::WebPushError;
    use std::net::{IpAddr, SocketAddr};
    use url::Url;

    #[test]
    fn rejects_non_public_addresses() {
        for value in [
            "127.0.0.1",
            "10.0.0.1",
            "169.254.1.2",
            "192.168.1.1",
            "::1",
            "fc00::1",
            "fe80::1",
            "2001:db8::1",
        ] {
            let address: IpAddr = value
                .parse()
                .unwrap_or_else(|error| unreachable!("test IP must parse: {error}"));
            assert!(!is_public(address), "{value}");
        }
    }

    #[test]
    fn accepts_public_addresses() {
        for value in ["1.1.1.1", "8.8.8.8", "2606:4700:4700::1111"] {
            let address: IpAddr = value
                .parse()
                .unwrap_or_else(|error| unreachable!("test IP must parse: {error}"));
            assert!(is_public(address), "{value}");
        }
    }

    #[test]
    fn validates_endpoint_url_boundaries() {
        let public = Url::parse("https://1.1.1.1/push").unwrap();
        assert!(validate_url(&public).is_ok());

        for value in [
            "http://push.example/message",
            "https://user:password@push.example/message",
        ] {
            let url = Url::parse(value).unwrap();
            assert!(matches!(
                validate_url(&url),
                Err(WebPushError::InvalidEndpoint(_))
            ));
        }
        let private = Url::parse("https://127.0.0.1/push").unwrap();
        assert!(matches!(
            validate_url(&private),
            Err(WebPushError::UnsafeEndpoint)
        ));
    }

    #[test]
    fn rejects_empty_and_mixed_dns_results() {
        assert!(matches!(
            validate_resolved(&[]),
            Err(WebPushError::UnresolvedEndpoint)
        ));
        let mixed: Vec<SocketAddr> = ["1.1.1.1:443", "127.0.0.1:443"]
            .into_iter()
            .map(|value| value.parse().unwrap())
            .collect();
        assert!(matches!(
            validate_resolved(&mixed),
            Err(WebPushError::UnsafeEndpoint)
        ));
        let public = ["1.1.1.1:443".parse().unwrap()];
        assert!(validate_resolved(&public).is_ok());
    }
}
