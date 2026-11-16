// SPDX-License-Identifier: MIT
//! The owner's answers are judged by the rules the phone's first start
//! applies: `omamobile-firstboot`'s `validate` (omarchy-mobile,
//! packages/omamobile-firstboot/bin/omamobile-firstboot, lines 120-153).
//!
//! `the_same_verdict_as_first_boot_itself` runs that script on every case
//! here and compares, when OMAMOBILE_FIRSTBOOT_BIN names it; without it the
//! cases stand on what was read from the script.
mod common;
use omamobile_link::answers::*;

fn base() -> Answers {
    common::answers().get().clone()
}

/// Every case: a field, a value for it (`None`: the key left out), and the
/// reason it is refused for, if it is.
fn cases() -> Vec<(Field, Option<String>, Option<&'static str>)> {
    use Field::*;
    let s = |x: &str| Some(x.to_string());
    let hash = |salt: &str, n: usize| {
        format!(
            "$6${salt}${}",
            "./aZ09".chars().cycle().take(n).collect::<String>()
        )
    };
    let key = "AAAAC3NzaC1lZDI1NTE5AAAAIFake";
    let mut v = vec![
        // USERNAME_RE (line 67), required (124), RESERVED (66)
        (Username, s("owner"), None),
        (Username, s("_o"), None),
        (Username, s("a"), None),
        (Username, s("a-b_c9"), None),
        (Username, Some("a".repeat(32)), None),
        (Username, Some("a".repeat(33)), Some(reason::FORMAT)),
        (Username, s(""), Some(reason::REQUIRED)),
        (Username, s("Owner"), Some(reason::FORMAT)),
        (Username, s("9lives"), Some(reason::FORMAT)),
        (Username, s("-dash"), Some(reason::FORMAT)),
        (Username, s("o wner"), Some(reason::FORMAT)),
        (Username, s("o.wner"), Some(reason::FORMAT)),
        (Username, s("ownér"), Some(reason::FORMAT)),
        (Username, s("root"), Some(reason::RESERVED)),
        (Username, s("omamobile"), Some(reason::RESERVED)),
        (Username, s("admin"), Some(reason::RESERVED)),
        (Username, s("_talkd"), Some(reason::RESERVED)),
        (Username, s("cups-browsed"), Some(reason::RESERVED)),
        (Username, s("systemd-"), Some(reason::RESERVED)),
        (Username, s("systemd-network"), Some(reason::RESERVED)),
        (Username, s("systemd"), None),
        (Username, s("roots"), None),
        (Username, s("xroot"), None),
        (Username, s("gitlab"), None),
        (Username, s("a\tb"), Some(reason::CONTROL)),
        // HASH_RE (line 69)
        (PasswordHash, Some(hash("salt", 86)), None),
        (PasswordHash, Some(hash("s", 86)), None),
        (PasswordHash, Some(hash("0123456789abcdef", 86)), None),
        (PasswordHash, Some(hash("rounds=5000$salt", 86)), None),
        (PasswordHash, Some(hash("rounds=123456789$salt", 86)), None),
        (
            PasswordHash,
            Some(hash("rounds=123$salt", 86)),
            Some(reason::FORMAT),
        ),
        (
            PasswordHash,
            Some(hash("rounds=1234567890$salt", 86)),
            Some(reason::FORMAT),
        ),
        (
            PasswordHash,
            Some(hash("rounds=5000", 86)),
            Some(reason::FORMAT),
        ),
        (
            PasswordHash,
            Some(hash("Rounds=5000$salt", 86)),
            Some(reason::FORMAT),
        ),
        (
            PasswordHash,
            Some(hash("0123456789abcdefg", 86)),
            Some(reason::FORMAT),
        ),
        (PasswordHash, Some(hash("", 86)), Some(reason::FORMAT)),
        (PasswordHash, Some(hash("salt", 85)), Some(reason::FORMAT)),
        (PasswordHash, Some(hash("salt", 87)), Some(reason::FORMAT)),
        (PasswordHash, Some(hash("sa+lt", 86)), Some(reason::FORMAT)),
        (
            PasswordHash,
            Some(hash("salt", 86).replace("$6$", "$5$")),
            Some(reason::FORMAT),
        ),
        (
            PasswordHash,
            Some(hash("salt", 86).replace("$6$", "$y$")),
            Some(reason::FORMAT),
        ),
        (
            PasswordHash,
            Some(format!(" {}", hash("salt", 86))),
            Some(reason::FORMAT),
        ),
        (
            PasswordHash,
            Some(format!("{}$", hash("salt", 86))),
            Some(reason::FORMAT),
        ),
        (PasswordHash, s("1234"), Some(reason::FORMAT)),
        (PasswordHash, s("!"), Some(reason::FORMAT)),
        (PasswordHash, s(""), Some(reason::REQUIRED)),
        // CREDENTIAL (lines 149-150): empty or left out is pin
        (Credential, None, None),
        (Credential, s(""), None),
        (Credential, s("pin"), None),
        (Credential, s("password"), None),
        (Credential, s("PIN"), Some(reason::FORMAT)),
        (Credential, s("pin "), Some(reason::FORMAT)),
        (Credential, s("fingerprint"), Some(reason::FORMAT)),
        // HOSTNAME_RE (line 68): empty or left out is omamobile
        (Hostname, None, None),
        (Hostname, s(""), None),
        (Hostname, s("a"), None),
        (Hostname, s("My-Phone7"), None),
        (Hostname, s("7"), None),
        (Hostname, Some("h".repeat(63)), None),
        (Hostname, Some("h".repeat(64)), Some(reason::FORMAT)),
        (Hostname, s("-phone"), Some(reason::FORMAT)),
        (Hostname, s("phone-"), Some(reason::FORMAT)),
        (Hostname, s("-"), Some(reason::FORMAT)),
        (Hostname, s("my.phone"), Some(reason::FORMAT)),
        (Hostname, s("my_phone"), Some(reason::FORMAT)),
        (Hostname, s("my phone"), Some(reason::FORMAT)),
        (Hostname, s("téléphone"), Some(reason::FORMAT)),
        // TZ_RE (line 71) and no `..` (136): empty or left out is UTC
        (Timezone, None, None),
        (Timezone, s(""), None),
        (Timezone, s("UTC"), None),
        (Timezone, s("Europe/Berlin"), None),
        (Timezone, s("America/Argentina/Buenos_Aires"), None),
        (Timezone, s("Etc/GMT+5"), None),
        (Timezone, s("Etc/GMT-14"), None),
        (Timezone, s("/etc/passwd"), Some(reason::FORMAT)),
        (Timezone, s("Europe/"), Some(reason::FORMAT)),
        (Timezone, s("Europe//Berlin"), Some(reason::FORMAT)),
        (Timezone, s("../../etc/shadow"), Some(reason::FORMAT)),
        (Timezone, s("Europe/../UTC"), Some(reason::FORMAT)),
        (Timezone, s("Europe/Ber lin"), Some(reason::FORMAT)),
        (Timezone, s("posix.rules"), Some(reason::FORMAT)),
        (Timezone, Some(format!("A/{}", "b".repeat(62))), None),
        (
            Timezone,
            Some(format!("A/{}", "b".repeat(63))),
            Some(reason::TOO_LONG),
        ),
        // WIFI_SSID (lines 143-144): given, it is 1 to 32 bytes
        (WifiSsid, s("x"), None),
        (WifiSsid, Some("s".repeat(32)), None),
        (WifiSsid, s("Café ☕ net"), None),
        (WifiSsid, s(" leading and trailing "), None),
        (WifiSsid, s("#hash=equals"), None),
        (WifiSsid, s(""), Some(reason::LENGTH)),
        (WifiSsid, Some("s".repeat(33)), Some(reason::LENGTH)),
        (WifiSsid, Some("é".repeat(17)), Some(reason::LENGTH)),
        (WifiSsid, Some("é".repeat(16)), None),
        (WifiSsid, s("a\u{1}b"), Some(reason::CONTROL)),
        (WifiSsid, s("a\u{7f}b"), Some(reason::CONTROL)),
        // WIFI_PSK (lines 145-146)
        (WifiPsk, None, None),
        (WifiPsk, s("12345678"), None),
        (WifiPsk, s(" spaces ~ and # signs "), None),
        (WifiPsk, Some("p".repeat(63)), None),
        (
            WifiPsk,
            Some("0123456789abcdefABCDEF".chars().cycle().take(64).collect()),
            None,
        ),
        (WifiPsk, Some("p".repeat(64)), Some(reason::FORMAT)),
        (WifiPsk, Some("0".repeat(65)), Some(reason::FORMAT)),
        (WifiPsk, s("1234567"), Some(reason::FORMAT)),
        (WifiPsk, s(""), Some(reason::FORMAT)),
        (WifiPsk, s("pässwörter"), Some(reason::FORMAT)),
        // SSHKEY_RE (line 70)
        (SshKeys, None, None),
        (SshKeys, Some(format!("ssh-ed25519 {key}")), None),
        (
            SshKeys,
            Some(format!("ssh-ed25519 {key} owner@computer")),
            None,
        ),
        (SshKeys, Some(format!("ssh-ed25519 {key} ")), None),
        (
            SshKeys,
            Some(format!("ssh-ed25519 {key} a comment with spaces ~")),
            None,
        ),
        (SshKeys, Some(format!("ssh-rsa {key}==")), None),
        (SshKeys, Some(format!("ssh-rsa {key}===")), None),
        (SshKeys, Some(format!("ecdsa-sha2-nistp256 {key}")), None),
        (SshKeys, Some(format!("ecdsa-sha2-nistp384 {key}")), None),
        (SshKeys, Some(format!("ecdsa-sha2-nistp521 {key}")), None),
        (
            SshKeys,
            Some(format!("sk-ssh-ed25519@openssh.com {key}")),
            None,
        ),
        (
            SshKeys,
            Some(format!("sk-ecdsa-sha2-nistp256@openssh.com {key}")),
            None,
        ),
        (
            SshKeys,
            Some(format!("ssh-rsa {key}====")),
            Some(reason::FORMAT),
        ),
        (
            SshKeys,
            Some(format!("ssh-rsa {key}=x")),
            Some(reason::FORMAT),
        ),
        (SshKeys, s("ssh-rsa ==="), Some(reason::FORMAT)),
        (
            SshKeys,
            Some(format!("ssh-dss {key}")),
            Some(reason::FORMAT),
        ),
        (
            SshKeys,
            Some(format!("ecdsa-sha2-nistp224 {key}")),
            Some(reason::FORMAT),
        ),
        (
            SshKeys,
            Some(format!("sk-ssh-ed25519@opensshxcom {key}")),
            Some(reason::FORMAT),
        ),
        (
            SshKeys,
            Some(format!("ssh-ed25519  {key}")),
            Some(reason::FORMAT),
        ),
        (
            SshKeys,
            Some(format!(" ssh-ed25519 {key}")),
            Some(reason::FORMAT),
        ),
        (
            SshKeys,
            Some(format!("ssh-ed25519 {key} commént")),
            Some(reason::FORMAT),
        ),
        (
            SshKeys,
            Some(format!("ssh-ed25519 {key}-_")),
            Some(reason::FORMAT),
        ),
        (SshKeys, s("ssh-ed25519"), Some(reason::FORMAT)),
        (SshKeys, s("ssh-ed25519 "), Some(reason::FORMAT)),
        (
            SshKeys,
            Some(format!("command=\"id\" ssh-ed25519 {key}")),
            Some(reason::FORMAT),
        ),
        (SshKeys, s(""), Some(reason::FORMAT)),
        (
            SshKeys,
            Some(format!("ssh-ed25519 {key}\tx")),
            Some(reason::CONTROL),
        ),
        (
            SshKeys,
            Some(format!("ssh-rsa {}", "A".repeat(8192 - 8))),
            None,
        ),
        (
            SshKeys,
            Some(format!("ssh-rsa {}", "A".repeat(8192 - 7))),
            Some(reason::TOO_LONG),
        ),
        // FULL_NAME (line 151)
        (FullName, None, None),
        (FullName, s(""), None),
        (FullName, s("Zoë Ångström-O'Neil 3rd"), None),
        (FullName, s("a=b #c"), None),
        (FullName, s("Last, First"), Some(reason::FORMAT)),
        (FullName, s("a:b"), Some(reason::FORMAT)),
        (FullName, Some("n".repeat(128)), None),
        (FullName, Some("n".repeat(129)), Some(reason::TOO_LONG)),
        // EMAIL (line 152)
        (Email, None, None),
        (Email, s(""), None),
        (Email, s("a@b"), None),
        (Email, s("zoë+tag@exämple.org"), None),
        (Email, s("a"), Some(reason::FORMAT)),
        (Email, s("@b"), Some(reason::FORMAT)),
        (Email, s("a@"), Some(reason::FORMAT)),
        (Email, s("a@b@c"), Some(reason::FORMAT)),
        (Email, s("a b@c"), Some(reason::FORMAT)),
        (Email, s("a@b c"), Some(reason::FORMAT)),
        (Email, s(" a@b"), Some(reason::FORMAT)),
        (Email, Some(format!("{}@b", "a".repeat(252))), None),
        (
            Email,
            Some(format!("{}@b", "a".repeat(253))),
            Some(reason::TOO_LONG),
        ),
    ];
    // Line 103: a control character in any value, whatever the field
    for f in Field::ALL {
        for c in ['\u{1}', '\u{8}', '\u{1b}', '\u{1f}', '\u{7f}'] {
            v.push((f, Some(format!("ab{c}cd")), Some(reason::CONTROL)));
        }
    }
    v
}

