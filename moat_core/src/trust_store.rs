use crate::manifest::{
    canonical_manifest_json, validate_manifest, AgentManifest, SignedManifest,
    UNTRUSTED_AGENT_CEILING,
};
use crate::{MoatError, MoatResult};
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use base64::Engine as _;
use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use std::collections::HashSet;
use std::env;
use std::sync::RwLock;

const TRUSTED_MANIFEST_KEYS_ENV: &str = "MOAT_TRUSTED_MANIFEST_PUBKEYS";

#[derive(Debug, Clone)]
pub struct ManifestAssessment {
    pub manifest: AgentManifest,
    pub trusted: bool,
    pub effective_capabilities: Vec<String>,
}

pub struct TrustStore {
    trusted_keys: Vec<VerifyingKey>,
    builtin_worker_ids: RwLock<HashSet<String>>,
}

impl TrustStore {
    pub fn new() -> Self {
        Self {
            trusted_keys: load_trusted_keys(),
            builtin_worker_ids: RwLock::new(HashSet::new()),
        }
    }

    pub fn register_builtin_worker(&self, worker_id: &str) {
        if let Ok(mut ids) = self.builtin_worker_ids.write() {
            ids.insert(worker_id.trim().to_ascii_lowercase());
        }
    }

    pub fn clear_builtin_workers(&self) {
        if let Ok(mut ids) = self.builtin_worker_ids.write() {
            ids.clear();
        }
    }

    pub fn assess_builtin_manifest(
        &self,
        manifest: AgentManifest,
    ) -> MoatResult<ManifestAssessment> {
        validate_manifest(&manifest)?;
        self.register_builtin_worker(&manifest.id);
        Ok(ManifestAssessment {
            effective_capabilities: manifest.requested_capabilities.clone(),
            manifest,
            trusted: true,
        })
    }

    pub fn assess_signed_manifest(
        &self,
        signed: &SignedManifest,
    ) -> MoatResult<ManifestAssessment> {
        validate_manifest(&signed.manifest)?;
        let trusted = self.is_builtin(&signed.manifest.id) || self.verify_signature(signed)?;
        let effective_capabilities = if trusted {
            signed.manifest.requested_capabilities.clone()
        } else {
            self.clamp_untrusted_capabilities(&signed.manifest)?
        };

        Ok(ManifestAssessment {
            manifest: signed.manifest.clone(),
            trusted,
            effective_capabilities,
        })
    }

    fn is_builtin(&self, worker_id: &str) -> bool {
        self.builtin_worker_ids
            .read()
            .map(|ids| ids.contains(&worker_id.trim().to_ascii_lowercase()))
            .unwrap_or(false)
    }

    fn verify_signature(&self, signed: &SignedManifest) -> MoatResult<bool> {
        let Some(signature_b64) = signed.signature.as_deref() else {
            return Ok(false);
        };
        if signature_b64.trim().is_empty() {
            return Ok(false);
        }
        if self.trusted_keys.is_empty() {
            return Ok(false);
        }

        let signature_bytes = BASE64_STANDARD
            .decode(signature_b64.trim())
            .map_err(|err| {
                MoatError::SecurityViolation(format!("invalid manifest signature encoding: {err}"))
            })?;
        let signature = Signature::from_slice(&signature_bytes).map_err(|err| {
            MoatError::SecurityViolation(format!("invalid manifest signature bytes: {err}"))
        })?;
        let payload = canonical_manifest_json(&signed.manifest)?;

        Ok(self
            .trusted_keys
            .iter()
            .any(|key| key.verify(&payload, &signature).is_ok()))
    }

    fn clamp_untrusted_capabilities(&self, manifest: &AgentManifest) -> MoatResult<Vec<String>> {
        let ceiling: HashSet<&str> = UNTRUSTED_AGENT_CEILING.iter().copied().collect();
        let mut effective = Vec::new();
        for capability in &manifest.requested_capabilities {
            if ceiling.contains(capability.as_str()) {
                effective.push(capability.clone());
            } else {
                return Err(MoatError::SecurityViolation(format!(
                    "untrusted manifest '{}' requested capability '{}' above untrusted ceiling",
                    manifest.id, capability
                )));
            }
        }
        Ok(effective)
    }
}

fn load_trusted_keys() -> Vec<VerifyingKey> {
    let raw = match env::var(TRUSTED_MANIFEST_KEYS_ENV) {
        Ok(value) => value,
        Err(_) => return Vec::new(),
    };

    raw.split(',')
        .map(str::trim)
        .filter(|entry| !entry.is_empty())
        .filter_map(|entry| {
            let decoded = BASE64_STANDARD.decode(entry).ok()?;
            let bytes: [u8; 32] = decoded.try_into().ok()?;
            VerifyingKey::from_bytes(&bytes).ok()
        })
        .collect()
}

impl Default for TrustStore {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::TrustStore;
    use crate::manifest::{AgentManifest, SignedManifest};

    #[test]
    fn unsigned_manifest_above_ceiling_is_rejected() {
        let manifest = SignedManifest {
            manifest: AgentManifest {
                schema_version: 1,
                id: "third_party".into(),
                description: "test".into(),
                requested_capabilities: vec!["fs:write".into()],
                min_kernel_version: "0.1.0".into(),
            },
            signature: None,
        };

        let store = TrustStore::new();
        assert!(store.assess_signed_manifest(&manifest).is_err());
    }

    #[test]
    fn builtin_manifest_is_trusted() {
        let store = TrustStore::new();
        let assessment = store
            .assess_builtin_manifest(AgentManifest {
                schema_version: 1,
                id: "coder".into(),
                description: "code".into(),
                requested_capabilities: vec!["fs:write".into()],
                min_kernel_version: "0.1.0".into(),
            })
            .expect("builtin manifest");
        assert!(assessment.trusted);
    }
}
