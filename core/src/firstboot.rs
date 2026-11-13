// SPDX-License-Identifier: MIT
//! The owner's answers as the files a first start reads: what Omarchy
//! Mobile's `tools/install-phone` writes into its local copy of the root
//! image before it flashes it.
//!
//! **Nothing in this crate puts these files on a phone yet.** install-phone
//! writes them into the ext4 image with `debugfs`; the core has no ext4
//! writer and runs no other program (docs/DESIGN.md, "Open: the owner's
//! answers in the root image"). This module is everything up to that step:
//! the exact bytes, with the password already hashed, so that whichever way
//! is chosen has only the writing left to do.
//!
//! - `/etc/omamobile/firstboot.conf` (mode 0600, root): `KEY=value` lines
//!   that `omamobile-firstboot` reads as data and deletes. It holds the
//!   password's hash and any Wi-Fi password.
//! - `/root/.ssh/authorized_keys` (mode 0600, in a directory of mode 0700),
//!   when the owner gave an SSH public key.
//!
//! Not made here: `/etc/omamobile/boot-update.conf` with `AUTO_WRITE=yes`,
//! which install-phone also writes. It lets the phone write slot B's boot
//! images by itself on a kernel update, and Omarchy Mobile's rules name
//! install-phone as the one tool that writes it; whether this installer
//! does is the maintainer's decision. Nor the same key appended to slot B's
//! ramdisk image: that changes an image the release signed.
//!
//! The password is hashed here with sha512-crypt, the `$6$` scheme that
//! `openssl passwd -6` gives install-phone and that firstboot requires
//! (Ulrich Drepper, "Unix crypt using SHA-256 and SHA-512", public domain).
//! The salt comes from the front end: the core has no source of randomness.
use crate::flow::OwnerAnswers;
use crate::{Error, Result};
use sha2::{Digest, Sha512};

pub const FIRSTBOOT_CONF: &str = "/etc/omamobile/firstboot.conf";
pub const AUTHORIZED_KEYS: &str = "/root/.ssh/authorized_keys";

/// One file for the root image: where, with which mode (owner and group
/// are root), and its bytes.
#[derive(Clone, PartialEq, Eq)]
pub struct ImageFile {
    pub path: &'static str,
    pub mode: u32,
    pub content: Vec<u8>,
}

impl core::fmt::Debug for ImageFile {
    // The content holds a password hash, and may hold a Wi-Fi password
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("ImageFile")
            .field("path", &self.path)
            .field("mode", &format_args!("{:o}", self.mode))
            .field("bytes", &self.content.len())
            .finish()
    }
}

/// What install-phone asks beyond the flow's answers. All optional here.
#[derive(Clone, Default)]
pub struct Extras<'a> {
    /// One OpenSSH public key (the line of a `.pub` file), for root and the
    /// user. Never a private key.
    pub ssh_key: Option<&'a str>,
    /// For git, as Omarchy's own installer asks.
    pub full_name: Option<&'a str>,
    pub email: Option<&'a str>,
}

const B64: &[u8; 64] = b"./0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";
const ROUNDS: usize = 5000;

/// A salt for [`sha512_crypt`] from 12 random bytes: 16 characters.
pub fn salt_from(random: &[u8; 12]) -> String {
    let mut out = String::with_capacity(16);
    for three in random.chunks(3) {
        let w = (three[0] as u32) << 16 | (three[1] as u32) << 8 | three[2] as u32;
        for n in 0..4 {
            out.push(B64[((w >> (6 * n)) & 63) as usize] as char);
        }
    }
    out
}

