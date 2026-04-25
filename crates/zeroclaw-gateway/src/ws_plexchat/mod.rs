//! PlexChat WebSocket route — port of SPEC §6 PlexChat protocol.
//!
//! Sibling to `ws.rs`: separate route at `/ws/plexchat` with its own
//! wire format, session state, and auth gate semantics. Tool dispatch
//! and LLM streaming wire in PR 3.

pub mod wire;
