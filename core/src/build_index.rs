// SPDX-License-Identifier: MIT
//! The build index, `factory-builds.json` (Omarchy Mobile's
//! docs/INSTALL-GUIDED.md §5, written by its `tools/factory-index`): which of
//! Google's factory images matches a phone. Metadata only, never a file of
//! Google's: for each build its id, the bootloader and baseband versions it
//! carries, the zip's name, size and SHA-256, its address on Google's
//! servers, the page the installer opens for the owner, and whether a release
//! was tested with that build's firmware.
//!
//! Three rules live here:
//!
//! - **Read as data, every field against a pattern.** A file that breaks one
//!   rule is refused whole ([`IndexData::parse`]).
//! - **Trusted only through a verified release.** The installer believes the
//!   SHA-256 it finds here, so a [`BuildIndex`] can only be made by
//!   [`crate::bundle::VerifiedRelease::build_index`], from bytes the release's
//!   signed checksums cover.
//! - **The match is both versions, and tested, or nothing**
//!   ([`BuildIndex::lookup`]). There is no nearest build: the `tested` mark is
//!   one per build, earned with that build's own bootloader and baseband.
//!
//! `url` is the one address anything of Google's is ever fetched from (the
//! maintainer's decision of 2026-11-12): once the owner has accepted Google's
//! terms in the flow, a front end fetches the zip from it, and the core checks
//! every answer ([`crate::download`]). [`is_download_url`] is the rule for it,
//! here and on every redirect. `page` is what a front end opens for the owner.
use crate::hash::is_sha256_hex;
use crate::manifest::is_calendar_version;
use crate::{Error, Result};
use serde::Deserialize;

/// The index's path inside a release bundle.
pub const FILE: &str = "factory-builds.json";
pub const FORMAT: u64 = 1;
const DOWNLOAD_HOST: &str = "https://dl.google.com/";
const PAGE: &str = "https://developers.google.com/android/images";

// Closed, as the manifest's install plan is: a key this installer doesn't
// know could be a rule it would ignore (a withdrawn build, say), so an
// unknown one is refused. Stricter than tools/factory-index, which drops
// them.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawIndex {
    format: u64,
    codename: String,
    updated: String,
    builds: Vec<RawBuild>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawBuild {
    build: String,
    bootloader: String,
    baseband: String,
    zip: String,
    url: String,
    page: String,
    sha256: String,
    size: u64,
    tested: bool,
    #[serde(default)]
    tested_with: Option<String>,
}

/// One of Google's factory builds, every field checked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Build {
    /// Google's build id, for example `CP2A.260705.006`.
    pub build: String,
    pub bootloader: String,
    pub baseband: String,
    /// The zip's name as Google publishes it.
    pub zip: String,
    /// On `https://dl.google.com/`. Fetched only through
    /// [`crate::flow::Flow::factory_download`], after Google's terms were
    /// accepted.
    pub url: String,
    /// Google's factory images page, which a front end opens for the owner.
    pub page: String,
    pub sha256: String,
    pub size: u64,
    /// The release this build's firmware was tested with, if any.
    pub tested_with: Option<String>,
}

impl Build {
    pub fn tested(&self) -> bool {
        self.tested_with.is_some()
    }
}

/// A build index that passed every rule. Checked, not yet trusted: see
/// [`BuildIndex`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexData {
    pub codename: String,
    /// `YYYY-MM-DD`.
    pub updated: String,
    /// Newest first, as the file lists them.
    pub builds: Vec<Build>,
}

fn err<T>(why: impl Into<String>) -> Result<T> {
    Err(Error::BuildIndex(why.into()))
}

