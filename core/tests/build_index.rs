// SPDX-License-Identifier: MIT
//! The build index: every field's rule, on the real cheetah index and on
//! changed copies of it; the match rule; and trust, which comes only
//! through a verified release.
mod support;
use omamobile_installer_core::build_index::{IndexData, Lookup};
use omamobile_installer_core::Error;
use serde_json::{json, Value};
use support::bundle::{TestBundle, PHONE_BUILD};

/// device/cheetah/factory-builds.json as Omarchy Mobile carries it (branch
/// installer-cheetah-index, 2026-11-10): public metadata about Google's builds.
const REAL: &[u8] = include_bytes!("../../fixtures/index/cheetah-factory-builds.json");

fn changed(edit: impl FnOnce(&mut Value)) -> Result<IndexData, Error> {
    let mut v: Value = serde_json::from_slice(REAL).unwrap();
    edit(&mut v);
    IndexData::parse(&serde_json::to_vec(&v).unwrap(), "cheetah")
}

fn refused(edit: impl FnOnce(&mut Value), needle: &str) {
    match changed(edit) {
        Err(Error::BuildIndex(why)) => {
            assert!(why.contains(needle), "refused for another reason: {why}")
        }
        other => panic!("not refused ({needle}): {other:?}"),
    }
}

#[test]
fn the_real_index_is_accepted() {
    let index = IndexData::parse(REAL, "cheetah").unwrap();
    assert_eq!(index.updated, "2026-11-10");
    let ids: Vec<&str> = index.builds.iter().map(|b| b.build.as_str()).collect();
    assert_eq!(ids, ["CP3A.261005.002.A1", "CP2A.260705.006"]);
    let tested: Vec<bool> = index.builds.iter().map(|b| b.tested()).collect();
    assert_eq!(tested, [false, true]);
    let b = &index.builds[1];
    assert_eq!(b.tested_with.as_deref(), Some("2026.11.08"));
    assert_eq!(b.size, 3_910_686_132);
    assert_eq!(&b.zip[b.zip.len() - 12..b.zip.len() - 4], &b.sha256[..8]);
    // Unchanged by a round trip through the editing helper
    assert_eq!(changed(|_| {}).unwrap(), index);
}

#[test]
fn it_is_for_one_device_and_one_format() {
    assert!(matches!(
        IndexData::parse(REAL, "panther"),
        Err(Error::BuildIndex(why)) if why.contains("is for cheetah, not panther")
    ));
    refused(|v| v["format"] = json!(2), "format 2");
    refused(|v| v["format"] = json!(true), "not a build index");
    refused(
        |v| v["codename"] = json!("Cheetah"),
        "codename is not valid",
    );
    refused(
        |v| v["updated"] = json!("2026-13-01"),
        "updated is not a date",
    );
    refused(
        |v| v["updated"] = json!("yesterday"),
        "updated is not a date",
    );
    refused(|v| v["builds"] = json!({}), "not a build index");
    refused(
        |v| {
            v.as_object_mut().unwrap().remove("builds");
        },
        "not a build index",
    );
    // A key this installer doesn't know could be a rule it would ignore
    refused(
        |v| v["withdrawn"] = json!(["CP2A.260705.006"]),
        "not a build index",
    );
    refused(
        |v| v["builds"][1]["withdrawn"] = json!(true),
        "not a build index",
    );
    assert!(matches!(
        IndexData::parse(b"not json", "cheetah"),
        Err(Error::BuildIndex(_))
    ));
}

#[test]
fn every_field_of_a_build_is_checked() {
    let b = |v: &mut Value, key: &str, to: Value| v["builds"][1][key] = to;
    refused(|v| b(v, "build", json!("cp2a.260705.006")), "build id");
    refused(|v| b(v, "build", json!("CP2A.2607.006")), "build id");
    refused(|v| b(v, "build", json!("CP2A.260705.006.")), "build id");
    refused(|v| b(v, "bootloader", json!("")), "bootloader is not valid");
    refused(
        |v| b(v, "bootloader", json!("-rf")),
        "bootloader is not valid",
    );
    refused(
        |v| b(v, "baseband", json!("g5300q 260317")),
        "baseband is not valid",
    );
    refused(|v| b(v, "sha256", json!("23D564AD")), "sha256 is not valid");
    refused(|v| b(v, "size", json!(0)), "size is not a size");
    refused(|v| b(v, "size", json!(-1)), "not a build index");
    refused(|v| b(v, "size", json!("3910686132")), "not a build index");
    refused(|v| b(v, "tested", json!("yes")), "not a build index");
    refused(
        |v| {
            v["builds"][1].as_object_mut().unwrap().remove("sha256");
        },
        "not a build index",
    );
    // tested and tested_with come together, and tested_with is a release
    refused(|v| b(v, "tested", json!(false)), "only a tested one");
    refused(
        |v| {
            v["builds"][1]
                .as_object_mut()
                .unwrap()
                .remove("tested_with");
        },
        "names the release it was tested with",
    );
    refused(
        |v| b(v, "tested_with", json!("soon")),
        "tested_with is not a release",
    );
    refused(|v| b(v, "tested_with", json!(null)), "names the release");
    // The same build twice
    refused(
        |v| {
            let again = v["builds"][1].clone();
            v["builds"].as_array_mut().unwrap().push(again);
        },
        "listed twice",
    );
}

