// SPDX-License-Identifier: MIT
//! manifest.json and device.conf rules, on the published cheetah manifest
//! and on changed copies of it.
use omamobile_installer_core::device_conf::DeviceConf;
use omamobile_installer_core::manifest::{Manifest, Slot};
use omamobile_installer_core::Error;
use serde_json::{json, Value};

const PUBLISHED: &[u8] = include_bytes!("../../fixtures/published/manifest.json");
const PUBLISHED_CONF: &[u8] = include_bytes!("../../fixtures/published/device.conf");

fn changed(edit: impl FnOnce(&mut Value)) -> Result<Manifest, Error> {
    let mut m: Value = serde_json::from_slice(PUBLISHED).unwrap();
    edit(&mut m);
    Manifest::parse(&serde_json::to_vec(&m).unwrap())
}

fn refused(edit: impl FnOnce(&mut Value), needle: &str) {
    match changed(edit) {
        Err(Error::Manifest(why)) => {
            assert!(why.contains(needle), "refused for another reason: {why}")
        }
        other => panic!("not refused: {other:?}"),
    }
}

#[test]
fn the_published_manifest_is_accepted() {
    let m = Manifest::parse(PUBLISHED).unwrap();
    assert_eq!(m.codename, "cheetah");
    assert_eq!(m.version, "2026.11.08");
    let parts: Vec<String> = m
        .boot_writes
        .iter()
        .map(|w| w.partition(Slot::B).name().to_string())
        .collect();
    assert_eq!(
        parts,
        [
            "boot_b",
            "init_boot_b",
            "vendor_boot_b",
            "vendor_kernel_boot_b",
            "dtbo_b"
        ]
    );
    assert_eq!(m.boot_writes[0].partition_size, Some(67108864));
    assert_eq!(m.boot_writes[2].partition_size, None);
    assert_eq!(m.root_image.partition.name(), "userdata");
    assert_eq!(m.root_image.size, Some(3412028613));
    m.check_descriptor(&DeviceConf::parse(PUBLISHED_CONF).unwrap())
        .unwrap();
}

#[test]
fn a_partition_off_the_allowlist_is_refused() {
    for target in [
        "boot_a",
        "vbmeta_b",
        "boot",
        "persist",
        "devinfo",
        "modem_b",
        "bootloader_b",
        "userdata_b",
        "../boot_b",
    ] {
        refused(
            |m| m["install"]["writes"][0]["partition"] = json!(target),
            "not a slot B boot image",
        );
    }
    // Named on the allowlist as well: still refused
    refused(
        |m| {
            m["install"]["storage_guard_allowlist"]
                .as_array_mut()
                .unwrap()
                .push(json!("vbmeta_b"))
        },
        "only slot B's boot images and userdata",
    );
    refused(
        |m| {
            m["install"]["storage_guard_allowlist"]
                .as_array_mut()
                .unwrap()
                .push(json!("boot_a"))
        },
        "only slot B's boot images and userdata",
    );
    // An extra write after userdata
    refused(
        |m| {
            m["install"]["writes"]
                .as_array_mut()
                .unwrap()
                .push(json!({"partition": "vbmeta_b", "source": "images/vbmeta.img", "size": 1}))
        },
        "not a slot B boot image",
    );
}

