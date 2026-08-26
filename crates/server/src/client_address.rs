//! Proxy-safe resolution and normalization of request client addresses.

use std::{
    convert::Infallible,
    net::{IpAddr, Ipv4Addr, SocketAddr},
};

use axum::{
    extract::{ConnectInfo, FromRequestParts},
    http::{HeaderMap, request::Parts},
};
use ipnet::{IpNet, Ipv6Net};

/// Socket peer extractor with a loopback fallback for in-process router tests.
pub(crate) struct ClientSocket(pub(crate) IpAddr);

impl<S: Send + Sync> FromRequestParts<S> for ClientSocket {
    type Rejection = Infallible;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        Ok(Self(
            parts
                .extensions
                .get::<ConnectInfo<SocketAddr>>()
                .map_or(IpAddr::V4(Ipv4Addr::LOCALHOST), |peer| peer.0.ip()),
        ))
    }
}

/// Resolve a request's client address without trusting caller-controlled forwarding headers.
pub(crate) fn resolve(peer: IpAddr, headers: &HeaderMap, trusted_proxies: &[IpNet]) -> IpAddr {
    let peer = normalize(peer);
    if !is_trusted(peer, trusted_proxies) {
        return peer;
    }
    let Some(value) = headers
        .get("x-forwarded-for")
        .and_then(|value| value.to_str().ok())
    else {
        return peer;
    };
    let parts = value.split(',').map(str::trim).collect::<Vec<_>>();
    if parts.is_empty() || parts.len() > 32 {
        return peer;
    }
    let Some(chain) = parts
        .iter()
        .map(|part| part.parse::<IpAddr>().ok().map(normalize))
        .collect::<Option<Vec<_>>>()
    else {
        return peer;
    };
    chain
        .into_iter()
        .rev()
        .find(|address| !is_trusted(*address, trusted_proxies))
        .unwrap_or(peer)
}

/// Convert an address to the stable grouping used as HMAC input.
pub(crate) fn group(address: IpAddr, ipv6_prefix_length: u8) -> String {
    match normalize(address) {
        IpAddr::V4(address) => address.to_string(),
        IpAddr::V6(address) => Ipv6Net::new(address, ipv6_prefix_length).map_or_else(
            |_| address.to_string(),
            |network| network.network().to_string(),
        ),
    }
}

fn normalize(address: IpAddr) -> IpAddr {
    match address {
        IpAddr::V6(address) => address
            .to_ipv4_mapped()
            .map_or(IpAddr::V6(address), IpAddr::V4),
        address => address,
    }
}

fn is_trusted(address: IpAddr, trusted_proxies: &[IpNet]) -> bool {
    trusted_proxies
        .iter()
        .any(|network| network.contains(&address))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ignores_forwarding_from_an_untrusted_peer() {
        let mut headers = HeaderMap::new();
        headers.insert("x-forwarded-for", "198.51.100.1".parse().unwrap());
        assert_eq!(
            resolve("203.0.113.2".parse().unwrap(), &headers, &[]),
            "203.0.113.2".parse::<IpAddr>().unwrap()
        );
    }

    #[test]
    fn walks_a_trusted_proxy_chain_from_the_right() {
        let mut headers = HeaderMap::new();
        headers.insert("x-forwarded-for", "198.51.100.1, 10.0.0.2".parse().unwrap());
        let trusted = vec!["10.0.0.0/8".parse().unwrap()];
        assert_eq!(
            resolve("10.0.0.3".parse().unwrap(), &headers, &trusted),
            "198.51.100.1".parse::<IpAddr>().unwrap()
        );
    }

    #[test]
    fn malformed_and_oversized_chains_fall_back_to_the_peer() {
        let trusted = vec!["10.0.0.0/8".parse().unwrap()];
        let oversized = vec!["10.0.0.1"; 33].join(",");
        for value in ["invalid", oversized.as_str()] {
            let mut headers = HeaderMap::new();
            headers.insert("x-forwarded-for", value.parse().unwrap());
            assert_eq!(
                resolve("10.0.0.3".parse().unwrap(), &headers, &trusted),
                "10.0.0.3".parse::<IpAddr>().unwrap()
            );
        }
    }

    #[test]
    fn normalizes_mapped_ipv4_and_masks_ipv6() {
        assert_eq!(group("::ffff:192.0.2.4".parse().unwrap(), 64), "192.0.2.4");
        assert_eq!(
            group("2001:db8:abcd:12::42".parse().unwrap(), 56),
            "2001:db8:abcd::"
        );
    }
}