fn with(field: Field, value: Option<String>) -> Answers {
    let mut a = base();
    match field {
        Field::Username => a.username = value.unwrap_or_default(),
        Field::PasswordHash => a.password_hash = value.unwrap_or_default(),
        Field::Credential => a.credential = value,
        Field::Hostname => a.hostname = value,
        Field::Timezone => a.timezone = value,
        Field::WifiSsid => a.wifi_ssid = value,
        Field::WifiPsk => a.wifi_psk = value,
        Field::SshKeys => a.ssh_keys = value.into_iter().collect(),
        Field::FullName => a.full_name = value,
        Field::Email => a.email = value,
    }
    a
}

#[test]
fn every_rule_of_first_boot() {
    let all = cases();
    assert!(all.len() > 200);
    for (field, value, want) in all {
        let got = with(field, value.clone()).validate();
        match (want, got) {
            (None, Ok(_)) => {}
            (Some(why), Err(p)) => {
                assert_eq!(p, vec![Problem::new(field, why)], "{field:?} = {value:?}")
            }
            (want, got) => panic!(
                "{field:?} = {value:?}: wanted {want:?}, got {:?}",
                got.err()
            ),
        }
    }
}

#[test]
fn rules_that_take_two_fields() {
    // Line 142: a passphrase without a network
    let mut a = base();
    a.wifi_ssid = None;
    assert_eq!(
        a.validate().unwrap_err(),
        vec![Problem::new(Field::WifiPsk, reason::WITHOUT_SSID)]
    );
    // An open network: a name and no passphrase
    a.wifi_ssid = Some("open".into());
    a.wifi_psk = None;
    assert!(a.validate().is_ok());
    // No Wi-Fi at all
    a.wifi_ssid = None;
    assert!(a.validate().is_ok());
    // §5.5: at most eight keys
    let mut a = base();
    a.ssh_keys = vec![a.ssh_keys[0].clone(); 8];
    assert!(a.validate().is_ok());
    a.ssh_keys.push(a.ssh_keys[0].clone());
    assert_eq!(
        a.validate().unwrap_err(),
        vec![Problem::new(Field::SshKeys, reason::TOO_MANY)]
    );
    // Every objection at once, one per field, in the message's order
    let a = Answers {
        username: "root".into(),
        password_hash: "x".into(),
        credential: Some("face".into()),
        hostname: Some("-".into()),
        timezone: Some("/".into()),
        wifi_ssid: Some(String::new()),
        wifi_psk: Some("short".into()),
        ssh_keys: vec!["nope".into()],
        full_name: Some("a,b".into()),
        email: Some("nope".into()),
    };
    let fields: Vec<Field> = a
        .validate()
        .unwrap_err()
        .into_iter()
        .map(|p| p.field)
        .collect();
    assert_eq!(fields, Field::ALL);
    // The least first boot takes: a name and a hash
    let least = Answers {
        username: "owner".into(),
        password_hash: base().password_hash,
        ..Answers::default()
    };
    assert_eq!(
        least.validate().unwrap().firstboot_conf(),
        format!("USERNAME=owner\nPASSWORD_HASH={}\n", least.password_hash)
    );
}

