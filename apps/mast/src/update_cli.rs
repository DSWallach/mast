//! Source builds are updated by rebuilding the repository.

pub async fn update(_edge_url: &str, _check_only: bool) -> anyhow::Result<()> {
    anyhow::bail!("Mast updates come from building from source; pull the latest reviewed changes and rebuild.")
}
