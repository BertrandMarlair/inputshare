//! Permanent machine identity (requirement 1).
//!
//! The identity is generated once, on first launch, and then never changes. It
//! is deliberately independent of IP address, hostname, MAC address and network
//! interface: those are runtime metadata, and a machine that moves from Ethernet
//! to Wi-Fi is still the same machine.
//!
//! Alongside the ID, each installation holds an Ed25519 key pair. The public
//! half is what peers record when they pair, so a reconnecting machine proves it
//! is the machine it claims to be rather than merely asserting an ID.

use std::fmt;

use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use rand::rngs::OsRng;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{Error, MachineId, Result};

/// An Ed25519 public key, serialized as hex.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct PubKey([u8; 32]);

impl PubKey {
    pub fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    pub fn to_hex(&self) -> String {
        hex::encode(self.0)
    }

    pub fn from_hex(s: &str) -> Result<Self> {
        let raw = hex::decode(s).map_err(|e| Error::Hex(e.to_string()))?;
        let bytes = <[u8; 32]>::try_from(raw.as_slice()).map_err(|_| Error::KeyLength {
            expected: 32,
            got: raw.len(),
        })?;
        Ok(Self(bytes))
    }

    /// Short form for logs and for the pairing dialog, where a human compares
    /// two screens.
    pub fn fingerprint(&self) -> String {
        hex::encode(&self.0[..4])
    }

    fn verifying_key(&self) -> Result<VerifyingKey> {
        VerifyingKey::from_bytes(&self.0).map_err(|e| Error::BadKey(e.to_string()))
    }

    /// Whether `sig` is this key's signature over `msg`.
    pub fn verify(&self, msg: &[u8], sig: &[u8; 64]) -> bool {
        match self.verifying_key() {
            Ok(vk) => vk.verify(msg, &Signature::from_bytes(sig)).is_ok(),
            Err(_) => false,
        }
    }
}

impl fmt::Debug for PubKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "PubKey({}…)", self.fingerprint())
    }
}

impl fmt::Display for PubKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_hex())
    }
}

impl Serialize for PubKey {
    fn serialize<S: serde::Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        s.serialize_str(&self.to_hex())
    }
}

impl<'de> Deserialize<'de> for PubKey {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        use serde::de::Error as _;
        let s = String::deserialize(d)?;
        Self::from_hex(&s).map_err(D::Error::custom)
    }
}

/// This installation's permanent identity. Written once, read every boot.
#[derive(Clone, Serialize, Deserialize)]
pub struct Identity {
    pub machine_id: MachineId,
    pub created_at_ms: u64,
    #[serde(with = "hex32")]
    secret_key: [u8; 32],
}

impl Identity {
    /// Mints a new identity. Called on first launch, and after a reinstall —
    /// requirement 14 treats a fresh install as a new machine rather than
    /// silently inheriting the old one's trust.
    pub fn generate() -> Self {
        let signing_key = SigningKey::generate(&mut OsRng);
        Self {
            machine_id: Uuid::new_v4(),
            created_at_ms: crate::now_ms(),
            secret_key: signing_key.to_bytes(),
        }
    }

    fn signing_key(&self) -> SigningKey {
        SigningKey::from_bytes(&self.secret_key)
    }

    pub fn public_key(&self) -> PubKey {
        PubKey(self.signing_key().verifying_key().to_bytes())
    }

    pub fn sign(&self, msg: &[u8]) -> [u8; 64] {
        self.signing_key().sign(msg).to_bytes()
    }
}

/// Hand-written so the secret key cannot reach a log line.
impl fmt::Debug for Identity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Identity")
            .field("machine_id", &self.machine_id)
            .field("public_key", &self.public_key())
            .field("secret_key", &"<redacted>")
            .finish()
    }
}

mod hex32 {
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(bytes: &[u8; 32], s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&hex::encode(bytes))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<[u8; 32], D::Error> {
        use serde::de::Error as _;
        let s = String::deserialize(d)?;
        let raw = hex::decode(&s).map_err(D::Error::custom)?;
        <[u8; 32]>::try_from(raw.as_slice()).map_err(|_| D::Error::custom("expected 32 bytes"))
    }
}
