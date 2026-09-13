//! The handshake, and the cipher that protects everything after it.
//!
//! This link carries keystrokes. Plaintext on a LAN would be a keylogger anyone
//! on the same Wi-Fi could read, so the transport is encrypted from the first
//! frame after the handshake, not as a later hardening pass.
//!
//! The shape is standard and deliberately boring:
//!
//! 1. Each side sends a `Hello` in the clear carrying an **ephemeral** X25519
//!    public key, a random nonce, and its long-term Ed25519 identity key.
//! 2. The `Hello` is signed by that identity key over the ephemeral key and the
//!    nonce. Signing the ephemeral key is what stops a machine in the middle
//!    from swapping in its own: an attacker cannot forge the signature without
//!    the identity key, which never leaves the machine that owns it.
//! 3. Both sides derive the same secret with X25519, run it through HKDF along
//!    with both nonces, and split the output into one key per direction.
//!
//! Ephemeral keys mean a stolen identity key cannot decrypt yesterday's
//! recording of the link — only impersonate the machine from now on, which
//! unpairing fixes.
//!
//! What this does *not* do is authorise. The handshake proves "this really is
//! the holder of key K". Whether key K belongs in the workspace is a separate
//! question, answered against the paired records in the workspace document.

use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{ChaCha20Poly1305, Key, Nonce};
use hkdf::Hkdf;
use rand::rngs::OsRng;
use rand::RngCore;
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use x25519_dalek::{EphemeralSecret, PublicKey as X25519Public};

use is_core::{Identity, MachineId, PubKey};

use crate::{Error, Result};

/// Bumped whenever the wire format changes in a way older builds cannot read.
pub const PROTOCOL_VERSION: u16 = 1;

/// Domain separation, so a signature made here can never be replayed as a
/// signature for something else.
const SIGNING_CONTEXT: &[u8] = b"inputshare-handshake-v1";
const HKDF_INFO: &[u8] = b"inputshare-session-v1";

/// The one message sent in the clear. Everything after it is encrypted.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Hello {
    pub protocol: u16,
    pub machine_id: MachineId,
    pub identity_key: PubKey,
    /// Ephemeral X25519 public key, discarded when the connection ends.
    pub ephemeral: [u8; 32],
    pub nonce: [u8; 32],
    #[serde(with = "sig64")]
    pub signature: [u8; 64],
}

impl Hello {
    fn transcript(machine_id: MachineId, ephemeral: &[u8; 32], nonce: &[u8; 32]) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(SIGNING_CONTEXT.len() + 16 + 32 + 32);
        bytes.extend_from_slice(SIGNING_CONTEXT);
        bytes.extend_from_slice(machine_id.as_bytes());
        bytes.extend_from_slice(ephemeral);
        bytes.extend_from_slice(nonce);
        bytes
    }

    /// Whether this `Hello` was really produced by the holder of the key it
    /// claims. Says nothing about whether that key is trusted.
    pub fn verify(&self) -> bool {
        if self.protocol != PROTOCOL_VERSION {
            return false;
        }
        let transcript = Self::transcript(self.machine_id, &self.ephemeral, &self.nonce);
        self.identity_key.verify(&transcript, &self.signature)
    }
}

/// Our half of a handshake in progress.
pub struct Initiation {
    pub hello: Hello,
    secret: EphemeralSecret,
    nonce: [u8; 32],
}

/// Starts a handshake, minting a fresh ephemeral key for this connection.
pub fn begin(identity: &Identity) -> Initiation {
    let secret = EphemeralSecret::random_from_rng(OsRng);
    let ephemeral = X25519Public::from(&secret).to_bytes();
    let mut nonce = [0u8; 32];
    OsRng.fill_bytes(&mut nonce);

    let transcript = Hello::transcript(identity.machine_id, &ephemeral, &nonce);
    Initiation {
        hello: Hello {
            protocol: PROTOCOL_VERSION,
            machine_id: identity.machine_id,
            identity_key: identity.public_key(),
            ephemeral,
            nonce,
            signature: identity.sign(&transcript),
        },
        secret,
        nonce,
    }
}