/// `$6$<salt>$<hash>`: sha512-crypt with the default 5000 rounds. `salt`:
/// 1 to 16 characters of `./0-9A-Za-z`.
pub fn sha512_crypt(password: &[u8], salt: &str) -> Result<String> {
    let s = salt.as_bytes();
    if s.is_empty() || s.len() > 16 || !s.iter().all(|b| B64.contains(b)) {
        return Err(Error::Refused(
            "a salt is 1 to 16 characters of ./0-9A-Za-z".into(),
        ));
    }
    let p = password;
    let sha = |parts: &[&[u8]]| -> [u8; 64] {
        let mut h = Sha512::new();
        for part in parts {
            h.update(part);
        }
        h.finalize().into()
    };
    // `len` bytes of `block`, repeated
    let stretch = |block: &[u8; 64], len: usize| -> Vec<u8> {
        block.iter().copied().cycle().take(len).collect()
    };
    let b = sha(&[p, s, p]);
    let mut a = Sha512::new();
    a.update(p);
    a.update(s);
    a.update(stretch(&b, p.len()));
    let mut n = p.len();
    while n > 0 {
        if n & 1 == 1 {
            a.update(b);
        } else {
            a.update(p);
        }
        n >>= 1;
    }
    let a: [u8; 64] = a.finalize().into();
    let dp = {
        let mut h = Sha512::new();
        for _ in 0..p.len() {
            h.update(p);
        }
        let d: [u8; 64] = h.finalize().into();
        stretch(&d, p.len())
    };
    let ds = {
        let mut h = Sha512::new();
        for _ in 0..16 + a[0] as usize {
            h.update(s);
        }
        let d: [u8; 64] = h.finalize().into();
        stretch(&d, s.len())
    };
    let mut c = a;
    for round in 0..ROUNDS {
        let mut h = Sha512::new();
        if round % 2 == 1 {
            h.update(&dp);
        } else {
            h.update(c);
        }
        if round % 3 != 0 {
            h.update(&ds);
        }
        if round % 7 != 0 {
            h.update(&dp);
        }
        if round % 2 == 1 {
            h.update(c);
        } else {
            h.update(&dp);
        }
        c = h.finalize().into();
    }
    let mut out = format!("$6${salt}$");
    let mut put = |b2: u8, b1: u8, b0: u8, chars: usize| {
        let mut w = (b2 as u32) << 16 | (b1 as u32) << 8 | b0 as u32;
        for _ in 0..chars {
            out.push(B64[(w & 63) as usize] as char);
            w >>= 6;
        }
    };
    // The order the scheme takes the digest's bytes in
    let (mut i, mut j, mut k) = (0usize, 21usize, 42usize);
    for _ in 0..21 {
        put(c[i], c[j], c[k], 4);
        (i, j, k) = (j + 1, k + 1, i + 1);
    }
    put(0, 0, c[63], 2);
    Ok(out)
}

pub(crate) fn is_ssh_public_key(line: &str) -> bool {
    const KINDS: [&str; 7] = [
        "ssh-ed25519",
        "ssh-rsa",
        "ecdsa-sha2-nistp256",
        "ecdsa-sha2-nistp384",
        "ecdsa-sha2-nistp521",
        "sk-ssh-ed25519@openssh.com",
        "sk-ecdsa-sha2-nistp256@openssh.com",
    ];
    let mut parts = line.splitn(3, ' ');
    let (Some(kind), Some(key)) = (parts.next(), parts.next()) else {
        return false;
    };
    let body = key.trim_end_matches('=');
    KINDS.contains(&kind)
        && !body.is_empty()
        && key.len() - body.len() <= 3
        && body
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'+' || b == b'/')
        && parts
            .next()
            .is_none_or(|comment| comment.bytes().all(|b| (b' '..=b'~').contains(&b)))
}