#[test]
fn the_candidate_file_is_first_boots_format() {
    let conf = common::answers().firstboot_conf();
    let want = format!(
        "USERNAME=owner\nPASSWORD_HASH={}\nCREDENTIAL=pin\nHOSTNAME=phone\nTIMEZONE=Europe/Berlin\n\
         WIFI_SSID=<SSID>\nWIFI_PSK=a passphrase\nSSH_KEY=ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAA owner@computer\n\
         FULL_NAME=An Owner\nEMAIL=owner@example.org\n",
        base().password_hash
    );
    assert_eq!(conf, want);
    // Empty is left out, so first boot's defaults apply; each key once; a
    // key per SSH key
    let mut a = base();
    a.hostname = Some(String::new());
    a.timezone = None;
    a.credential = Some(String::new());
    a.email = Some(String::new());
    a.ssh_keys.push("ssh-rsa AAAA second".into());
    let conf = a.validate().unwrap().firstboot_conf();
    assert!(
        !conf.contains("HOSTNAME")
            && !conf.contains("TIMEZONE")
            && !conf.contains("CREDENTIAL")
            && !conf.contains("EMAIL")
    );
    assert_eq!(conf.matches("SSH_KEY=").count(), 2);
    // No value can start a line of its own: a line break is a control
    // character, and those never get this far
    for f in Field::ALL {
        assert!(
            with(f, Some("x\nUSERNAME=root".into())).validate().is_err(),
            "{f:?}"
        );
        assert!(
            with(f, Some("x\rUSERNAME=root".into())).validate().is_err(),
            "{f:?}"
        );
        assert!(with(f, Some("x\0y".into())).validate().is_err(), "{f:?}");
    }
}

