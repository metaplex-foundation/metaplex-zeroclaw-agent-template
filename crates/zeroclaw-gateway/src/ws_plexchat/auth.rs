//! Bearer-token extraction for the PlexChat WebSocket route.
//!
//! SPEC §6.2 specifies three accepted auth carriers (the client picks one):
//!
//! 1. `Authorization: Bearer <token>` header.
//! 2. `Sec-WebSocket-Protocol: bearer, <token>` two-entry subprotocol.
//! 3. `?token=<token>` query parameter (discouraged in production).
//!
//! The two-entry subprotocol form is **distinct** from the existing
//! `bearer.<token>` dot-separated form used by `crate::ws`. We keep this
//! helper separate so PlexChat strictly matches the SPEC, while still
//! accepting the legacy dot-separated form as a fallback for clients that
//! have not migrated.

use axum::http::{HeaderMap, header};

/// Extract a bearer token from WebSocket-compatible carriers.
///
/// Precedence (first non-empty wins):
///
/// 1. `Authorization: Bearer <token>` header.
/// 2. `Sec-WebSocket-Protocol: bearer, <token>` (SPEC §6.2: two entries,
///    the second is the token; `bearer` is echoed back on accept).
/// 3. `Sec-WebSocket-Protocol: bearer.<token>` (legacy dot-separated form
///    used elsewhere in this gateway; accepted as a compat fallback).
/// 4. `?token=<token>` query parameter.
///
/// Returns `None` if no carrier produces a non-empty token.
pub fn extract_ws_token<'a>(
    headers: &'a HeaderMap,
    query_token: Option<&'a str>,
) -> Option<&'a str> {
    // 1. Authorization header
    if let Some(t) = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|auth| auth.strip_prefix("Bearer "))
        && !t.is_empty()
    {
        return Some(t);
    }

    // 2 & 3. Sec-WebSocket-Protocol carrier — try SPEC two-entry form first,
    // then legacy `bearer.<token>` dot form.
    if let Some(protos) = headers
        .get("sec-websocket-protocol")
        .and_then(|v| v.to_str().ok())
    {
        let entries: Vec<&str> = protos.split(',').map(str::trim).collect();

        // SPEC §6.2: `bearer, <token>` — `bearer` is the FIRST entry, token
        // is the SECOND. We accept `bearer` anywhere in the list as the
        // sentinel and treat the immediately-following entry as the token.
        for window in entries.windows(2) {
            if window[0].eq_ignore_ascii_case("bearer") && !window[1].is_empty() {
                return Some(window[1]);
            }
        }

        // Legacy dot form: `bearer.<token>` (compat with `crate::ws`).
        if let Some(t) = entries.iter().find_map(|p| p.strip_prefix("bearer."))
            && !t.is_empty()
        {
            return Some(t);
        }
    }

    // 4. ?token= query parameter
    if let Some(t) = query_token
        && !t.is_empty()
    {
        return Some(t);
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderMap;

    #[test]
    fn extracts_from_authorization_header() {
        let mut h = HeaderMap::new();
        h.insert("authorization", "Bearer plex_abc".parse().unwrap());
        assert_eq!(extract_ws_token(&h, None), Some("plex_abc"));
    }

    #[test]
    fn extracts_from_two_entry_subprotocol() {
        let mut h = HeaderMap::new();
        h.insert(
            "sec-websocket-protocol",
            "bearer, plex_sub".parse().unwrap(),
        );
        assert_eq!(extract_ws_token(&h, None), Some("plex_sub"));
    }

    #[test]
    fn extracts_from_legacy_dot_subprotocol() {
        let mut h = HeaderMap::new();
        h.insert(
            "sec-websocket-protocol",
            "zeroclaw.v1, bearer.plex_legacy".parse().unwrap(),
        );
        assert_eq!(extract_ws_token(&h, None), Some("plex_legacy"));
    }

    #[test]
    fn extracts_from_query_param() {
        let h = HeaderMap::new();
        assert_eq!(extract_ws_token(&h, Some("plex_q")), Some("plex_q"));
    }

    #[test]
    fn header_takes_precedence_over_subprotocol_and_query() {
        let mut h = HeaderMap::new();
        h.insert("authorization", "Bearer plex_h".parse().unwrap());
        h.insert(
            "sec-websocket-protocol",
            "bearer, plex_sub".parse().unwrap(),
        );
        assert_eq!(extract_ws_token(&h, Some("plex_q")), Some("plex_h"));
    }

    #[test]
    fn subprotocol_takes_precedence_over_query() {
        let mut h = HeaderMap::new();
        h.insert(
            "sec-websocket-protocol",
            "bearer, plex_sub".parse().unwrap(),
        );
        assert_eq!(extract_ws_token(&h, Some("plex_q")), Some("plex_sub"));
    }

    #[test]
    fn empty_header_falls_through_to_query() {
        let mut h = HeaderMap::new();
        h.insert("authorization", "Bearer ".parse().unwrap());
        assert_eq!(extract_ws_token(&h, Some("plex_q")), Some("plex_q"));
    }

    #[test]
    fn empty_query_returns_none() {
        let h = HeaderMap::new();
        assert_eq!(extract_ws_token(&h, Some("")), None);
    }

    #[test]
    fn no_carrier_returns_none() {
        let h = HeaderMap::new();
        assert_eq!(extract_ws_token(&h, None), None);
    }

    #[test]
    fn bearer_sentinel_is_case_insensitive() {
        let mut h = HeaderMap::new();
        h.insert(
            "sec-websocket-protocol",
            "Bearer, plex_xyz".parse().unwrap(),
        );
        assert_eq!(extract_ws_token(&h, None), Some("plex_xyz"));
    }

    #[test]
    fn lone_bearer_entry_with_no_token_returns_none() {
        let mut h = HeaderMap::new();
        h.insert("sec-websocket-protocol", "bearer".parse().unwrap());
        assert_eq!(extract_ws_token(&h, None), None);
    }
}
