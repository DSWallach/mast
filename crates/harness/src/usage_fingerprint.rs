use serde_json::Value;
use sha2::{Digest, Sha256};

/// A capture-local secret keeps low-entropy payloads out of the ledger and
/// prevents matching their fingerprints across separate harness captures.
pub(crate) struct UsageFingerprint(uuid::Uuid);

impl Default for UsageFingerprint {
    fn default() -> Self {
        Self(uuid::Uuid::new_v4())
    }
}

impl UsageFingerprint {
    pub(crate) fn of(&self, payload: &Value) -> String {
        self.measure(payload).1
    }

    pub(crate) fn measure(&self, payload: &Value) -> (u64, String) {
        let body = payload
            .as_str()
            .map(std::borrow::Cow::Borrowed)
            .unwrap_or_else(|| std::borrow::Cow::Owned(payload.to_string()));
        let mut hash = Sha256::new();
        hash.update(self.0.as_bytes());
        hash.update(if payload.is_string() {
            b"text:".as_slice()
        } else {
            b"json:".as_slice()
        });
        hash.update(body.as_bytes());
        (body.len() as u64, format!("harness:{:x}", hash.finalize()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fingerprints_match_only_within_a_capture_and_do_not_retain_payloads() {
        let key = UsageFingerprint::default();
        let payload = serde_json::json!("private response");
        assert_eq!(key.of(&payload), key.of(&payload));
        assert_ne!(key.of(&payload), UsageFingerprint::default().of(&payload));
        assert_ne!(key.of(&payload), key.of(&serde_json::json!("other")));
        assert!(!key.of(&payload).contains("private response"));
    }
}