#[test]
fn the_plan_cannot_be_widened() {
    refused(|m| m["format"] = json!(2), "format 2");
    refused(
        |m| m["install"]["product"] = json!("panther"),
        "install.product",
    );
    refused(
        |m| m["install"]["require_unlocked_bootloader"] = json!(false),
        "unlocked",
    );
    refused(
        |m| m["install"]["require_matching_bootloader_slots"] = json!(false),
        "slots",
    );
    refused(|m| m["install"]["tool"] = json!("odin"), "fastboot");
    refused(
        |m| m["install"]["after"] = json!(["set-active a", "reboot"]),
        "set-active b",
    );
    refused(
        |m| m["install"]["after"] = json!(["set-active b", "flashing lock", "reboot"]),
        "set-active b",
    );
    // Keys this installer doesn't know are not ignored
    refused(
        |m| m["install"]["erase"] = json!(["metadata"]),
        "unknown field",
    );
    refused(
        |m| m["install"]["writes"][0]["slot"] = json!("a"),
        "unknown field",
    );
    // A boot image from somewhere else, twice, or erasing
    refused(
        |m| m["install"]["writes"][0]["source"] = json!("images/dtbo.img"),
        "must be written from",
    );
    refused(
        |m| m["install"]["writes"][0]["source"] = json!("../boot.img"),
        "must be written from",
    );
    refused(
        |m| m["install"]["writes"][1] = m["install"]["writes"][0].clone(),
        "twice",
    );
    refused(
        |m| m["install"]["writes"][0]["erases_userdata"] = json!(true),
        "erase userdata",
    );
    refused(|m| m["install"]["writes"][0]["size"] = json!(1), "size");
    refused(
        |m| m["install"]["writes"][0]["partition_size"] = json!(134217728),
        "exactly its partition",
    );
    // userdata last, from the root image, marked
    refused(
        |m| m["install"]["writes"][5]["source"] = json!("images/boot.img"),
        "last write",
    );
    refused(
        |m| m["install"]["writes"][5]["erases_userdata"] = json!(false),
        "last write",
    );
    refused(
        |m| {
            m["install"]["writes"].as_array_mut().unwrap().swap(4, 5);
        },
        "not a slot B boot image",
    );
    refused(
        |m| {
            m["install"]["writes"].as_array_mut().unwrap().remove(4);
        },
        "dtbo_b",
    );
    refused(
        |m| m["files"][0]["path"] = json!("../../etc/passwd"),
        "path",
    );
    refused(
        |m| m["root_image"]["name"] = json!("x/../y.img.zst"),
        "root_image.name",
    );
    refused(|m| m["root_image"]["compression"] = json!("xz"), "zstd");
}

#[test]
fn the_descriptor_is_data_and_must_agree() {
    for bad in [
        "CODENAME=$(reboot)",
        "NAME=\"a`b\"",
        "X=a b",
        "lower=1",
        "NAME='x'",
        "NAME=\"x",
        "A=\\n",
    ] {
        assert!(
            matches!(DeviceConf::parse(bad.as_bytes()), Err(Error::DeviceConf(_))),
            "{bad}"
        );
    }
    let m = Manifest::parse(PUBLISHED).unwrap();
    let conf = String::from_utf8(PUBLISHED_CONF.to_vec()).unwrap();
    let with = |from: &str, to: &str| {
        assert!(conf.contains(from));
        m.check_descriptor(&DeviceConf::parse(conf.replace(from, to).as_bytes()).unwrap())
    };
    assert!(with("CODENAME=cheetah", "CODENAME=panther").is_err());
    assert!(with("dtbo_b userdata\"", "dtbo_b userdata vbmeta_b\"").is_err());
    assert!(with(
        "BOOT_IMAGES=\"boot init_boot",
        "BOOT_IMAGES=\"init_boot boot"
    )
    .is_err());
    assert!(with("init_boot:0x800000", "init_boot:0x1000000").is_err());
    assert!(with("SOC_IDS=", "SOC_IDZ=").is_err());
    assert!(with(
        "CODENAME=cheetah",
        "CODENAME=cheetah\nBOOT_METHOD=android-v0"
    )
    .is_err());
}

/// The published manifest as format 2 (the running slot), for the rules
/// below: its writes name images, and its guard is the running slot's.
fn as_format_2(m: &mut Value) {
    m["format"] = json!(2);
    let i = m["install"].as_object_mut().unwrap();
    i.remove("require_matching_bootloader_slots");
    i.remove("storage_guard_allowlist");
    i.insert("slot".into(), json!("active"));
    i.insert("accept_current_slots".into(), json!(["a"]));
    i.insert("after".into(), json!(["reboot"]));
    i.insert(
        "storage_guard".into(),
        json!({"slot": "running", "images": ["boot", "init_boot", "vendor_boot", "vendor_kernel_boot", "dtbo"],
               "also": ["userdata"], "marker": "/etc/omamobile-ufs-rw-slot"}),
    );
    for w in i["writes"].as_array_mut().unwrap() {
        let w = w.as_object_mut().unwrap();
        let p = w["partition"].as_str().unwrap().to_string();
        if let Some(image) = p.strip_suffix("_b") {
            w.remove("partition");
            w.insert("image".into(), json!(image));
        }
    }
}

