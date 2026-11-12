// SPDX-License-Identifier: MIT
//! The owner's answers, checked by the rules the phone's first start applies.
//!
//! The rules are `omamobile-firstboot`'s (omarchy-mobile,
//! `packages/omamobile-firstboot/bin/omamobile-firstboot`), reproduced here
//! with the line each comes from, so the installer can say what is wrong
//! under a field before anything is sent, and the phone can refuse the same
//! things before it writes a candidate file. Three of first boot's rules need
//! the phone's own files (a zone that exists, a name that is a system
//! account, an account that exists already): those stay the phone's, and its
//! shell reports them with the same [`Problem`] type.
//!
//! Where INSTALL-LINK-PLAN §5.5 and first boot disagree, first boot's rule is
//! the one here; docs/LINK.md lists each such point.
use core::fmt;

/// The fields of `answers`, by their names on the wire.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Field {
    Username,
    PasswordHash,
    Credential,
    Hostname,
    Timezone,
    WifiSsid,
    WifiPsk,
    SshKeys,
    FullName,
    Email,
}

impl Field {
    pub const ALL: [Field; 10] = [
        Field::Username,
        Field::PasswordHash,
        Field::Credential,
        Field::Hostname,
        Field::Timezone,
        Field::WifiSsid,
        Field::WifiPsk,
        Field::SshKeys,
        Field::FullName,
        Field::Email,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Field::Username => "username",
            Field::PasswordHash => "password_hash",
            Field::Credential => "credential",
            Field::Hostname => "hostname",
            Field::Timezone => "timezone",
            Field::WifiSsid => "wifi_ssid",
            Field::WifiPsk => "wifi_psk",
            Field::SshKeys => "ssh_keys",
            Field::FullName => "full_name",
            Field::Email => "email",
        }
    }

    pub fn from_wire(s: &str) -> Option<Field> {
        Field::ALL.into_iter().find(|f| f.as_str() == s)
    }

    /// The key in `firstboot.conf`.
    pub fn conf_key(self) -> &'static str {
        match self {
            Field::Username => "USERNAME",
            Field::PasswordHash => "PASSWORD_HASH",
            Field::Credential => "CREDENTIAL",
            Field::Hostname => "HOSTNAME",
            Field::Timezone => "TIMEZONE",
            Field::WifiSsid => "WIFI_SSID",
            Field::WifiPsk => "WIFI_PSK",
            Field::SshKeys => "SSH_KEY",
            Field::FullName => "FULL_NAME",
            Field::Email => "EMAIL",
        }
    }

    pub fn from_conf_key(s: &str) -> Option<Field> {
        Field::ALL.into_iter().find(|f| f.conf_key() == s)
    }
}

/// The reasons this crate gives. The phone's shell adds its own for what
/// only the phone can check; any reason is 1 to 32 of `a-z`, `0-9` and `-`.
pub mod reason {
    /// The field is needed and empty.
    pub const REQUIRED: &str = "required";
    /// The value does not have the field's form.
    pub const FORMAT: &str = "format";
    /// A user name the system keeps for itself.
    pub const RESERVED: &str = "reserved";
    /// A control character (bytes 0x00 to 0x1f, and 0x7f).
    pub const CONTROL: &str = "control-character";
    /// Longer than the link carries.
    pub const TOO_LONG: &str = "too-long";
    /// More SSH keys than the link carries.
    pub const TOO_MANY: &str = "too-many";
    /// A Wi-Fi name that is not 1 to 32 bytes.
    pub const LENGTH: &str = "length";
    /// A Wi-Fi passphrase with no Wi-Fi name.
    pub const WITHOUT_SSID: &str = "without-ssid";
    /// Phone only: the name is a system account in the image.
    pub const SYSTEM_ACCOUNT: &str = "system-account";
    /// Phone only: an account of that name exists and first boot did not make it.
    pub const EXISTS: &str = "exists";
    /// Phone only: no such zone under `/usr/share/zoneinfo`.
    pub const NO_SUCH_ZONE: &str = "no-such-zone";
}

/// One objection: which field, and why in a fixed word. It never holds the
/// value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Problem {
    pub field: Field,
    pub reason: String,
}

impl Problem {
    pub fn new(field: Field, reason: &str) -> Problem {
        Problem {
            field,
            reason: reason.to_string(),
        }
    }
}

