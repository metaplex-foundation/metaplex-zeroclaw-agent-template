//! AGENT_KEYPAIR decoder. Accepts base58-encoded 64-byte secret keys and
//! JSON byte arrays of length 64. Matches SPEC §8.1.

use anyhow::{Context, Result, anyhow};
use solana_sdk::signature::Keypair;

pub fn decode(raw: &str) -> Result<Keypair> {
    let trimmed = raw.trim();
    if trimmed.starts_with('[') {
        let bytes: Vec<u8> =
            serde_json::from_str(trimmed).context("AGENT_KEYPAIR: expected JSON byte array")?;
        if bytes.len() != 64 {
            return Err(anyhow!(
                "AGENT_KEYPAIR: JSON byte array has length {}, expected 64",
                bytes.len()
            ));
        }
        Keypair::try_from(&bytes[..]).map_err(|e| anyhow!("AGENT_KEYPAIR: {e}"))
    } else {
        let bytes = bs58::decode(trimmed)
            .into_vec()
            .context("AGENT_KEYPAIR: base58 decode failed")?;
        if bytes.len() != 64 {
            return Err(anyhow!(
                "AGENT_KEYPAIR: base58 secret has length {}, expected 64",
                bytes.len()
            ));
        }
        Keypair::try_from(&bytes[..]).map_err(|e| anyhow!("AGENT_KEYPAIR: {e}"))
    }
}

pub fn from_env() -> Result<Keypair> {
    let raw = std::env::var("AGENT_KEYPAIR").context("AGENT_KEYPAIR is not set")?;
    decode(&raw)
}

pub use solana_sdk::signature::{Keypair as SolanaKeypair, Signer as SolanaSigner};

#[cfg(test)]
mod tests {
    use super::*;
    use solana_sdk::signature::Signer;

    fn fresh_keypair_bytes() -> [u8; 64] {
        solana_sdk::signature::Keypair::new().to_bytes()
    }

    #[test]
    fn decodes_base58_secret() {
        let bytes = fresh_keypair_bytes();
        let expected_pk = solana_sdk::signature::Keypair::try_from(&bytes[..])
            .unwrap()
            .pubkey();
        let encoded = bs58::encode(bytes).into_string();
        let decoded = decode(&encoded).expect("should decode base58 secret");
        assert_eq!(decoded.pubkey(), expected_pk);
    }

    #[test]
    fn decodes_json_byte_array() {
        let bytes = fresh_keypair_bytes();
        let expected_pk = solana_sdk::signature::Keypair::try_from(&bytes[..])
            .unwrap()
            .pubkey();
        let json = serde_json::to_string(&bytes.to_vec()).unwrap();
        let decoded = decode(&json).expect("should decode JSON byte array");
        assert_eq!(decoded.pubkey(), expected_pk);
    }

    #[test]
    fn trims_surrounding_whitespace_on_both_formats() {
        let bytes = fresh_keypair_bytes();
        let b58 = format!("  {}  \n", bs58::encode(bytes).into_string());
        assert!(decode(&b58).is_ok());
        let json = format!("\t{}  ", serde_json::to_string(&bytes.to_vec()).unwrap());
        assert!(decode(&json).is_ok());
    }

    #[test]
    fn rejects_malformed_base58() {
        let err = decode("not-valid-base58-!!!").unwrap_err();
        let msg = err.to_string().to_lowercase();
        assert!(msg.contains("agent_keypair"));
    }

    #[test]
    fn rejects_wrong_length_base58() {
        // 32 bytes encoded → not 64 long
        let encoded = bs58::encode([0u8; 32]).into_string();
        let err = decode(&encoded).unwrap_err();
        let msg = err.to_string().to_lowercase();
        assert!(msg.contains("length") || msg.contains("64"));
    }

    #[test]
    fn rejects_wrong_length_json_array() {
        let bad = serde_json::to_string(&vec![1u8, 2, 3]).unwrap();
        let err = decode(&bad).unwrap_err();
        let msg = err.to_string().to_lowercase();
        assert!(msg.contains("length") || msg.contains("64"));
    }

    #[test]
    fn rejects_malformed_json_array() {
        let err = decode("[1, 2, \"three\"]").unwrap_err();
        let msg = err.to_string().to_lowercase();
        assert!(msg.contains("json") || msg.contains("expected"));
    }
}
