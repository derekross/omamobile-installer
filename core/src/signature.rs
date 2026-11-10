// SPDX-License-Identifier: MIT
//! The release key's detached OpenPGP signature (`SHA256SUMS.sig`).
//!
//! Done with rPGP (the `pgp` crate, MIT OR Apache-2.0, pure Rust), which
//! builds for native targets and for `wasm32-unknown-unknown` alike. Only
//! verification is used: no random numbers, no clock (the caller passes none;
//! validity is judged at the signature's own creation time, see below).
//!
//! Trust is the primary key's fingerprint, built into the installer
//! ([`OMAMOBILE_KEYS`]). The certificate itself may come from the bundle
//! (`omamobile.gpg`): a certificate with another fingerprint is ignored, and
//! one with a trusted fingerprint is the trusted key by definition.
use crate::{Error, Result};

/// The primary fingerprints `tools/install-phone` accepts: the release key,
/// and the original package key until it is retired.
pub const OMAMOBILE_KEYS: [&str; 2] = ["20567593566EEEE2DD8864608632C8075EB94673", "6C84FE3D240709E70E27CC94B301F5EA62B249FE"];

/// Primary key fingerprints (40 upper-case hex digits) that may sign a release.
#[derive(Debug, Clone)]
pub struct TrustedKeys(Vec<String>);

impl TrustedKeys {
    pub fn omamobile() -> Self {
        Self(OMAMOBILE_KEYS.iter().map(|s| s.to_string()).collect())
    }
    pub fn new<S: AsRef<str>>(fingerprints: &[S]) -> Result<Self> {
        let mut out = Vec::new();
        for f in fingerprints {
            let f = f.as_ref().to_ascii_uppercase();
            if f.len() != 40 || !f.bytes().all(|b| b.is_ascii_hexdigit()) {
                return Err(Error::Signature("a trusted key is named by its 40-digit fingerprint".into()));
            }
            out.push(f);
        }
        Ok(Self(out))
    }
    pub fn contains(&self, fingerprint: &str) -> bool {
        self.0.iter().any(|f| f.eq_ignore_ascii_case(fingerprint))
    }
}

/// Who signed, for the UI and the log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Signer {
    /// The trusted primary key's fingerprint.
    pub primary: String,
    /// The key that made the signature (the primary or its signing subkey).
    pub signing_key: String,
    /// When the signature says it was made (seconds since 1970).
    pub created: u32,
    /// When the signing key expires, if it does.
    pub key_expires: Option<u64>,
}

#[cfg(feature = "openpgp")]
pub fn verify_detached(keyring: &[u8], signature: &[u8], data: &[u8], trusted: &TrustedKeys) -> Result<Signer> {
    use pgp::composed::{Deserializable, DetachedSignature, SignedPublicKey};
    use pgp::crypto::hash::HashAlgorithm;
    use pgp::packet::SignatureType;
    use pgp::types::KeyDetails;

    let sig_err = |why: String| Error::Signature(why);
    let (sig, _) = DetachedSignature::from_reader_single(signature).map_err(|e| sig_err(format!("not an OpenPGP signature ({e})")))?;
    let s = &sig.signature;
    if s.typ() != Some(SignatureType::Binary) {
        return Err(sig_err("not a signature over a binary document".into()));
    }
    if !matches!(s.hash_alg(), Some(HashAlgorithm::Sha256 | HashAlgorithm::Sha384 | HashAlgorithm::Sha512)) {
        return Err(sig_err("made with a hash this installer doesn't accept".into()));
    }
    let created = s.created().ok_or_else(|| sig_err("carries no creation time".into()))?.as_secs();
    let issuers: Vec<String> = s.issuer_fingerprint().iter().map(|f| format!("{f:X}")).collect();

    // Binary or ASCII-armored (the bundle's omamobile.gpg is armored)
    let (certs, _) = SignedPublicKey::from_reader_many(keyring).map_err(|e| sig_err(format!("unreadable keyring ({e})")))?;
    let mut seen_trusted = false;
    for cert in certs {
        let Ok(cert) = cert else { continue };
        let primary = format!("{:X}", cert.primary_key.fingerprint());
        if !trusted.contains(&primary) {
            continue;
        }
        seen_trusted = true;
        // Self-signatures and subkey bindings (with the subkey's own
        // back-signature) must all hold, and a revoked key is never accepted.
        if cert.verify_bindings().is_err() {
            return Err(sig_err(format!("key {primary} has an invalid self-signature or subkey binding")));
        }
        if !cert.details.revocation_signatures.is_empty() {
            return Err(sig_err(format!("key {primary} is revoked")));
        }

        // The primary, when its user ID signatures allow signing
        let primary_signs = cert.details.users.iter().flat_map(|u| &u.signatures).any(|b| b.key_flags().sign());
        if primary_signs && (issuers.is_empty() || issuers.contains(&primary)) && sig.verify(&cert.primary_key, data).is_ok() {
            let expires = cert.details.users.iter().flat_map(|u| &u.signatures).filter_map(|b| b.key_expiration_time()).map(|d| d.as_secs() as u64).filter(|d| *d > 0).min().map(|d| cert.primary_key.created_at().as_secs() as u64 + d);
            return finish(primary.clone(), primary, cert.primary_key.created_at().as_secs(), created, expires);
        }
        for sub in &cert.public_subkeys {
            let fpr = format!("{:X}", sub.key.fingerprint());
            if !(issuers.is_empty() || issuers.contains(&fpr)) {
                continue;
            }
            if sub.signatures.iter().any(|b| b.typ() == Some(SignatureType::SubkeyRevocation)) {
                return Err(sig_err(format!("signing subkey {fpr} is revoked")));
            }
            if !sub.signatures.iter().any(|b| b.key_flags().sign()) {
                continue;
            }
            if sig.verify(&sub.key, data).is_ok() {
                let key_created = sub.key.created_at().as_secs();
                let expires = sub.signatures.iter().filter_map(|b| b.key_expiration_time()).map(|d| d.as_secs() as u64).filter(|d| *d > 0).min().map(|d| key_created as u64 + d);
                return finish(primary, fpr, key_created, created, expires);
            }
        }
    }
    if seen_trusted {
        Err(sig_err("the signature does not verify with a trusted key".into()))
    } else {
        Err(sig_err("not signed by an Omarchy Mobile key: the keyring holds none of the trusted fingerprints".into()))
    }
}

/// Validity at the signature's own time: made after the key, and before the
/// key expired. A release signed while its key was valid stays installable
/// after the key expires; whether a front end also compares with today's
/// date is a policy decision left to it ([`Signer::key_expires`]).
#[cfg(feature = "openpgp")]
fn finish(primary: String, signing_key: String, key_created: u32, created: u32, key_expires: Option<u64>) -> Result<Signer> {
    if created < key_created {
        return Err(Error::Signature("the signature is older than its key".into()));
    }
    if key_expires.is_some_and(|e| created as u64 >= e) {
        return Err(Error::Signature("the signature was made after its key expired".into()));
    }
    Ok(Signer { primary, signing_key, created, key_expires })
}

/// Built without the `openpgp` feature (only to measure its size): nothing
/// verifies, so nothing can be installed.
#[cfg(not(feature = "openpgp"))]
pub fn verify_detached(_keyring: &[u8], _signature: &[u8], _data: &[u8], _trusted: &TrustedKeys) -> Result<Signer> {
    Err(Error::Signature("this build has no OpenPGP support".into()))
}
