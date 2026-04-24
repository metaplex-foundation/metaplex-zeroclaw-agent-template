//! Solana + Metaplex foundation for ZeroClaw-hosted agents.
//!
//! This crate is the port of the Metaplex Agent Template's `@metaplex-agent/shared`
//! package to ZeroClaw. It contains the data structures, error taxonomy, RPC
//! client wrapper, keypair decoder, state file, agent context, and owner resolver.
//! It intentionally has **no LLM or transport concerns** — those live in
//! `zeroclaw-tools` and `zeroclaw-gateway`.
//!
//! See `docs/plans/2026-04-24-metaplex-agent-port-design.md` §1 for the full
//! crate map.

pub mod error_codes;
pub mod tool_result;
