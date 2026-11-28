//! `GetQuotaSettings` / `SetQuotaSettings`: the owning device's quota cap
//! settings (see [`crate::quota_settings`]). Set takes the complete record and
//! both reply with what is saved.

use std::path::Path;

use mast_rpc::{RpcError, RpcReply, methods, parse_params};

use crate::quota_settings::{self, QuotaSettings, SaveError};

pub(super) fn handle(
    method: &str,
    params: serde_json::Value,
    store_root: &Path,
) -> Result<RpcReply, RpcError> {
    if method == methods::SET_QUOTA_SETTINGS {
        let settings: QuotaSettings = parse_params(params)?;
        quota_settings::save(store_root, &settings).map_err(|error| match error {
            SaveError::Invalid(error) => RpcError::BadParams(error.to_string()),
            error @ SaveError::Io(_) => RpcError::Failed(error.to_string()),
        })?;
    }
    RpcReply::value(&quota_settings::load(store_root))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn both_methods_follow_target_device_id() {
        for method in [methods::GET_QUOTA_SETTINGS, methods::SET_QUOTA_SETTINGS] {
            assert!(super::super::forwardable(method), "{method}");
        }
    }
}
