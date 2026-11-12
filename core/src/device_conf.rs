// SPDX-License-Identifier: MIT
//! `device.conf`: `KEY=value` or `KEY="value"` lines, comments and blank
//! lines (docs/RELEASES.md §3). Data, never shell: a line with `$`, a
//! backtick, a backslash or (unquoted) a quote or a space is refused.
use crate::{Error, Result};
use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceConf {
    keys: BTreeMap<String, String>,
}

fn bad(n: usize, why: &str) -> Error {
    Error::DeviceConf(format!("line {n}: {why}"))
}

impl DeviceConf {
    pub fn parse(text: &[u8]) -> Result<Self> {
        let text = core::str::from_utf8(text).map_err(|_| Error::DeviceConf("not UTF-8".into()))?;
        let mut keys = BTreeMap::new();
        for (i, line) in text.split('\n').enumerate() {
            let n = i + 1;
            let trimmed = line.trim_start_matches([' ', '\t']);
            if trimmed.is_empty() || trimmed.starts_with('#') {
                continue;
            }
            let (key, value) = line
                .split_once('=')
                .ok_or_else(|| bad(n, "not a KEY=value line"))?;
            let mut kb = key.bytes();
            let first_ok = kb.next().is_some_and(|b| b.is_ascii_uppercase());
            if !first_ok || !kb.all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_') {
                return Err(bad(n, "not a KEY=value line"));
            }
            let forbidden = |c: char| matches!(c, '"' | '$' | '`' | '\\');
            let value = if let Some(rest) = value.strip_prefix('"') {
                let inner = rest
                    .strip_suffix('"')
                    .ok_or_else(|| bad(n, "unterminated quote"))?;
                if inner.contains(forbidden) {
                    return Err(bad(n, "a quoted value can't hold \", $, ` or \\"));
                }
                inner
            } else {
                if value.contains(|c: char| forbidden(c) || c == '\'' || c.is_whitespace()) {
                    return Err(bad(
                        n,
                        "an unquoted value can't hold quotes, spaces, $, ` or \\",
                    ));
                }
                value
            };
            keys.insert(key.to_string(), value.to_string());
        }
        Ok(Self { keys })
    }

    pub fn get(&self, key: &str) -> Option<&str> {
        self.keys.get(key).map(String::as_str)
    }

    /// A space-separated list; empty when the key is absent.
    pub fn list(&self, key: &str) -> Vec<&str> {
        self.get(key)
            .map(|v| v.split(' ').filter(|s| !s.is_empty()).collect())
            .unwrap_or_default()
    }

    /// `BOOTLOADER_SLOT_PROBE`: whether this device's maintainers allow the
    /// installer to read the other slot's bootloader version by making that
    /// slot active and switching back. Off unless the descriptor says `yes`;
    /// anything but `yes` or `no` is refused. No published descriptor sets
    /// it, and this version of the installer never switches slots either way
    /// (docs/DESIGN.md, "The bootloader's two slots").
    pub fn bootloader_slot_probe(&self) -> Result<bool> {
        match self.get("BOOTLOADER_SLOT_PROBE") {
            None | Some("no") => Ok(false),
            Some("yes") => Ok(true),
            Some(other) => Err(Error::DeviceConf(format!(
                "BOOTLOADER_SLOT_PROBE is yes or no, not {other}"
            ))),
        }
    }

    /// `AVB_FOOTERS` as (image, partition size in bytes).
    pub fn avb_footers(&self) -> Result<Vec<(String, u64)>> {
        let mut out = Vec::new();
        for entry in self.list("AVB_FOOTERS") {
            let err = || {
                Error::DeviceConf(format!(
                    "AVB_FOOTERS entries are image:partition_size, not {entry}"
                ))
            };
            let (image, size) = entry.split_once(':').ok_or_else(err)?;
            let size = match size.strip_prefix("0x") {
                Some(h)
                    if !h.is_empty()
                        && h.len() <= 10
                        && h.bytes()
                            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)) =>
                {
                    u64::from_str_radix(h, 16).map_err(|_| err())?
                }
                None if !size.is_empty()
                    && size.len() <= 12
                    && !size.starts_with('0')
                    && size.bytes().all(|b| b.is_ascii_digit()) =>
                {
                    size.parse().map_err(|_| err())?
                }
                _ => return Err(err()),
            };
            if image.is_empty() || !image.bytes().all(|b| b.is_ascii_lowercase() || b == b'_') {
                return Err(err());
            }
            out.push((image.to_string(), size));
        }
        Ok(out)
    }
}