#[test]
fn a_format_2_manifest_writes_the_five_boot_images_and_userdata_and_nothing_else() {
    // The published manifest as format 2 is accepted, and writes exactly
    // the five images on the running slot
    let m = changed(as_format_2).unwrap();
    let images: Vec<&str> = m.boot_writes.iter().map(|w| w.image.as_str()).collect();
    assert_eq!(
        images,
        [
            "boot",
            "init_boot",
            "vendor_boot",
            "vendor_kernel_boot",
            "dtbo"
        ]
    );
    let on_a: Vec<String> = m
        .boot_writes
        .iter()
        .map(|w| w.partition(Slot::A).name().to_string())
        .collect();
    assert_eq!(
        on_a,
        [
            "boot_a",
            "init_boot_a",
            "vendor_boot_a",
            "vendor_kernel_boot_a",
            "dtbo_a"
        ]
    );
    // Any other image name would hit stock's live slot: refused, whether as
    // a write, as a write's partition, or in the guard's list, with or
    // without a slot in the name
    for name in [
        "vbmeta",
        "vbmeta_system",
        "vbmeta_vendor",
        "abl",
        "bl1",
        "bl2",
        "bl31",
        "pbl",
        "tzsw",
        "gsa",
        "ldfw",
        "modem",
        "radio",
        "bootloader",
        "persist",
        "efs",
        "efs_backup",
        "modem_userdata",
        "devinfo",
        "misc",
        "super",
        "userdata",
        "metadata",
        "boot_a",
        "boot_b",
        "vbmeta_a",
        "abl_a",
        "modem_a",
        "BOOT",
        "boot ",
        "",
        "made_up",
    ] {
        refused(
            |m| {
                as_format_2(m);
                m["install"]["writes"][0]["image"] = json!(name);
            },
            "",
        );
        refused(
            |m| {
                as_format_2(m);
                let w = m["install"]["writes"].as_array_mut().unwrap();
                w.insert(0, json!({"image": name, "source": format!("images/{name}.img"), "size": 4096, "partition_size": null}));
            },
            "",
        );
        refused(
            |m| {
                as_format_2(m);
                m["install"]["storage_guard"]["images"]
                    .as_array_mut()
                    .unwrap()
                    .push(json!(name));
            },
            "",
        );
        for p in [name.to_string(), format!("{name}_a"), format!("{name}_b")] {
            refused(
                |m| {
                    as_format_2(m);
                    m["install"]["writes"][0]["partition"] = json!(p);
                },
                "",
            );
        }
    }
    // The guard may leave userdata writable and nothing else besides
    for also in [
        json!(["userdata", "persist"]),
        json!(["metadata"]),
        json!([]),
    ] {
        refused(
            |m| {
                as_format_2(m);
                m["install"]["storage_guard"]["also"] = also.clone();
            },
            "storage_guard.also",
        );
    }
    // The descriptor can't widen it either: a BOOT_IMAGES or UFS_RW that
    // names another image is not what the manifest writes
    let conf = String::from_utf8(PUBLISHED_CONF.to_vec()).unwrap()
        + "INSTALL_SLOT=active\nINSTALL_ACTIVE_SLOTS=\"a\"\n";
    m.check_descriptor(&DeviceConf::parse(conf.as_bytes()).unwrap())
        .unwrap();
    for (key, extra) in [("BOOT_IMAGES=\"", "vbmeta "), ("UFS_RW=\"", "vbmeta_b ")] {
        let widened = conf.replace(key, &format!("{key}{extra}"));
        assert!(
            m.check_descriptor(&DeviceConf::parse(widened.as_bytes()).unwrap())
                .is_err(),
            "{key}{extra}"
        );
    }
}