/// Is `s` a reason word: 1 to 32 of `a-z`, `0-9` and `-`?
pub fn is_reason(s: &str) -> bool {
    (1..=32).contains(&s.len())
        && s.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

/// The most SSH keys `answers` carries (§5.5).
pub const MAX_SSH_KEYS: usize = 8;
/// The longest SSH key line (§5.5).
pub const MAX_SSH_KEY: usize = 8192;
/// §5.5's lengths for the fields first boot itself gives no length.
pub const MAX_TIMEZONE: usize = 64;
pub const MAX_FULL_NAME: usize = 128;
pub const MAX_EMAIL: usize = 254;

/// The owner's answers as `answers` carries them. `None` is a key left out;
/// first boot takes an empty host name, time zone or credential as left out
/// too (its defaults: `omamobile`, `UTC`, `pin`).
#[derive(Clone, PartialEq, Eq, Default)]
pub struct Answers {
    pub username: String,
    pub password_hash: String,
    pub credential: Option<String>,
    pub hostname: Option<String>,
    pub timezone: Option<String>,
    pub wifi_ssid: Option<String>,
    pub wifi_psk: Option<String>,
    pub ssh_keys: Vec<String>,
    pub full_name: Option<String>,
    pub email: Option<String>,
}

/// Never the values: the password hash and the Wi-Fi passphrase are not for
/// a log (INSTALL-LINK-PLAN §9).
impl fmt::Debug for Answers {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "Answers {{ {} ssh keys, the rest withheld }}",
            self.ssh_keys.len()
        )
    }
}

/// Answers that passed [`Answers::validate`]. Only these become a message on
/// the computer or a `firstboot.conf` candidate on the phone.
#[derive(Clone, PartialEq, Eq)]
pub struct ValidAnswers(Answers);

impl fmt::Debug for ValidAnswers {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Valid{:?}", self.0)
    }
}

impl ValidAnswers {
    pub fn get(&self) -> &Answers {
        &self.0
    }

    /// The `firstboot.conf` candidate: `KEY=value` lines, the value literal to
    /// the end of the line, as first boot's `parse` reads them (lines 95-112).
    /// A key left out or empty writes no line, except the Wi-Fi pair, whose
    /// presence is the meaning. No value holds a line break: [`Answers::validate`]
    /// refused control characters.
    pub fn firstboot_conf(&self) -> String {
        let a = &self.0;
        let mut out = String::new();
        let mut line = |f: Field, v: &str| {
            out.push_str(f.conf_key());
            out.push('=');
            out.push_str(v);
            out.push('\n');
        };
        line(Field::Username, &a.username);
        line(Field::PasswordHash, &a.password_hash);
        for (f, v) in [
            (Field::Credential, &a.credential),
            (Field::Hostname, &a.hostname),
            (Field::Timezone, &a.timezone),
        ] {
            if let Some(v) = v.as_deref().filter(|v| !v.is_empty()) {
                line(f, v);
            }
        }
        if let Some(v) = &a.wifi_ssid {
            line(Field::WifiSsid, v);
        }
        if let Some(v) = &a.wifi_psk {
            line(Field::WifiPsk, v);
        }
        for k in &a.ssh_keys {
            line(Field::SshKeys, k);
        }
        for (f, v) in [(Field::FullName, &a.full_name), (Field::Email, &a.email)] {
            if let Some(v) = v.as_deref().filter(|v| !v.is_empty()) {
                line(f, v);
            }
        }
        out
    }
}

/// First boot's `RESERVED` (line 66), without `systemd-.*`, which is a prefix.
const RESERVED: &[&str] = &[
    "root",
    "alarm",
    "omamobile",
    "omandroid",
    "omarchy",
    "bin",
    "daemon",
    "mail",
    "ftp",
    "http",
    "nobody",
    "dbus",
    "tss",
    "uuidd",
    "alpm",
    "git",
    "avahi",
    "cups",
    "cups-browsed",
    "lp",
    "_talkd",
    "polkitd",
    "rtkit",
    "qemu",
    "brltty",
    "gluster",
    "rpc",
    "libvirt-qemu",
    "pcscd",
    "nvidia-persistenced",
    "sddm",
    "builder",
    "admin",
];

/// Line 103: `[[ $value != *[$'\x01'-$'\x1f\x7f']* ]]`. A NUL byte cannot be
/// in a line bash reads at all, so it is refused with them.
fn has_control(s: &str) -> bool {
    s.bytes().any(|b| b <= 0x1f || b == 0x7f)
}

/// Line 67: `USERNAME_RE='^[a-z_][a-z0-9_-]{0,31}$'`.
pub fn is_username(s: &str) -> bool {
    let b = s.as_bytes();
    (1..=32).contains(&b.len())
        && (b[0].is_ascii_lowercase() || b[0] == b'_')
        && b[1..]
            .iter()
            .all(|&c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'_' || c == b'-')
}

