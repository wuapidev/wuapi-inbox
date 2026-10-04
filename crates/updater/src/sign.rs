//! The signature on a manifest.
//!
//! Ed25519, in [minisign](https://jedisct1.github.io/minisign/)'s format,
//! so a manifest signed here is verified by `minisign -V` and the other
//! way round. A signature file is four lines:
//!
//! ```text
//! untrusted comment: <anything>
//! base64("ED" || key id (8) || Ed25519(BLAKE2b-512(file)) (64))
//! trusted comment: <text>
//! base64(Ed25519(signature (64) || trusted comment) (64))
//! ```
//!
//! and a public key is `base64("Ed" || key id (8) || key (32))`. The
//! secret key is this tool's own (`base64("SK" || key id (8) || seed (32))`,
//! not minisign's password-protected file): it lives in a CI secret and
//! nowhere else.
//!
//! The application embeds public keys only ([`EMBEDDED_KEYS`]). TLS is not
//! what is trusted: a manifest that this does not verify is never acted on.

use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine as _;
use blake2::{Blake2b512, Digest as _};
use ed25519_dalek::{Signer as _, SigningKey, Verifier as _, VerifyingKey};

/// What stands in [`EMBEDDED_KEYS`] until a real key is made. While no
/// entry is a real key the updater is disabled.
pub const PLACEHOLDER_KEY: &str = "REPLACE-WITH-THE-PUBLIC-KEY-PRINTED-BY-XTASK-KEYGEN";

/// The public keys a manifest's signature is checked against: the output
/// of `cargo xtask keygen`.
///
/// Two places, so that a key can be rotated: put the new key in the free
/// place, ship that version, sign with the new key from then on, and take
/// the old key out once the versions that only know it are below the
/// manifest's `min_supported`. An empty string or the placeholder is no key.
pub const EMBEDDED_KEYS: [&str; 2] = [
    "RWRVdlGKQ7U8Qt6/plGIyPhhlYX5OCxTVSONvkvJAB+JuPbqirPP6Mug",
    "",
];

/// What went wrong with a key or a signature.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum SignError {
    /// The text is not a key of this format.
    #[error("not a valid key: {0}")]
    BadKey(&'static str),
    /// The text is not a signature file of this format.
    #[error("not a valid signature: {0}")]
    BadSignature(&'static str),
    /// None of the keys given made this signature.
    #[error("the signature was made by a key that is not trusted (key id {0})")]
    UnknownKey(String),
    /// The signature is not this key's signature of these bytes.
    #[error("the signature does not match")]
    Mismatch,
}

/// A key's public half.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PublicKey {
    id: [u8; 8],
    key: VerifyingKey,
}

/// A key's secret half. Never written to disk by this crate.
pub struct SecretKey {
    id: [u8; 8],
    key: SigningKey,
}

/// What a verified signature says.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Verified {
    /// The comment the signer vouched for.
    pub trusted_comment: String,
    /// The id of the key that signed.
    pub key_id: String,
}

fn id_text(id: [u8; 8]) -> String {
    format!("{:016X}", u64::from_le_bytes(id))
}

/// The one line of base64 in a key or the body of a signature file: what
/// is left once the comment lines are put aside.
fn decode(line: &str, wrong: SignError) -> Result<Vec<u8>, SignError> {
    BASE64.decode(line.trim()).map_err(|_| wrong)
}

impl PublicKey {
    /// Reads a public key: the base64 line, with or without minisign's
    /// "untrusted comment" line before it.
    pub fn parse(text: &str) -> Result<Self, SignError> {
        let line = text
            .lines()
            .map(str::trim)
            .find(|line| !line.is_empty() && !line.starts_with("untrusted comment:"))
            .ok_or(SignError::BadKey("empty"))?;
        let bytes = decode(line, SignError::BadKey("not base64"))?;
        if bytes.len() != 42 {
            return Err(SignError::BadKey("wrong length"));
        }
        if &bytes[..2] != b"Ed" {
            return Err(SignError::BadKey("not an Ed25519 public key"));
        }
        let id: [u8; 8] = bytes[2..10].try_into().expect("eight bytes");
        let key: [u8; 32] = bytes[10..].try_into().expect("thirty-two bytes");
        let key =
            VerifyingKey::from_bytes(&key).map_err(|_| SignError::BadKey("not a curve point"))?;
        Ok(Self { id, key })
    }