#[test]
fn the_phones_own_objections_are_read_from_porcelain() {
    let text = "USERNAME\tsystem-account\nTIMEZONE\tno-such-zone\n\nSSH_KEY\tformat\n";
    assert_eq!(
        parse_porcelain(text).unwrap(),
        vec![
            Problem::new(Field::Username, reason::SYSTEM_ACCOUNT),
            Problem::new(Field::Timezone, reason::NO_SUCH_ZONE),
            Problem::new(Field::SshKeys, reason::FORMAT),
        ]
    );
    assert_eq!(parse_porcelain(""), Some(vec![]));
    // Anything else is not porcelain: no guess is made, and no value echoed
    for bad in [
        "USERNAME reserved",
        "SHELL\tformat",
        "USERNAME\tNot A Reason",
        "USERNAME\t",
        "omamobile-firstboot: USERNAME 'x' is reserved",
    ] {
        assert_eq!(parse_porcelain(bad), None, "{bad}");
    }
    for f in Field::ALL {
        assert_eq!(Field::from_conf_key(f.conf_key()), Some(f));
        assert_eq!(Field::from_wire(f.as_str()), Some(f));
    }
}

/// The file `tools/install-phone` would write for these answers, values as
/// they are: what first boot's own `parse` and `validate` are asked about.
fn raw_conf(a: &Answers) -> String {
    let mut s = format!(
        "USERNAME={}\nPASSWORD_HASH={}\n",
        a.username, a.password_hash
    );
    for (k, v) in [
        ("CREDENTIAL", &a.credential),
        ("HOSTNAME", &a.hostname),
        ("TIMEZONE", &a.timezone),
        ("WIFI_SSID", &a.wifi_ssid),
        ("WIFI_PSK", &a.wifi_psk),
        ("FULL_NAME", &a.full_name),
        ("EMAIL", &a.email),
    ] {
        if let Some(v) = v {
            s.push_str(&format!("{k}={v}\n"));
        }
    }
    for k in &a.ssh_keys {
        s.push_str(&format!("SSH_KEY={k}\n"));
    }
    s
}