/// Line 66: `RESERVED`, anchored at both ends.
pub fn is_reserved_username(s: &str) -> bool {
    RESERVED.contains(&s) || s.starts_with("systemd-")
}

/// Line 69: `HASH_RE='^\$6\$(rounds=[0-9]{4,9}\$)?[./A-Za-z0-9]{1,16}\$[./A-Za-z0-9]{86}$'`.
pub fn is_sha512_crypt(s: &str) -> bool {
    let b64 = |p: &str| {
        p.bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'.' || c == b'/')
    };
    let Some(rest) = s.strip_prefix("$6$") else {
        return false;
    };
    let parts: Vec<&str> = rest.split('$').collect();
    let (salt, hash) = match parts.as_slice() {
        [salt, hash] => (salt, hash),
        [rounds, salt, hash] => {
            let Some(n) = rounds.strip_prefix("rounds=") else {
                return false;
            };
            if !(4..=9).contains(&n.len()) || !n.bytes().all(|c| c.is_ascii_digit()) {
                return false;
            }
            (salt, hash)
        }
        _ => return false,
    };
    (1..=16).contains(&salt.len()) && b64(salt) && hash.len() == 86 && b64(hash)
}

/// Line 68: `HOSTNAME_RE='^[A-Za-z0-9]([A-Za-z0-9-]{0,61}[A-Za-z0-9])?$'`.
pub fn is_hostname(s: &str) -> bool {
    let b = s.as_bytes();
    (1..=63).contains(&b.len())
        && b[0].is_ascii_alphanumeric()
        && b[b.len() - 1].is_ascii_alphanumeric()
        && b.iter().all(|&c| c.is_ascii_alphanumeric() || c == b'-')
}

/// Line 71: `TZ_RE='^[A-Za-z0-9_+-]+(/[A-Za-z0-9_+-]+)*$'`. Line 136 also
/// refuses `..`, which this form cannot hold. That the zone's file exists
/// (line 137) is the phone's to check.
pub fn is_timezone_name(s: &str) -> bool {
    s.split('/').all(|seg| {
        !seg.is_empty()
            && seg
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || c == b'_' || c == b'+' || c == b'-')
    })
}

/// Line 70: `SSHKEY_RE='^(ssh-ed25519|ssh-rsa|ecdsa-sha2-nistp(256|384|521)|
/// sk-ssh-ed25519@openssh\.com|sk-ecdsa-sha2-nistp256@openssh\.com)
/// [A-Za-z0-9+/]+={0,3}( [ -~]*)?$'`.
pub fn is_ssh_key(s: &str) -> bool {
    const TYPES: &[&str] = &[
        "ssh-ed25519",
        "ssh-rsa",
        "ecdsa-sha2-nistp256",
        "ecdsa-sha2-nistp384",
        "ecdsa-sha2-nistp521",
        "sk-ssh-ed25519@openssh.com",
        "sk-ecdsa-sha2-nistp256@openssh.com",
    ];
    let Some((kind, rest)) = s.split_once(' ') else {
        return false;
    };
    if !TYPES.contains(&kind) {
        return false;
    }
    let (body, comment) = match rest.split_once(' ') {
        Some((b, c)) => (b, Some(c)),
        None => (rest, None),
    };
    let data = body.trim_end_matches('=');
    let pad = body.len() - data.len();
    !data.is_empty()
        && pad <= 3
        && data
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'+' || c == b'/')
        && comment.is_none_or(|c| c.bytes().all(|c| (b' '..=b'~').contains(&c)))
}

/// Line 146: `^[\ -~]{8,63}$` or `^[0-9A-Fa-f]{64}$`.
pub fn is_wifi_psk(s: &str) -> bool {
    let b = s.as_bytes();
    ((8..=63).contains(&b.len()) && b.iter().all(|c| (b' '..=b'~').contains(c)))
        || (b.len() == 64 && b.iter().all(u8::is_ascii_hexdigit))
}

/// Line 152: `^[^[:space:]@]+@[^[:space:]@]+$`, under `LC_ALL=C` (line 122).
pub fn is_email(s: &str) -> bool {
    let space = |c: u8| matches!(c, b' ' | b'\t' | b'\n' | 0x0b | 0x0c | b'\r');
    match s.split_once('@') {
        Some((a, b)) => !a.is_empty() && !b.is_empty() && !b.contains('@') && !s.bytes().any(space),
        None => false,
    }
}