    /// The key as the one line that goes into [`EMBEDDED_KEYS`].
    pub fn to_text(&self) -> String {
        let mut bytes = Vec::with_capacity(42);
        bytes.extend_from_slice(b"Ed");
        bytes.extend_from_slice(&self.id);
        bytes.extend_from_slice(self.key.as_bytes());
        BASE64.encode(bytes)
    }

    /// The key's id, as minisign prints it.
    pub fn id(&self) -> String {
        id_text(self.id)
    }
}

impl SecretKey {
    /// A new key from the operating system's generator.
    pub fn generate() -> Result<Self, SignError> {
        let mut seed = [0u8; 32];
        let mut id = [0u8; 8];
        getrandom::fill(&mut seed).map_err(|_| SignError::BadKey("no randomness"))?;
        getrandom::fill(&mut id).map_err(|_| SignError::BadKey("no randomness"))?;
        let key = SigningKey::from_bytes(&seed);
        seed.fill(0);
        Ok(Self { id, key })
    }

    /// Reads a secret key as [`SecretKey::to_text`] wrote it.
    pub fn parse(text: &str) -> Result<Self, SignError> {
        let mut bytes = decode(text, SignError::BadKey("not base64"))?;
        if bytes.len() != 42 {
            bytes.fill(0);
            return Err(SignError::BadKey("wrong length"));
        }
        if &bytes[..2] != b"SK" {
            bytes.fill(0);
            return Err(SignError::BadKey("not a secret key of this tool"));
        }
        let id: [u8; 8] = bytes[2..10].try_into().expect("eight bytes");
        let mut seed: [u8; 32] = bytes[10..].try_into().expect("thirty-two bytes");
        bytes.fill(0);
        let key = SigningKey::from_bytes(&seed);
        seed.fill(0);
        Ok(Self { id, key })
    }

    /// The secret key as one line: what goes into the CI secret.
    pub fn to_text(&self) -> String {
        let mut bytes = Vec::with_capacity(42);
        bytes.extend_from_slice(b"SK");
        bytes.extend_from_slice(&self.id);
        bytes.extend_from_slice(&self.key.to_bytes());
        let text = BASE64.encode(&bytes);
        bytes.fill(0);
        text
    }

    /// The public half.
    pub fn public(&self) -> PublicKey {
        PublicKey {
            id: self.id,
            key: self.key.verifying_key(),
        }
    }
}

/// Signs `message`. Returns the text of the signature file.
///
/// `trusted_comment` is covered by the signature; it must be one line.
pub fn sign(secret: &SecretKey, message: &[u8], trusted_comment: &str) -> String {
    let comment: String = trusted_comment
        .chars()
        .filter(|c| *c != '\n' && *c != '\r')
        .collect();
    let digest = Blake2b512::digest(message);
    let signature = secret.key.sign(&digest).to_bytes();

    let mut body = Vec::with_capacity(74);
    body.extend_from_slice(b"ED");
    body.extend_from_slice(&secret.id);
    body.extend_from_slice(&signature);

    let mut global = Vec::with_capacity(64 + comment.len());
    global.extend_from_slice(&signature);
    global.extend_from_slice(comment.as_bytes());
    let global = secret.key.sign(&global).to_bytes();

    format!(
        "untrusted comment: signature from wuapi Inbox release key {}\n{}\ntrusted comment: {}\n{}\n",
        id_text(secret.id),
        BASE64.encode(body),
        comment,
        BASE64.encode(global),
    )
}

