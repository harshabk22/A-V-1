use crate::{MoatError, MoatResult};
use semver::Version;
use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize, Serialize, Clone, PartialEq, Eq)]
pub struct AgentManifest {
    pub schema_version: u32,
    pub id: String,
    pub description: String,
    pub requested_capabilities: Vec<String>,
    pub min_kernel_version: String,
}

#[derive(Debug, Deserialize, Serialize, Clone, PartialEq, Eq)]
pub struct SignedManifest {
    pub manifest: AgentManifest,
    pub signature: Option<String>,
}

pub const UNTRUSTED_AGENT_CEILING: &[&str] = &[
    "network:declared_domains_only",
    "fs:sandbox_subdirectory_only",
    "tool:read_only",
];

pub fn kernel_version() -> Version {
    Version::parse(env!("CARGO_PKG_VERSION")).unwrap_or_else(|_| Version::new(0, 1, 0))
}

pub fn canonical_manifest_json(manifest: &AgentManifest) -> MoatResult<Vec<u8>> {
    serde_json::to_vec(manifest).map_err(MoatError::Serde)
}

pub fn validate_manifest(manifest: &AgentManifest) -> MoatResult<()> {
    if manifest.schema_version != 1 {
        return Err(MoatError::SecurityViolation(format!(
            "manifest '{}' rejected: schema_version {} is not supported",
            manifest.id, manifest.schema_version
        )));
    }

    let required = Version::parse(manifest.min_kernel_version.trim()).map_err(|err| {
        MoatError::SecurityViolation(format!(
            "manifest '{}' has invalid min_kernel_version '{}': {err}",
            manifest.id, manifest.min_kernel_version
        ))
    })?;
    let running = kernel_version();
    if required > running {
        return Err(MoatError::SecurityViolation(format!(
            "manifest '{}' requires kernel {} but running kernel is {}",
            manifest.id, required, running
        )));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{validate_manifest, AgentManifest};

    #[test]
    fn validate_manifest_rejects_wrong_schema() {
        let manifest = AgentManifest {
            schema_version: 2,
            id: "coder".into(),
            description: "test".into(),
            requested_capabilities: vec![],
            min_kernel_version: "0.1.0".into(),
        };
        assert!(validate_manifest(&manifest).is_err());
    }
}
