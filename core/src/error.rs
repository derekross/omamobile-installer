// SPDX-License-Identifier: MIT
use core::fmt;

/// Why the installer stopped. Each variant names the layer that refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// `manifest.json` is not a valid format 1 release manifest.
    Manifest(String),
    /// `device.conf` is not a valid descriptor, or disagrees with the manifest.
    DeviceConf(String),
    /// A file does not match its signed checksum or size.
    Checksum(String),
    /// The signature is missing, invalid or not by a trusted key.
    Signature(String),
    /// A safety rule refused the request (allowlist, order, pinning).
    Refused(String),
    /// The phone is not in a state the install accepts.
    Phone(String),
    /// The phone answered something the fastboot protocol does not allow here.
    Protocol(String),
    /// The phone answered FAIL.
    DeviceFail { command: String, message: String },
    /// Fewer bytes were accepted than the download needs.
    ShortDownload { wanted: u64, accepted: u64 },
    /// The USB transport failed.
    Transport(String),
    /// The image cannot be cut into sparse images.
    Sparse(String),
    /// The zstd stream is damaged or unsupported.
    Zstd(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Manifest(s) => write!(f, "manifest.json: {s}"),
            Error::DeviceConf(s) => write!(f, "device.conf: {s}"),
            Error::Checksum(s) => write!(f, "checksum: {s}"),
            Error::Signature(s) => write!(f, "signature: {s}"),
            Error::Refused(s) => write!(f, "refused: {s}"),
            Error::Phone(s) => write!(f, "phone: {s}"),
            Error::Protocol(s) => write!(f, "fastboot protocol: {s}"),
            Error::DeviceFail { command, message } => {
                write!(f, "the phone answered FAIL to '{command}': {message}")
            }
            Error::ShortDownload { wanted, accepted } => {
                write!(f, "short download: {accepted} of {wanted} bytes accepted")
            }
            Error::Transport(s) => write!(f, "USB: {s}"),
            Error::Sparse(s) => write!(f, "sparse image: {s}"),
            Error::Zstd(s) => write!(f, "zstd: {s}"),
        }
    }
}

impl std::error::Error for Error {}