impl Initiation {
    /// Completes the handshake against the peer's `Hello`.
    ///
    /// `dialled` says whether we opened the connection. Both sides must agree,
    /// or they would pick the same key for both directions.
    pub fn complete(self, peer: &Hello, dialled: bool) -> Result<Session> {
        if !peer.verify() {
            return Err(Error::Handshake("peer could not prove its identity key"));
        }
        if peer.machine_id == self.hello.machine_id {
            return Err(Error::Handshake("peer reports our own machine id"));
        }

        let shared = self
            .secret
            .diffie_hellman(&X25519Public::from(peer.ephemeral));

        // Both nonces in a fixed order, so the two sides salt identically.
        let (first, second) = if dialled {
            (self.nonce, peer.nonce)
        } else {
            (peer.nonce, self.nonce)
        };
        let mut salt = [0u8; 64];
        salt[..32].copy_from_slice(&first);
        salt[32..].copy_from_slice(&second);

        let hkdf = Hkdf::<Sha256>::new(Some(&salt), shared.as_bytes());
        let mut keys = [0u8; 64];
        hkdf.expand(HKDF_INFO, &mut keys)
            .map_err(|_| Error::Handshake("key derivation failed"))?;

        // The dialling side takes the first key to send with; the accepting
        // side takes it to receive with.
        let (send, receive) = if dialled {
            (&keys[..32], &keys[32..])
        } else {
            (&keys[32..], &keys[..32])
        };

        Ok(Session {
            peer_machine: peer.machine_id,
            peer_key: peer.identity_key,
            send: Direction::new(send),
            receive: Direction::new(receive),
        })
    }
}

struct Direction {
    cipher: ChaCha20Poly1305,
    counter: u64,
}

impl Direction {
    fn new(key: &[u8]) -> Self {
        Self {
            cipher: ChaCha20Poly1305::new(Key::from_slice(key)),
            counter: 0,
        }
    }

    /// A counter nonce. Never reused, because each direction has its own key
    /// and the counter only moves forward within one connection.
    fn next_nonce(&mut self) -> Nonce {
        let mut bytes = [0u8; 12];
        bytes[4..].copy_from_slice(&self.counter.to_be_bytes());
        self.counter += 1;
        *Nonce::from_slice(&bytes)
    }
}

/// An established, mutually authenticated, encrypted channel.
pub struct Session {
    pub peer_machine: MachineId,
    pub peer_key: PubKey,
    send: Direction,
    receive: Direction,
}

impl Session {
    /// Splits into the two halves, so reading and writing can run as separate
    /// tasks.
    ///
    /// They have to be separate: framed reads are not cancellation-safe, so a
    /// single task selecting over "read a frame" and "write a frame" would lose
    /// half a frame the first time a write won the race.
    pub fn split(self) -> (Sealer, Opener) {
        (Sealer(self.send), Opener(self.receive))
    }
}

/// The outbound half.
pub struct Sealer(Direction);

impl Sealer {
    pub fn seal(&mut self, plaintext: &[u8]) -> Result<Vec<u8>> {
        let nonce = self.0.next_nonce();
        self.0
            .cipher
            .encrypt(
                &nonce,
                Payload {
                    msg: plaintext,
                    aad: &[],
                },
            )
            .map_err(|_| Error::Crypto("could not encrypt frame"))
    }
}

/// The inbound half.
pub struct Opener(Direction);

impl Opener {
    /// Fails if a frame was altered, replayed, reordered or dropped: the
    /// counter has to line up exactly, so the stream is protected as a
    /// sequence and not merely frame by frame.
    pub fn open(&mut self, ciphertext: &[u8]) -> Result<Vec<u8>> {
        let nonce = self.0.next_nonce();
        self.0
            .cipher
            .decrypt(
                &nonce,
                Payload {
                    msg: ciphertext,
                    aad: &[],
                },
            )
            .map_err(|_| Error::Crypto("frame failed authentication"))
    }
}

mod sig64 {
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(bytes: &[u8; 64], s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&hex_encode(bytes))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<[u8; 64], D::Error> {
        use serde::de::Error as _;
        let text = String::deserialize(d)?;
        if text.len() != 128 {
            return Err(D::Error::custom("expected a 64-byte signature"));
        }
        let mut out = [0u8; 64];
        for (i, byte) in out.iter_mut().enumerate() {
            *byte = u8::from_str_radix(&text[i * 2..i * 2 + 2], 16).map_err(D::Error::custom)?;
        }
        Ok(out)
    }

    fn hex_encode(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }
}