#[test]
fn the_zip_is_named_for_the_device_the_build_and_its_own_digest() {
    let b = |v: &mut Value, key: &str, to: &str| v["builds"][1][key] = json!(to);
    // Another digest than the name carries (the SHA-256's first digit changed)
    refused(
        |v| {
            b(
                v,
                "sha256",
                "33d564ad5d0db2d04fa4bbde43930c9b0f8e570a51e8090c8d37c71dc96a2c0c",
            )
        },
        "first eight digits of its SHA-256",
    );
    refused(
        |v| b(v, "zip", "cheetah-cp2a.260705.006-factory-00000000.zip"),
        "first eight digits of its SHA-256",
    );
    // Another build's, another device's, another kind of file
    refused(
        |v| b(v, "zip", "cheetah-cp3a.261005.002.a1-factory-23d564ad.zip"),
        "not named for cheetah and this build",
    );
    refused(
        |v| b(v, "zip", "panther-cp2a.260705.006-factory-23d564ad.zip"),
        "not named for cheetah and this build",
    );
    refused(
        |v| b(v, "zip", "cheetah-cp2a.260705.006-factory-23d564ad.zip.exe"),
        "not named for cheetah and this build",
    );
    refused(
        |v| b(v, "zip", "../cheetah-cp2a.260705.006-factory-23d564ad.zip"),
        "not named for cheetah and this build",
    );
}

#[test]
fn the_download_is_on_dl_google_com_and_the_page_on_developers_google_com() {
    let name = "cheetah-cp2a.260705.006-factory-23d564ad.zip";
    let url = |v: &mut Value, to: String| v["builds"][1]["url"] = json!(to);
    for host in [
        "https://dl.google.com.example.org/dl/android/aosp/",
        "https://example.org/dl.google.com/",
        "http://dl.google.com/dl/android/aosp/",
        "https://dl.google.com@example.org/",
        "https://dl.google.com:8443/dl/",
        "https://DL.google.com/dl/",
        "//dl.google.com/dl/",
    ] {
        refused(
            |v| url(v, format!("{host}{name}")),
            "url is not on dl.google.com",
        );
    }
    for path in ["dl/../../x/", "dl/a?b=", "dl/a#", "dl/%2e%2e/", "dl\\"] {
        refused(
            |v| url(v, format!("https://dl.google.com/{path}{name}")),
            "url is not on dl.google.com",
        );
    }
    // On the right host, but another file
    refused(
        |v| url(v, "https://dl.google.com/dl/android/aosp/other.zip".into()),
        "url doesn't end in the zip's name",
    );
    refused(
        |v| url(v, format!("https://dl.google.com/dl/android/aosp/x{name}")),
        "url doesn't end in the zip's name",
    );
    let page = |v: &mut Value, to: &str| v["builds"][1]["page"] = json!(to);
    for bad in [
        "https://developers.google.com.example.org/android/images",
        "https://example.org/android/images#cheetah",
        "http://developers.google.com/android/images",
        "https://developers.google.com/android/images/../../x",
        "https://developers.google.com/android/images?x=1",
        "https://developers.google.com/android/images#",
        "https://developers.google.com/android/images#a b",
        "https://developers.google.com/android/ota",
        "javascript:alert(1)",
    ] {
        refused(|v| page(v, bad), "page is not Google's factory images page");
    }
    assert!(changed(|v| page(v, "https://developers.google.com/android/images")).is_ok());
}