/// Checks that `signature` (the text of a signature file) is the
/// signature of `message` by one of `keys`.
pub fn verify(keys: &[PublicKey], message: &[u8], signature: &str) -> Result<Verified, SignError> {
    let mut lines = signature.lines();
    let mut next = |missing| lines.next().ok_or(SignError::BadSignature(missing));
    let untrusted = next("no comment line")?;
    if !untrusted.starts_with("untrusted comment:") {
        return Err(SignError::BadSignature("no comment line"));
    }
    let body = decode(
        next("no signature line")?,
        SignError::BadSignature("not base64"),
    )?;
    let comment = next("no trusted comment")?
        .strip_prefix("trusted comment: ")
        .ok_or(SignError::BadSignature("no trusted comment"))?;
    let global = decode(
        next("no global signature")?,
        SignError::BadSignature("not base64"),
    )?;
    if body.len() != 74 || global.len() != 64 {
        return Err(SignError::BadSignature("wrong length"));
    }
    // "ED": the file was hashed first, which is what minisign writes
    // today. "Ed" (the file itself was signed) is what it wrote before.
    let prehashed = match &body[..2] {
        b"ED" => true,
        b"Ed" => false,
        _ => return Err(SignError::BadSignature("unknown algorithm")),
    };
    let id: [u8; 8] = body[2..10].try_into().expect("eight bytes");
    let key = keys
        .iter()
        .find(|key| key.id == id)
        .ok_or_else(|| SignError::UnknownKey(id_text(id)))?;

    let signature: [u8; 64] = body[10..].try_into().expect("sixty-four bytes");
    let signed = ed25519_dalek::Signature::from_bytes(&signature);
    let matches = if prehashed {
        key.key.verify(&Blake2b512::digest(message), &signed)
    } else {
        key.key.verify(message, &signed)
    };
    matches.map_err(|_| SignError::Mismatch)?;

    let mut covered = Vec::with_capacity(64 + comment.len());
    covered.extend_from_slice(&signature);
    covered.extend_from_slice(comment.as_bytes());
    let global: [u8; 64] = global.try_into().expect("sixty-four bytes");
    key.key
        .verify(&covered, &ed25519_dalek::Signature::from_bytes(&global))
        .map_err(|_| SignError::Mismatch)?;

    Ok(Verified {
        trusted_comment: comment.to_owned(),
        key_id: key.id(),
    })
}

/// The keys in [`EMBEDDED_KEYS`] that are keys. Empty while only the
/// placeholder is there: the updater is then disabled.
pub fn embedded_keys() -> Vec<PublicKey> {
    keys_of(&EMBEDDED_KEYS)
}