impl Answers {
    /// Every objection first boot's `validate` (lines 120-153) would raise
    /// from the values alone, and the lengths §5.5 adds. First boot stops at
    /// its first objection; this lists one per field. None: the answers, as
    /// the only thing a message or a candidate file is made from.
    pub fn validate(&self) -> Result<ValidAnswers, Vec<Problem>> {
        let mut problems = Vec::new();
        let mut check =
            |field: Field, value: Option<&str>, rule: &dyn Fn(&str) -> Option<&'static str>| {
                let Some(v) = value else { return };
                let why = if has_control(v) {
                    Some(reason::CONTROL)
                } else {
                    rule(v)
                };
                if let Some(why) = why {
                    problems.push(Problem::new(field, why));
                }
            };
        // Lines 124-126. Lines 127-131 (system and existing accounts) are the phone's.
        check(Field::Username, Some(&self.username), &|u| {
            if u.is_empty() {
                Some(reason::REQUIRED)
            } else if !is_username(u) {
                Some(reason::FORMAT)
            } else if is_reserved_username(u) {
                Some(reason::RESERVED)
            } else {
                None
            }
        });
        // Line 132
        check(Field::PasswordHash, Some(&self.password_hash), &|h| {
            if h.is_empty() {
                Some(reason::REQUIRED)
            } else if !is_sha512_crypt(h) {
                Some(reason::FORMAT)
            } else {
                None
            }
        });
        // Lines 149-150: empty is `pin`
        check(Field::Credential, self.credential.as_deref(), &|c| {
            (!matches!(c, "" | "pin" | "password")).then_some(reason::FORMAT)
        });
        // Lines 133-134: empty is `omamobile`
        check(Field::Hostname, self.hostname.as_deref(), &|h| {
            (!h.is_empty() && !is_hostname(h)).then_some(reason::FORMAT)
        });
        // Lines 135-136: empty is `UTC`
        check(Field::Timezone, self.timezone.as_deref(), &|z| {
            if z.len() > MAX_TIMEZONE {
                Some(reason::TOO_LONG)
            } else if !z.is_empty() && !is_timezone_name(z) {
                Some(reason::FORMAT)
            } else {
                None
            }
        });
        // Lines 143-144: a Wi-Fi name that is given is 1 to 32 bytes
        check(Field::WifiSsid, self.wifi_ssid.as_deref(), &|s| {
            (!(1..=32).contains(&s.len())).then_some(reason::LENGTH)
        });
        // Lines 142 and 145-146
        let no_ssid = self.wifi_ssid.is_none();
        check(Field::WifiPsk, self.wifi_psk.as_deref(), &|p| {
            if no_ssid {
                Some(reason::WITHOUT_SSID)
            } else if !is_wifi_psk(p) {
                Some(reason::FORMAT)
            } else {
                None
            }
        });
        // Lines 138-140, and §5.5's count and length
        let keys = if self.ssh_keys.len() > MAX_SSH_KEYS {
            Some(reason::TOO_MANY)
        } else if self.ssh_keys.iter().any(|k| has_control(k)) {
            Some(reason::CONTROL)
        } else if self.ssh_keys.iter().any(|k| k.len() > MAX_SSH_KEY) {
            Some(reason::TOO_LONG)
        } else if self.ssh_keys.iter().any(|k| !is_ssh_key(k)) {
            Some(reason::FORMAT)
        } else {
            None
        };
        check(Field::SshKeys, Some(""), &|_| keys);
        // Line 151
        check(Field::FullName, self.full_name.as_deref(), &|n| {
            if n.len() > MAX_FULL_NAME {
                Some(reason::TOO_LONG)
            } else if n.contains([':', ',']) {
                Some(reason::FORMAT)
            } else {
                None
            }
        });
        // Line 152: empty is no address
        check(Field::Email, self.email.as_deref(), &|e| {
            if e.len() > MAX_EMAIL {
                Some(reason::TOO_LONG)
            } else if !e.is_empty() && !is_email(e) {
                Some(reason::FORMAT)
            } else {
                None
            }
        });
        if problems.is_empty() {
            Ok(ValidAnswers(self.clone()))
        } else {
            Err(problems)
        }
    }
}

/// The lines `omamobile-firstboot --check --porcelain` prints, one
/// `KEY<TAB>reason` per objection, as problems. A line that is not of that
/// form, or names a key first boot does not have, is `None`: the phone's
/// shell then answers `internal`, not a guess.
pub fn parse_porcelain(text: &str) -> Option<Vec<Problem>> {
    text.lines()
        .filter(|l| !l.is_empty())
        .map(|l| {
            let (key, why) = l.split_once('\t')?;
            let field = Field::from_conf_key(key)?;
            is_reason(why).then(|| Problem::new(field, why))
        })
        .collect()
}