#[test]
fn the_same_verdict_as_first_boot_itself() {
    let Ok(script) = std::env::var("OMAMOBILE_FIRSTBOOT_BIN") else {
        eprintln!("skipped: OMAMOBILE_FIRSTBOOT_BIN does not name omamobile-firstboot");
        return;
    };
    // A scratch root as the script's own tests use (OMAMOBILE_FIRSTBOOT_ROOT)
    let root =
        std::env::temp_dir().join(format!("omamobile-link-firstboot-{}", std::process::id()));
    let zones = root.join("usr/share/zoneinfo");
    std::fs::create_dir_all(root.join("etc")).unwrap();
    std::fs::write(root.join("etc/passwd"), "root:x:0:0::/root:/bin/bash\n").unwrap();
    let mut compared = 0;
    for (field, value, want) in cases() {
        let a = with(field, value.clone());
        // Every zone name of the right form exists here: whether one exists
        // on the phone is the phone's to check, not this crate's
        if let Some(z) = a
            .timezone
            .as_deref()
            .filter(|z| !z.is_empty() && is_timezone_name(z))
        {
            std::fs::create_dir_all(zones.join(z).parent().unwrap()).unwrap();
            std::fs::write(zones.join(z), "").unwrap();
        }
        std::fs::create_dir_all(&zones).unwrap();
        std::fs::write(zones.join("UTC"), "").unwrap();
        let conf = root.join("candidate.conf");
        std::fs::write(&conf, raw_conf(&a)).unwrap();
        let out = std::process::Command::new("bash")
            .arg(&script)
            .arg("--check")
            .arg(&conf)
            .env("OMAMOBILE_FIRSTBOOT_ROOT", &root)
            .output()
            .unwrap();
        // The lengths §5.5 adds are the link's, not first boot's
        let links_own = matches!(want, Some(reason::TOO_LONG) | Some(reason::TOO_MANY));
        let ours = want.is_none() || links_own;
        assert_eq!(
            out.status.success(),
            ours,
            "{field:?} = {value:?}: first boot says {}",
            String::from_utf8_lossy(&out.stderr)
        );
        compared += 1;
    }
    std::fs::remove_dir_all(&root).unwrap();
    eprintln!("compared {compared} cases with {script}");
}