/// The entries of `texts` that are keys; the placeholder and empty places
/// are passed over, and an entry that is neither is logged and passed over.
pub fn keys_of(texts: &[&str]) -> Vec<PublicKey> {
    texts
        .iter()
        .filter(|text| !text.is_empty() && **text != PLACEHOLDER_KEY)
        .filter_map(|text| match PublicKey::parse(text) {
            Ok(key) => Some(key),
            Err(error) => {
                tracing::error!(%error, "an embedded update key is not a key");
                None
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const MESSAGE: &[u8] = br#"{"version":"1.2.3"}"#;

    #[test]
    fn a_signature_verifies_and_says_what_was_vouched_for() {
        let secret = SecretKey::generate().unwrap();
        let signature = sign(&secret, MESSAGE, "version:1.2.3");
        let verified = verify(&[secret.public()], MESSAGE, &signature).unwrap();
        assert_eq!(verified.trusted_comment, "version:1.2.3");
        assert_eq!(verified.key_id, secret.public().id());
    }

    #[test]
    fn a_changed_file_is_refused() {
        let secret = SecretKey::generate().unwrap();
        let signature = sign(&secret, MESSAGE, "c");
        let mut tampered = MESSAGE.to_vec();
        tampered[12] ^= 1;
        assert_eq!(
            verify(&[secret.public()], &tampered, &signature),
            Err(SignError::Mismatch)
        );
    }

    #[test]
    fn a_changed_trusted_comment_is_refused() {
        let secret = SecretKey::generate().unwrap();
        let signature = sign(&secret, MESSAGE, "version:1.2.3");
        let forged = signature.replace("version:1.2.3", "version:9.9.9");
        assert_eq!(
            verify(&[secret.public()], MESSAGE, &forged),
            Err(SignError::Mismatch)
        );
    }

    #[test]
    fn another_key_is_refused() {
        let (signer, other) = (
            SecretKey::generate().unwrap(),
            SecretKey::generate().unwrap(),
        );
        let signature = sign(&signer, MESSAGE, "c");
        assert!(matches!(
            verify(&[other.public()], MESSAGE, &signature),
            Err(SignError::UnknownKey(_))
        ));
        // Another key that claims the signer's id does not pass either.
        let imposter = PublicKey {
            id: signer.id,
            key: other.key.verifying_key(),
        };
        assert_eq!(
            verify(&[imposter], MESSAGE, &signature),
            Err(SignError::Mismatch)
        );
        assert!(matches!(
            verify(&[], MESSAGE, &signature),
            Err(SignError::UnknownKey(_))
        ));
    }

    #[test]
    fn either_of_two_embedded_keys_verifies() {
        // Rotation: the application knows the old key and the new one,
        // and a manifest signed by either is good.
        let (old, new) = (
            SecretKey::generate().unwrap(),
            SecretKey::generate().unwrap(),
        );
        let (old_text, new_text) = (old.public().to_text(), new.public().to_text());
        let keys = keys_of(&[&old_text, &new_text]);
        assert_eq!(keys.len(), 2);
        assert!(verify(&keys, MESSAGE, &sign(&old, MESSAGE, "c")).is_ok());
        assert!(verify(&keys, MESSAGE, &sign(&new, MESSAGE, "c")).is_ok());
        // Once the old key is taken out, what it signs is refused.
        let keys = keys_of(&["", &new_text]);
        assert!(verify(&keys, MESSAGE, &sign(&old, MESSAGE, "c")).is_err());
        assert!(verify(&keys, MESSAGE, &sign(&new, MESSAGE, "c")).is_ok());
    }

    #[test]
    fn keys_survive_being_written_down() {
        let secret = SecretKey::generate().unwrap();
        let again = SecretKey::parse(&secret.to_text()).unwrap();
        assert_eq!(again.public(), secret.public());
        let public = PublicKey::parse(&secret.public().to_text()).unwrap();
        assert_eq!(public, secret.public());
        // With minisign's comment line too.
        let file = format!(
            "untrusted comment: minisign public key {}\n{}\n",
            public.id(),
            public.to_text()
        );
        assert_eq!(PublicKey::parse(&file).unwrap(), public);
        // A secret key is not taken for a public one, nor the reverse.
        assert!(PublicKey::parse(&secret.to_text()).is_err());
        assert!(SecretKey::parse(&public.to_text()).is_err());
        assert!(PublicKey::parse("").is_err());
        assert!(PublicKey::parse("not a key").is_err());
    }

    #[test]
    fn the_placeholder_is_no_key() {
        assert!(keys_of(&[PLACEHOLDER_KEY, ""]).is_empty());
        assert!(keys_of(&["rubbish"]).is_empty());
    }

    #[test]
    fn damaged_signature_files_are_refused_not_panicked_on() {
        let secret = SecretKey::generate().unwrap();
        let keys = [secret.public()];
        let good = sign(&secret, MESSAGE, "c");
        for bad in [
            "",
            "untrusted comment: x",
            "untrusted comment: x\n!!!\ntrusted comment: c\nAAAA",
            "untrusted comment: x\nAAAA\ntrusted comment: c\nAAAA",
            &good.replace("trusted comment: ", "comment: "),
            &good.lines().take(3).collect::<Vec<_>>().join("\n"),
        ] {
            assert!(verify(&keys, MESSAGE, bad).is_err(), "{bad:?}");
        }
    }

    /// minisign's own verifier (another implementation) accepts what is
    /// signed here, with the key as it is written here.
    #[test]
    fn minisign_verifies_the_signature() {
        let secret = SecretKey::generate().unwrap();
        let signature = sign(&secret, MESSAGE, "timestamp:1\tfile:latest.json");
        let key = minisign_verify::PublicKey::from_base64(&secret.public().to_text()).unwrap();
        let signature = minisign_verify::Signature::decode(&signature).unwrap();
        key.verify(MESSAGE, &signature, false).unwrap();
        assert!(key.verify(b"something else", &signature, false).is_err());
    }
}
