// SPDX-License-Identifier: MIT
//! The chain of trust on the real thing: the small files of the published
//! cheetah 2026.11.08 release, checked with the fingerprints built into the
//! core. (The boot images and the root image are not fetched.)
use omamobile_installer_core::bundle::{verify_release, ReleaseFiles};
use omamobile_installer_core::signature::{verify_detached, TrustedKeys};
use omamobile_installer_core::zstd_push::frame_info;
use omamobile_installer_core::Error;

const SUMS: &[u8] = include_bytes!("../../fixtures/published/SHA256SUMS");
const SIG: &[u8] = include_bytes!("../../fixtures/published/SHA256SUMS.sig");
const KEYRING: &[u8] = include_bytes!("../../fixtures/published/omamobile.gpg");
const MANIFEST_JSON: &[u8] = include_bytes!("../../fixtures/published/manifest.json");
const MANIFEST_TXT: &[u8] = include_bytes!("../../fixtures/published/MANIFEST");
const DEVICE_CONF: &[u8] = include_bytes!("../../fixtures/published/device.conf");
const ROOT_HEAD: &[u8] = include_bytes!("../../fixtures/published/rootfs-head.bin");

fn files() -> ReleaseFiles<'static> {
    ReleaseFiles { sha256sums: SUMS, sha256sums_sig: SIG, keyring: KEYRING, manifest_json: MANIFEST_JSON, manifest_txt: MANIFEST_TXT, device_conf: DEVICE_CONF }
}

#[test]
fn the_published_release_verifies_with_the_built_in_fingerprints() {
    let release = verify_release(&files(), &TrustedKeys::omamobile()).unwrap();
    let signer = release.signer();
    // Signed by the release key's signing subkey; trust is the primary
    assert_eq!(signer.primary, "20567593566EEEE2DD8864608632C8075EB94673");
    assert_ne!(signer.signing_key, signer.primary);
    assert!(signer.key_expires.is_some_and(|e| e > signer.created as u64));
    assert_eq!(release.manifest().version, "2026.11.08");
    release.check_file("device.conf", DEVICE_CONF).unwrap();
    assert!(release.check_file("device.conf", MANIFEST_TXT).is_err());
}

#[test]
fn a_changed_byte_anywhere_is_refused() {
    let flip = |data: &[u8], at: usize| {
        let mut v = data.to_vec();
        v[at] ^= 1;
        v
    };
    let trusted = TrustedKeys::omamobile();
    let sums = flip(SUMS, 10);
    assert!(matches!(verify_release(&ReleaseFiles { sha256sums: &sums, ..files() }, &trusted), Err(Error::Signature(_))));
    let sig = flip(SIG, SIG.len() - 5);
    assert!(matches!(verify_release(&ReleaseFiles { sha256sums_sig: &sig, ..files() }, &trusted), Err(Error::Signature(_))));
    let manifest = flip(MANIFEST_JSON, 200);
    assert!(matches!(verify_release(&ReleaseFiles { manifest_json: &manifest, ..files() }, &trusted), Err(Error::Checksum(_))));
    let txt = flip(MANIFEST_TXT, 40);
    assert!(matches!(verify_release(&ReleaseFiles { manifest_txt: &txt, ..files() }, &trusted), Err(Error::Checksum(_))));
    let conf = flip(DEVICE_CONF, 40);
    assert!(matches!(verify_release(&ReleaseFiles { device_conf: &conf, ..files() }, &trusted), Err(Error::Checksum(_))));
}

#[test]
fn a_key_that_is_not_trusted_signs_nothing() {
    // The same valid signature, but the installer trusts another fingerprint
    let other = TrustedKeys::new(&["0000000000000000000000000000000000000001"]).unwrap();
    assert!(matches!(verify_detached(KEYRING, SIG, SUMS, &other), Err(Error::Signature(_))));
    // An empty keyring, and one that isn't a keyring
    assert!(matches!(verify_detached(&[], SIG, SUMS, &TrustedKeys::omamobile()), Err(Error::Signature(_))));
    assert!(matches!(verify_detached(SUMS, SIG, SUMS, &TrustedKeys::omamobile()), Err(Error::Signature(_))));
    assert!(TrustedKeys::new(&["7F1041BF47291504"]).is_err(), "a key ID is not a fingerprint");
}

#[test]
fn the_published_root_image_states_its_size() {
    // The first 32 bytes of the published .img.zst (an HTTP range request)
    let info = frame_info(ROOT_HEAD).unwrap().unwrap();
    assert_eq!(info.content_size, Some(12 << 30));
    assert!(info.has_checksum);
}