#[test]
fn an_index_is_trusted_only_as_a_verified_releases_signed_file() {
    let bundle = TestBundle::new();
    let release = bundle.verify().unwrap();
    assert!(release.has_build_index());
    let index = release.build_index(&bundle.index_json).unwrap();
    assert_eq!(index.data().builds.len(), 5);

    // The real index is well formed, but this release didn't sign it
    assert!(matches!(release.build_index(REAL), Err(Error::Checksum(_))));
    // One changed byte, still valid JSON with valid fields: refused
    let text = String::from_utf8(bundle.index_json.clone()).unwrap();
    let forged = text.replace("\"updated\": \"2026-01-02\"", "\"updated\": \"2026-01-03\"");
    assert_ne!(forged, text);
    assert!(IndexData::parse(forged.as_bytes(), "cheetah").is_ok());
    assert!(matches!(
        release.build_index(forged.as_bytes()),
        Err(Error::Checksum(_))
    ));
    // A release that carries none has none, whatever bytes are offered
    let without = TestBundle::with_index(|i| *i = Value::Null);
    let release = without.verify().unwrap();
    assert!(!release.has_build_index());
    assert!(matches!(
        release.build_index(&bundle.index_json),
        Err(Error::Checksum(why)) if why.contains("not a file of this release")
    ));
    // Signed and listed, but it breaks a rule: the signature doesn't save it
    let wrong_host = TestBundle::with_index(|i| {
        let url = i["builds"][2]["url"]
            .as_str()
            .unwrap()
            .replace("dl.google.com", "dl.example.org");
        i["builds"][2]["url"] = json!(url);
    });
    assert!(matches!(
        wrong_host.verify().unwrap().build_index(&wrong_host.index_json),
        Err(Error::BuildIndex(why)) if why.contains("dl.google.com")
    ));
    // Signed, for another device than the release
    let other = TestBundle::with_index(|i| i["codename"] = json!("panther"));
    assert!(matches!(
        other.verify().unwrap().build_index(&other.index_json),
        Err(Error::BuildIndex(why)) if why.contains("is for panther")
    ));
}

#[test]
fn a_match_is_both_versions_and_tested_or_nothing() {
    let bundle = TestBundle::new();
    let index = bundle
        .verify()
        .unwrap()
        .build_index(&bundle.index_json)
        .unwrap();

    // Both versions, tested: used, and the carrier variant is named
    match index.lookup("fake-1.0", "fake-radio-1.0") {
        Lookup::Tested { build, same_pair } => {
            assert_eq!(build.build, PHONE_BUILD);
            assert_eq!(build.zip, bundle.factory_zip_name);
            assert_eq!(build.size, bundle.factory_zip.len() as u64);
            assert_eq!(same_pair, ["TEST.260101.001.A1"]);
        }
        other => panic!("{other:?}"),
    }
    // The same bootloader with another baseband is not a match, either way round
    assert_eq!(
        index.lookup("fake-1.0", "fake-radio-1.1"),
        Lookup::NotListed
    );
    assert_eq!(
        index.lookup("fake-1.1", "fake-radio-1.0"),
        Lookup::NotListed
    );
    assert_eq!(
        index.lookup("fake-7.7", "fake-radio-7.7"),
        Lookup::NotListed
    );
    assert_eq!(index.lookup("", ""), Lookup::NotListed);
    // Listed and not tested: not used. Newer than every tested build...
    match index.lookup("fake-1.2", "fake-radio-1.2") {
        Lookup::NotTested {
            build,
            newer_tested,
        } => {
            assert_eq!(build.build, "TEST.260301.001");
            assert!(newer_tested.is_none());
        }
        other => panic!("{other:?}"),
    }
    // ...or older than one, which is then named: the nearest newer tested build
    match index.lookup("fake-0.9", "fake-radio-0.9") {
        Lookup::NotTested {
            build,
            newer_tested,
        } => {
            assert_eq!(build.build, "TEST.251201.001");
            assert_eq!(newer_tested.unwrap().build, PHONE_BUILD);
        }
        other => panic!("{other:?}"),
    }
    // Of two builds with the same versions, the tested one is used even
    // when the untested one is listed first
    let swapped = TestBundle::with_index(|i| i["builds"].as_array_mut().unwrap().swap(2, 3));
    let index = swapped
        .verify()
        .unwrap()
        .build_index(&swapped.index_json)
        .unwrap();
    assert!(matches!(
        index.lookup("fake-1.0", "fake-radio-1.0"),
        Lookup::Tested { build, same_pair } if build.build == PHONE_BUILD && same_pair == ["TEST.260101.001.A1"]
    ));
}

#[test]
fn the_real_index_matches_only_its_tested_build() {
    // Through a release that signs the real index, as a bundle would carry it
    let real: Value = serde_json::from_slice(REAL).unwrap();
    let bundle = TestBundle::with_index(|i| *i = real);
    let index = bundle
        .verify()
        .unwrap()
        .build_index(&bundle.index_json)
        .unwrap();
    assert!(matches!(
        index.lookup("cloudripper-17.0-15199429", "g5300q-260317-260505-B-15346003"),
        Lookup::Tested { build, same_pair } if build.build == "CP2A.260705.006" && same_pair.is_empty()
    ));
    // The newer build is listed but not tested, with no tested build after it
    assert!(matches!(
        index.lookup("cloudripper-17.0-15902555", "g5300q-260604-260807-B-16036236"),
        Lookup::NotTested { build, newer_tested: None } if build.build == "CP3A.261005.002.A1"
    ));
    // One build's bootloader with the other's baseband: nothing
    assert_eq!(
        index.lookup(
            "cloudripper-17.0-15199429",
            "g5300q-260604-260807-B-16036236"
        ),
        Lookup::NotListed
    );
}