/// The files for the root image, from answers that passed
/// [`OwnerAnswers::check`]. `salt`: from [`salt_from`], fresh for each
/// install. The rules for the extras are install-phone's.
pub fn files(answers: &OwnerAnswers, extras: &Extras<'_>, salt: &str) -> Result<Vec<ImageFile>> {
    let refuse = |why: &str| Err(Error::Refused(why.to_string()));
    if let Some(key) = extras.ssh_key {
        if !is_ssh_public_key(key) {
            return refuse(
                "not a single OpenSSH public key (a .pub file's line, never the private key)",
            );
        }
    }
    let plain = |v: &str| !v.is_empty() && !v.chars().any(char::is_control);
    if let Some(name) = extras.full_name {
        if !plain(name) || name.contains([':', ',']) {
            return refuse("the full name can't be empty or contain : or ,");
        }
    }
    if let Some(email) = extras.email {
        let ok = email.split_once('@').is_some_and(|(user, host)| {
            !user.is_empty()
                && !host.is_empty()
                && !host.contains('@')
                && !email.chars().any(|c| c.is_whitespace() || c.is_control())
        });
        if !ok {
            return refuse("that is not an email address");
        }
    }
    let hash = sha512_crypt(answers.password.as_bytes(), salt)?;
    let mut conf = String::from(
        "# Written by the Omarchy Mobile installer on the computer; omamobile-firstboot reads and deletes it.\n",
    );
    let mut line = |key: &str, value: &str| {
        conf.push_str(key);
        conf.push('=');
        conf.push_str(value);
        conf.push('\n');
    };
    line("USERNAME", &answers.username);
    line("PASSWORD_HASH", &hash);
    line("HOSTNAME", &answers.hostname);
    line("TIMEZONE", &answers.timezone);
    if let Some(key) = extras.ssh_key {
        line("SSH_KEY", key);
    }
    line(
        "CREDENTIAL",
        if answers.summary().pin {
            "pin"
        } else {
            "password"
        },
    );
    if let Some(name) = extras.full_name {
        line("FULL_NAME", name);
    }
    if let Some(email) = extras.email {
        line("EMAIL", email);
    }
    if let Some((ssid, psk)) = &answers.wifi {
        line("WIFI_SSID", ssid);
        if let Some(psk) = psk {
            line("WIFI_PSK", psk);
        }
    }
    let mut out = vec![ImageFile {
        path: FIRSTBOOT_CONF,
        mode: 0o600,
        content: conf.into_bytes(),
    }];
    if let Some(key) = extras.ssh_key {
        out.push(ImageFile {
            path: AUTHORIZED_KEYS,
            mode: 0o600,
            content: format!("{key}\n").into_bytes(),
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::flow::AnswersInput;

    #[test]
    fn sha512_crypt_gives_what_the_scheme_and_openssl_give() {
        // The scheme's own test vector (SHA-crypt.txt)
        assert_eq!(
            sha512_crypt(b"Hello world!", "saltstring").unwrap(),
            "$6$saltstring$svn8UoSVapNtMuq1ukKS4tPQd8iKwSMHWjl/O817G3uBnIFNjnQJuesI68u4OTLiBFdcbYEdFCoEOfaS35inz1"
        );
        // `openssl passwd -6 -salt SALT`, as install-phone hashes: a PIN, a
        // password longer than one digest, and one that is not ASCII
        for (password, salt, want) in OPENSSL {
            assert_eq!(sha512_crypt(password.as_bytes(), salt).unwrap(), want);
        }
        for bad in ["", "seventeen-chars-x", "with$dollar", "with space", "ü"] {
            assert!(sha512_crypt(b"x", bad).is_err(), "{bad:?}");
        }
        let salt = salt_from(&[0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 250, 255]);
        assert_eq!(salt.len(), 16);
        assert!(sha512_crypt(b"4812", &salt)
            .unwrap()
            .starts_with(&format!("$6${salt}$")));
        assert_ne!(salt, salt_from(&[9; 12]));
    }

    const OPENSSL: [(&str, &str, &str); 3] = [
        (
            "4812",
            "abcdefgh12345678",
            "$6$abcdefgh12345678$PI8Y71WetbDeVCXJTwpBRiv7MzcXsuGxcpSVh7Z4buYwCas.brG4eiy1byP/bziaFhvRNvNxpSv9z659PRcsI/",
        ),
        (
            "a-password-that-is-much-longer-than-sixty-four-bytes-of-sha512-digest-output-here",
            "saltstring",
            "$6$saltstring$EtvSaon4mhyYFfit2.QHyGdXolIJkMUVC4RLGpuvZLlEsghQUidwdk/jUgdj.L6qH4eaikP9GVrKDQGWRtlNl1",
        ),
        (
            "pässwörd",
            "./ABab01",
            "$6$./ABab01$Oj39mjI/PxFPnlGypLWGkSv7ZJcr49ST5/sV4XgnG7tg/WRs66TlzTxC4jNCQIBx3sDxfeUxQEuU6.4XxV7aV0",
        ),
    ];

    fn answers(password: &str, wifi: (&str, &str)) -> OwnerAnswers {
        OwnerAnswers::check(&AnswersInput {
            username: "alex",
            password,
            password_again: password,
            hostname: "",
            timezone: "Europe/Paris",
            wifi_ssid: wifi.0,
            wifi_psk: wifi.1,
        })
        .unwrap()
    }

    #[test]
    fn the_files_are_install_phones_line_for_line() {
        let key = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIPlaceholderPlaceholderPlaceholderPlacehol alex@laptop";
        let extras = Extras {
            ssh_key: Some(key),
            full_name: Some("Alex Example"),
            email: Some("alex@example.org"),
        };
        let files = files(
            &answers("4812", ("Home network", "correct horse")),
            &extras,
            "saltstring",
        )
        .unwrap();
        assert_eq!(files.len(), 2);
        assert_eq!((files[0].path, files[0].mode), (FIRSTBOOT_CONF, 0o600));
        let hash = sha512_crypt(b"4812", "saltstring").unwrap();
        assert_eq!(
            String::from_utf8(files[0].content.clone()).unwrap(),
            format!("# Written by the Omarchy Mobile installer on the computer; omamobile-firstboot reads and deletes it.\nUSERNAME=alex\nPASSWORD_HASH={hash}\nHOSTNAME=alex-phone\nTIMEZONE=Europe/Paris\nSSH_KEY={key}\nCREDENTIAL=pin\nFULL_NAME=Alex Example\nEMAIL=alex@example.org\nWIFI_SSID=Home network\nWIFI_PSK=correct horse\n")
        );
        assert_eq!((files[1].path, files[1].mode), (AUTHORIZED_KEYS, 0o600));
        assert_eq!(files[1].content, format!("{key}\n").into_bytes());
        // The password itself is in no file, and no file's content is in
        // its debug output
        assert!(!String::from_utf8_lossy(&files[0].content).contains("4812"));
        assert!(!format!("{files:?}").contains("PASSWORD_HASH"));
        assert!(!format!("{files:?}").contains("correct horse"));

        // Without the extras and without Wi-Fi: one file, a password
        let files = super::files(
            &answers("a long passphrase", ("", "")),
            &Extras::default(),
            "saltstring",
        )
        .unwrap();
        assert_eq!(files.len(), 1);
        let conf = String::from_utf8(files[0].content.clone()).unwrap();
        assert!(conf.contains("\nCREDENTIAL=password\n"));
        for absent in ["SSH_KEY", "FULL_NAME", "EMAIL", "WIFI_"] {
            assert!(!conf.contains(absent), "{absent}");
        }
        // An open network has a name and no password line
        let files = super::files(
            &answers("4812", ("Cafe", "")),
            &Extras::default(),
            "saltstring",
        )
        .unwrap();
        let conf = String::from_utf8(files[0].content.clone()).unwrap();
        assert!(conf.ends_with("\nWIFI_SSID=Cafe\n"));
    }

    #[test]
    fn the_extras_follow_install_phones_rules() {
        let a = answers("4812", ("", ""));
        let with = |ssh_key, full_name, email| {
            files(
                &a,
                &Extras {
                    ssh_key,
                    full_name,
                    email,
                },
                "saltstring",
            )
        };
        for key in [
            "ssh-ed25519 AAAA",
            "ssh-rsa AAAA+/== a comment with spaces",
            "sk-ssh-ed25519@openssh.com AAAA=",
            "ecdsa-sha2-nistp521 AAAA",
        ] {
            assert!(with(Some(key), None, None).is_ok(), "{key}");
        }
        for key in [
            "",
            "ssh-ed25519",
            "ssh-ed25519 ",
            "ssh-dss AAAA",
            "-----BEGIN OPENSSH PRIVATE KEY-----",
            "ssh-ed25519 AAAA\nssh-ed25519 BBBB",
            "ssh-ed25519 AA AA\u{7f}",
            "ssh-ed25519 AAAA====",
            "ssh-ed25519 AA=A",
            " ssh-ed25519 AAAA",
        ] {
            assert!(with(Some(key), None, None).is_err(), "{key:?}");
        }
        for name in ["", "a:b", "a,b", "a\nUSERNAME=root"] {
            assert!(with(None, Some(name), None).is_err(), "{name:?}");
        }
        for email in [
            "",
            "alex",
            "@example.org",
            "alex@",
            "a b@example.org",
            "a@b@c",
            "a@b\nX=1",
        ] {
            assert!(with(None, None, Some(email)).is_err(), "{email:?}");
        }
    }
}
