//! PlexChat wire enums (SPEC §6.3, §6.4, Appendix A).

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientMsg {
    Message {
        content: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        sender_name: Option<String>,
    },
    WalletConnect {
        address: String,
    },
    WalletDisconnect,
    TxResult {
        #[serde(rename = "correlationId")]
        correlation_id: String,
        signature: String,
    },
    TxError {
        #[serde(rename = "correlationId")]
        correlation_id: String,
        #[serde(default)]
        reason: Option<String>,
    },
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerMsg {
    Connected {
        jid: String,
    },
    Message {
        content: String,
        sender: String,
    },
    Typing {
        #[serde(rename = "isTyping")]
        is_typing: bool,
    },
    Transaction {
        transaction: String,
        #[serde(rename = "correlationId")]
        correlation_id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        message: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        index: Option<u32>,
        #[serde(skip_serializing_if = "Option::is_none")]
        total: Option<u32>,
        #[serde(rename = "feeSol", skip_serializing_if = "Option::is_none")]
        fee_sol: Option<f64>,
    },
    WalletConnected {
        address: String,
    },
    WalletDisconnected,
    Error {
        error: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        code: Option<String>,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn client_message_round_trip() {
        let v = json!({"type": "message", "content": "hi", "sender_name": "alice"});
        let m: ClientMsg = serde_json::from_value(v.clone()).unwrap();
        assert_eq!(
            m,
            ClientMsg::Message {
                content: "hi".into(),
                sender_name: Some("alice".into())
            }
        );
        assert_eq!(serde_json::to_value(&m).unwrap(), v);
    }

    #[test]
    fn client_wallet_connect_with_address() {
        let v = json!({"type": "wallet_connect", "address": "ABC"});
        let m: ClientMsg = serde_json::from_value(v).unwrap();
        assert_eq!(
            m,
            ClientMsg::WalletConnect {
                address: "ABC".into()
            }
        );
    }

    #[test]
    fn client_tx_result_uses_correlation_id_camel() {
        let v = json!({"type": "tx_result", "correlationId": "c1", "signature": "S"});
        let m: ClientMsg = serde_json::from_value(v).unwrap();
        assert_eq!(
            m,
            ClientMsg::TxResult {
                correlation_id: "c1".into(),
                signature: "S".into()
            }
        );
    }

    #[test]
    fn client_tx_error_reason_optional() {
        let v = json!({"type": "tx_error", "correlationId": "c1"});
        let m: ClientMsg = serde_json::from_value(v).unwrap();
        assert_eq!(
            m,
            ClientMsg::TxError {
                correlation_id: "c1".into(),
                reason: None
            }
        );
    }

    #[test]
    fn server_transaction_uses_camel_fields() {
        let m = ServerMsg::Transaction {
            transaction: "b64".into(),
            correlation_id: "c1".into(),
            message: Some("Buyback".into()),
            index: Some(1),
            total: Some(3),
            fee_sol: Some(0.001),
        };
        let v = serde_json::to_value(&m).unwrap();
        assert_eq!(v["correlationId"], "c1");
        assert_eq!(v["feeSol"], 0.001);
        assert_eq!(v["index"], 1);
        assert_eq!(v["type"], "transaction");
    }

    #[test]
    fn server_typing_camel() {
        let m = ServerMsg::Typing { is_typing: true };
        let v = serde_json::to_value(&m).unwrap();
        assert_eq!(v["isTyping"], true);
        assert_eq!(v["type"], "typing");
    }

    #[test]
    fn server_error_omits_code_when_none() {
        let m = ServerMsg::Error {
            error: "x".into(),
            code: None,
        };
        let s = serde_json::to_string(&m).unwrap();
        assert!(!s.contains("\"code\""));
    }

    #[test]
    fn unknown_client_type_is_rejected() {
        let v = json!({"type": "unknown_type"});
        let r: Result<ClientMsg, _> = serde_json::from_value(v);
        assert!(r.is_err());
    }
}
