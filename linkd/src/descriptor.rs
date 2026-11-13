// SPDX-License-Identifier: MIT
//! `/usr/share/omamobile/device.conf`: `KEY=value` lines, the value plain
//! or in double quotes, as the scripts read it.
use std::collections::BTreeMap;

#[derive(Debug, Clone, Default)]
pub struct Descriptor(BTreeMap<String, String>);

impl Descriptor {
    pub fn parse(text: &str) -> Descriptor {
        let mut m = BTreeMap::new();
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let Some((k, v)) = line.split_once('=') else {
                continue;
            };
            let v = v.trim();
            let v = v
                .strip_prefix('"')
                .and_then(|x| x.strip_suffix('"'))
                .unwrap_or(v);
            m.insert(k.trim().to_string(), v.to_string());
        }
        Descriptor(m)
    }
    pub fn get(&self, key: &str) -> Option<&str> {
        self.0.get(key).map(String::as_str)
    }
    /// A list of words.
    pub fn words(&self, key: &str) -> Vec<String> {
        self.get(key)
            .unwrap_or("")
            .split_whitespace()
            .map(str::to_string)
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn values_plain_and_quoted() {
        let d = Descriptor::parse(
            "# a comment\nCODENAME=cheetah\nNV_PARTS=\"efs persist\"\n\nCOMPATIBLE=\"a,b\"\nnot a line\n",
        );
        assert_eq!(d.get("CODENAME"), Some("cheetah"));
        assert_eq!(d.words("NV_PARTS"), ["efs", "persist"]);
        assert_eq!(d.get("COMPATIBLE"), Some("a,b"));
        assert!(d.words("NV_BACKUP_EXTRA").is_empty());
    }
}