fn all(s: &str, ok: impl Fn(u8) -> bool) -> bool {
    !s.is_empty() && s.bytes().all(ok)
}
fn digits(s: &str, len: usize) -> Option<u32> {
    (s.len() == len && s.bytes().all(|b| b.is_ascii_digit()))
        .then(|| s.parse().ok())
        .flatten()
}
/// `YYYY-MM-DD`, in this century.
fn is_date(s: &str) -> bool {
    let mut p = s.split('-');
    let (Some(y), Some(m), Some(d), None) = (p.next(), p.next(), p.next(), p.next()) else {
        return false;
    };
    matches!(digits(y, 4), Some(2000..=2099))
        && matches!(digits(m, 2), Some(1..=12))
        && matches!(digits(d, 2), Some(1..=31))
}
/// `LETTERS.YYMMDD.NNN` with optional further parts, as Google names builds.
fn is_build_id(s: &str) -> bool {
    let upper = |p: &str| all(p, |b| b.is_ascii_uppercase() || b.is_ascii_digit());
    let mut p = s.split('.');
    let (Some(a), Some(date), Some(n)) = (p.next(), p.next(), p.next()) else {
        return false;
    };
    upper(a) && digits(date, 6).is_some() && digits(n, 3).is_some() && p.all(upper)
}
/// A bootloader or baseband version, as fastboot reports it.
fn is_version(s: &str) -> bool {
    s.as_bytes().first().is_some_and(u8::is_ascii_alphanumeric)
        && all(s, |b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
}

/// Whether `url` is an address the installer may fetch Google's factory
/// image from: `https`, the host `dl.google.com` and nothing else. The host
/// is fixed, and no character that could name another one (@, :, ?, #, %, a
/// backslash) is allowed after it. The one rule for the index's `url` and for
/// every redirect a download is sent on.
pub fn is_download_url(url: &str) -> bool {
    url.strip_prefix(DOWNLOAD_HOST).is_some_and(|path| {
        all(path, |c| c.is_ascii_alphanumeric() || b"._/-".contains(&c))
            && !path.split('/').any(|part| part == ".." || part == ".")
    })
}

fn check_build(b: RawBuild, codename: &str) -> Result<Build> {
    if !is_build_id(&b.build) {
        return err("a build id is not valid");
    }
    let what = format!("build {}", b.build);
    if !is_version(&b.bootloader) {
        return err(format!("{what}: bootloader is not valid"));
    }
    if !is_version(&b.baseband) {
        return err(format!("{what}: baseband is not valid"));
    }
    if !is_sha256_hex(&b.sha256) {
        return err(format!("{what}: sha256 is not valid"));
    }
    if b.size == 0 {
        return err(format!("{what}: size is not a size"));
    }
    // <codename>-<build, lower-case>-factory-<the SHA-256's first eight digits>.zip
    let want_zip = format!(
        "{codename}-{}-factory-{}.zip",
        b.build.to_ascii_lowercase(),
        &b.sha256[..8]
    );
    if b.zip != want_zip {
        let named_for_build = b
            .zip
            .strip_prefix(&want_zip[..want_zip.len() - 12])
            .is_some_and(|rest| {
                rest.len() == 12
                    && rest.ends_with(".zip")
                    && rest[..8]
                        .bytes()
                        .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
            });
        return err(if named_for_build {
            format!("{what}: the zip's name doesn't carry the first eight digits of its SHA-256")
        } else {
            format!("{what}: the zip is not named for {codename} and this build")
        });
    }
    if !is_download_url(&b.url) {
        return err(format!("{what}: url is not on dl.google.com"));
    }
    if !b.url.ends_with(&format!("/{}", b.zip)) {
        return err(format!("{what}: url doesn't end in the zip's name"));
    }
    let page_ok = b.page.strip_prefix(PAGE).is_some_and(|rest| {
        rest.is_empty()
            || rest.strip_prefix('#').is_some_and(|anchor| {
                all(anchor, |c| c.is_ascii_alphanumeric() || b"._-".contains(&c))
            })
    });
    if !page_ok {
        return err(format!(
            "{what}: page is not Google's factory images page on developers.google.com"
        ));
    }
    if b.tested_with
        .as_deref()
        .is_some_and(|r| !is_calendar_version(r))
    {
        return err(format!("{what}: tested_with is not a release"));
    }
    if b.tested != b.tested_with.is_some() {
        return err(format!(
            "{what}: a tested build names the release it was tested with (tested_with), and only a tested one"
        ));
    }
    Ok(Build {
        build: b.build,
        bootloader: b.bootloader,
        baseband: b.baseband,
        zip: b.zip,
        url: b.url,
        page: b.page,
        sha256: b.sha256,
        size: b.size,
        tested_with: b.tested_with,
    })
}

impl IndexData {
    /// Check a `factory-builds.json` for the device `codename`. This says the
    /// file is well formed, not that it is the project's: use
    /// [`crate::bundle::VerifiedRelease::build_index`] for one to act on.
    pub fn parse(json: &[u8], codename: &str) -> Result<Self> {
        let raw: RawIndex = serde_json::from_slice(json)
            .map_err(|e| Error::BuildIndex(format!("not a build index ({e})")))?;
        if raw.format != FORMAT {
            return err(format!(
                "format {}; this installer reads format {FORMAT}",
                raw.format
            ));
        }
        if !all(&raw.codename, |b| {
            b.is_ascii_lowercase() || b.is_ascii_digit()
        }) {
            return err("codename is not valid");
        }
        if raw.codename != codename {
            return err(format!("is for {}, not {codename}", raw.codename));
        }
        if !is_date(&raw.updated) {
            return err("updated is not a date");
        }
        let mut builds: Vec<Build> = Vec::with_capacity(raw.builds.len());
        for b in raw.builds {
            let b = check_build(b, codename)?;
            if builds.iter().any(|o| o.build == b.build) {
                return err(format!("build {} is listed twice", b.build));
            }
            builds.push(b);
        }
        Ok(IndexData {
            codename: raw.codename,
            updated: raw.updated,
            builds,
        })
    }
}

/// A build index the release key vouches for: its bytes matched a verified
/// release's signed checksums. Only [`crate::bundle::VerifiedRelease`] makes
/// one.
#[derive(Debug, Clone)]
pub struct BuildIndex {
    data: IndexData,
    /// The release it came from (SHA-256 of that release's `manifest.json`).
    release: String,
}

/// What the index says about a phone's bootloader and baseband.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Lookup<'a> {
    /// A build carries both versions and is marked tested: the one to use.
    Tested {
        build: &'a Build,
        /// Other builds with the same two versions (carrier variants).
        /// Fastboot can't tell them apart; the first tested one is used.
        same_pair: Vec<&'a str>,
    },
    /// A build carries both versions, but none that does is marked tested.
    NotTested {
        build: &'a Build,
        /// A tested build listed before it, so newer, if there is one.
        newer_tested: Option<&'a Build>,
    },
    /// No build carries both versions. A build with one of them is not a
    /// match.
    NotListed,
}

impl BuildIndex {
    pub(crate) fn new(data: IndexData, release: String) -> Self {
        Self { data, release }
    }
    pub(crate) fn release(&self) -> &str {
        &self.release
    }
    pub fn data(&self) -> &IndexData {
        &self.data
    }

    /// The build for a phone that reports these two versions in fastboot.
    pub fn lookup(&self, bootloader: &str, baseband: &str) -> Lookup<'_> {
        let builds = &self.data.builds;
        let same: Vec<(usize, &Build)> = builds
            .iter()
            .enumerate()
            .filter(|(_, b)| b.bootloader == bootloader && b.baseband == baseband)
            .collect();
        let Some((first_at, first)) = same.first().copied() else {
            return Lookup::NotListed;
        };
        match same.iter().find(|(_, b)| b.tested()) {
            Some((_, build)) => Lookup::Tested {
                build,
                same_pair: same
                    .iter()
                    .filter(|(_, b)| b.build != build.build)
                    .map(|(_, b)| b.build.as_str())
                    .collect(),
            },
            None => Lookup::NotTested {
                build: first,
                newer_tested: builds[..first_at].iter().rev().find(|b| b.tested()),
            },
        }
    }
}
