//! Runner identity and state management.

use nostr::prelude::*;
use serde::{Deserialize, Serialize};

/// Runner identity derived from nsec
#[derive(Debug, Clone)]
pub struct RunnerIdentity {
    keys: Keys,
    pub name: String,
    pub tags: Vec<String>,
}

impl RunnerIdentity {
    /// Create runner identity from nsec
    pub fn from_nsec(nsec: &str, name: String, tags: Vec<String>) -> anyhow::Result<Self> {
        let keys = Keys::parse(nsec)?;
        Ok(Self { keys, name, tags })
    }

    /// Generate a new runner identity
    pub fn generate(name: String, tags: Vec<String>) -> Self {
        let keys = Keys::generate();
        Self { keys, name, tags }
    }

    /// Get npub (public key in bech32)
    pub fn npub(&self) -> String {
        self.keys.public_key().to_bech32().expect("valid bech32")
    }

    /// Get nsec (private key in bech32)
    pub fn nsec(&self) -> String {
        self.keys.secret_key().to_bech32().expect("valid bech32")
    }

    /// Sign data with runner's key
    pub fn sign(&self, data: &[u8]) -> anyhow::Result<String> {
        use sha2::{Digest, Sha256};

        let hash = Sha256::digest(data);
        let message = nostr::secp256k1::Message::from_digest(hash.into());
        let secp = nostr::secp256k1::Secp256k1::new();
        let keypair = self.keys.secret_key().keypair(&secp);
        let sig = secp.sign_schnorr(&message, &keypair);
        Ok(sig.to_string())
    }
}

/// Runner status for coordination
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunnerStatus {
    pub npub: String,
    pub name: String,
    pub tags: Vec<String>,
    pub state: RunnerState,
    pub current_jobs: u32,
    pub max_jobs: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunnerState {
    /// Ready to accept jobs
    Idle,
    /// Currently processing jobs
    Busy,
    /// Not accepting new jobs
    Offline,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_generate_identity() {
        let identity = RunnerIdentity::generate("test-runner".to_string(), vec!["linux".to_string()]);
        assert!(identity.npub().starts_with("npub1"));
        assert!(identity.nsec().starts_with("nsec1"));
    }

    #[test]
    fn test_from_nsec() {
        let identity = RunnerIdentity::generate("test".to_string(), vec![]);
        let nsec = identity.nsec();

        let restored = RunnerIdentity::from_nsec(&nsec, "test".to_string(), vec![]).unwrap();
        assert_eq!(identity.npub(), restored.npub());
    }
}
