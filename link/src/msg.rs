// SPDX-License-Identifier: MIT
//! Every message of protocol 1 (INSTALL-LINK-PLAN §5.2 to §5.6) as a type,
//! with one writer and one strict reader each way.
//!
//! The reader refuses what the protocol does not say: a key it does not
//! know, a key that is missing, a value of another type, a string over its
//! limit or outside its form, a number outside its range, a list that is too
//! long. `hello` (both ways) and the `version` error are the exception: they
//! pass keys they do not know, so two sides that speak different protocol
//! numbers can still find that out (INSTALL-LINK-PLAN §5.2).
//!
//! A message is written only if this reader would take it back
//! ([`HostFrame::to_json`], [`PhoneFrame::to_json`]): neither side can send
//! what the other must refuse.
use crate::answers::{is_reason, Answers, Field, Problem};
use crate::digest::{from_hex32, to_hex};
use crate::frame::{MAX_DATA_LIMIT, MAX_MESSAGE, MIN_MAX_DATA};
use crate::json::{self, array, quoted, JsonError, ObjWriter, Value, MAX_NUMBER};
use core::fmt;

/// The factory image is at most 8 GiB (§5.6). A larger `size` is read, and
/// answered `too-large`.
pub const MAX_FACTORY_SIZE: u64 = 8 << 30;
/// The longest list of protocol numbers.
pub const MAX_PROTOS: usize = 16;
/// The most partitions a backup lists.
pub const MAX_PARTS: usize = 32;
/// The most steps a list names.
pub const MAX_STEPS: usize = 64;
/// A partition name: 1 to 36 of `A-Z a-z 0-9 _ -` (a GPT name holds 36).
pub const MAX_PART_NAME: usize = 36;
/// A version, a release, a build, a step's name: printable ASCII.
pub const MAX_TEXT: usize = 64;
/// `kernel` (`uname -r`).
pub const MAX_KERNEL: usize = 128;
/// `error.message`.
pub const MAX_ERROR_TEXT: usize = 256;
/// `factory-begin.name`, shown only.
pub const MAX_FILE_NAME: usize = 255;

/// Why a payload is not a message of protocol 1.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MsgError {
    /// Not JSON (or JSON this crate refuses: a key twice, nesting too deep).
    Json(JsonError),
    /// JSON, and not an object.
    NotObject,
    /// No `t`, or one that is not a string.
    NoType,
    /// A `t` protocol 1 does not have. `id` is the message's, when it has a
    /// readable one, for the `unknown` reply.
    UnknownType { id: Option<u64> },
    /// A field that is wrong, by message and name.
    Field {
        t: &'static str,
        field: String,
        why: Why,
    },
    /// Written, the message would be over 262144 bytes.
    TooLong { t: &'static str, len: usize },
}

/// What is wrong with a field.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Why {
    Missing,
    /// A key the message does not have.
    Unknown,
    /// Not the JSON type the field has.
    Type,
    TooLong,
    TooMany,
    /// A number outside the field's range.
    Range,
    /// A string that does not have the field's form.
    Format,
}

impl fmt::Display for MsgError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MsgError::Json(e) => write!(f, "not JSON: {e}"),
            MsgError::NotObject => write!(f, "not a JSON object"),
            MsgError::NoType => write!(f, "no message type"),
            MsgError::UnknownType { .. } => write!(f, "a message type protocol 1 does not have"),
            MsgError::Field { t, field, why } => {
                let why = match why {
                    Why::Missing => "is missing",
                    Why::Unknown => "is not a field of the message",
                    Why::Type => "has the wrong type",
                    Why::TooLong => "is too long",
                    Why::TooMany => "has too many entries",
                    Why::Range => "is out of range",
                    Why::Format => "does not have the field's form",
                };
                write!(f, "{t}: {field} {why}")
            }
            MsgError::TooLong { t, len } => write!(f, "{t}: {len} bytes, over {MAX_MESSAGE}"),
        }
    }
}

impl std::error::Error for MsgError {}

// --- Small types -----------------------------------------------------------

/// The stages of §5.4, in order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    Waiting,
    Firstboot,
    FirstbootFailed,
    FirstbootDone,
    Provisioned,
    Finished,
}

impl Stage {
    pub const ALL: [Stage; 6] = [
        Stage::Waiting,
        Stage::Firstboot,
        Stage::FirstbootFailed,
        Stage::FirstbootDone,
        Stage::Provisioned,
        Stage::Finished,
    ];
    pub fn as_str(self) -> &'static str {
        match self {
            Stage::Waiting => "waiting",
            Stage::Firstboot => "firstboot",
            Stage::FirstbootFailed => "firstboot-failed",
            Stage::FirstbootDone => "firstboot-done",
            Stage::Provisioned => "provisioned",
            Stage::Finished => "finished",
        }
    }
    pub fn from_wire(s: &str) -> Option<Stage> {
        Stage::ALL.into_iter().find(|x| x.as_str() == s)
    }
    /// "Before `provisioned`", as §5.5 and §5.6 use it.
    pub fn before_provisioned(self) -> bool {
        !matches!(self, Stage::Provisioned | Stage::Finished)
    }
}

macro_rules! wire_enum {
    ($(#[$m:meta])* $name:ident { $($var:ident => $s:literal),+ $(,)? }) => {
        $(#[$m])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        pub enum $name { $($var),+ }
        impl $name {
            pub const ALL: &'static [$name] = &[$($name::$var),+];
            pub fn as_str(self) -> &'static str { match self { $($name::$var => $s),+ } }
            pub fn from_wire(s: &str) -> Option<$name> { match s { $($s => Some($name::$var),)+ _ => None } }
        }
    };
}

wire_enum!(
    /// `hello.pair`: what the host can pair with now (§5.3). `code`: a code
    /// is showing, `pair` is taken. `ask`: no code is showing; the owner can
    /// make the phone show one (§2.8). `token`: no code can be shown now,
    /// and the phone holds a token. `locked`: `pair` is locked for `wait_s`
    /// seconds; 0 is until someone acts at the phone or it restarts, or: no
    /// code can be shown and no token exists. `resume` is taken in every one
    /// of the four whenever the phone holds a token.
    PairMode { Code => "code", Ask => "ask", Token => "token", Locked => "locked" }
);
wire_enum!(
    /// A job on the phone: first boot's setup, provisioning.
    JobState { None => "none", Running => "running", Done => "done", Failed => "failed" }
);
wire_enum!(
    /// `status.backup.state` (§5.4).
    BackupState { None => "none", Listed => "listed", Done => "done" }
);
wire_enum!(
    /// `status.factory.state` (§5.4).
    FactoryState { None => "none", Partial => "partial", Verified => "verified" }
);
wire_enum!(
    /// `progress.what` (§5.6).
    What {
        FactoryCheck => "factory-check",
        Provision => "provision",
        BackupVerify => "backup-verify",
        Smoke => "smoke",
        Working => "working",
    }
);
wire_enum!(
    /// `provision-done.steps[].state`.
    StepState { Ok => "ok", Failed => "failed", Skipped => "skipped" }
);

/// The six digits the phone shows.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Code([u8; 6]);

impl Code {
    /// Six ASCII digits, or nothing.
    pub fn parse(s: &str) -> Option<Code> {
        let b: [u8; 6] = s.as_bytes().try_into().ok()?;
        b.iter().all(u8::is_ascii_digit).then_some(Code(b))
    }
    /// A number below 1000000 as six digits.
    pub fn from_number(n: u32) -> Code {
        let mut n = n % 1_000_000;
        let mut b = [b'0'; 6];
        for d in b.iter_mut().rev() {
            *d = b'0' + (n % 10) as u8;
            n /= 10;
        }
        Code(b)
    }
    /// The digits, for the phone's screen and for the message. Not for a log.
    pub fn digits(&self) -> &str {
        core::str::from_utf8(&self.0).unwrap_or("000000")
    }
    /// The six digits as bytes, for the comparison. Not for a log.
    pub fn bytes(&self) -> &[u8; 6] {
        &self.0
    }
}

impl fmt::Debug for Code {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Code(withheld)")
    }
}

/// The 32-byte session token. Not for a log.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Token(pub [u8; 32]);

impl fmt::Debug for Token {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Token(withheld)")
    }
}

pub(crate) fn is_text(s: &str) -> bool {
    s.bytes().all(|b| (0x20..=0x7e).contains(&b))
}
pub(crate) fn is_text1(s: &str) -> bool {
    !s.is_empty() && is_text(s)
}
/// A partition's name: 1 to 36 of `A-Z a-z 0-9 _ -`.
pub fn is_part_name(s: &str) -> bool {
    (1..=MAX_PART_NAME).contains(&s.len())
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}
fn is_device(s: &str) -> bool {
    !s.is_empty()
        && s.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-')
}
fn no_control(s: &str) -> bool {
    !s.is_empty() && !s.chars().any(char::is_control)
}
pub(crate) fn any(_: &str) -> bool {
    true
}

// --- The reader ------------------------------------------------------------

pub(crate) struct Rd<'a> {
    t: &'static str,
    path: &'static str,
    items: &'a [(String, Value)],
    seen: Vec<bool>,
    lenient: bool,
}

pub(crate) type R<T> = Result<T, MsgError>;

impl<'a> Rd<'a> {
    pub(crate) fn new(
        t: &'static str,
        path: &'static str,
        key: &str,
        v: &'a Value,
        lenient: bool,
    ) -> R<Rd<'a>> {
        match v {
            Value::Obj(items) => Ok(Rd {
                t,
                path,
                items,
                seen: vec![false; items.len()],
                lenient,
            }),
            _ => Err(MsgError::Field {
                t,
                field: key.to_string(),
                why: Why::Type,
            }),
        }
    }
    pub(crate) fn name(&self, key: &str) -> String {
        if self.path.is_empty() {
            key.to_string()
        } else {
            format!("{}.{key}", self.path)
        }
    }
    pub(crate) fn err<T>(&self, key: &str, why: Why) -> R<T> {
        Err(MsgError::Field {
            t: self.t,
            field: self.name(key),
            why,
        })
    }
    pub(crate) fn opt(&mut self, key: &str) -> Option<&'a Value> {
        let i = self.items.iter().position(|(k, _)| k == key)?;
        self.seen[i] = true;
        Some(&self.items[i].1)
    }
    pub(crate) fn req(&mut self, key: &str) -> R<&'a Value> {
        match self.opt(key) {
            Some(v) => Ok(v),
            None => self.err(key, Why::Missing),
        }
    }
    pub(crate) fn check_str(
        &self,
        key: &str,
        v: &Value,
        max: usize,
        form: fn(&str) -> bool,
    ) -> R<String> {
        let Value::Str(s) = v else {
            return self.err(key, Why::Type);
        };
        if s.len() > max {
            return self.err(key, Why::TooLong);
        }
        if !form(s) {
            return self.err(key, Why::Format);
        }
        Ok(s.clone())
    }
    pub(crate) fn str(&mut self, key: &str, max: usize, form: fn(&str) -> bool) -> R<String> {
        let v = self.req(key)?;
        self.check_str(key, v, max, form)
    }
    pub(crate) fn opt_str(
        &mut self,
        key: &str,
        max: usize,
        form: fn(&str) -> bool,
    ) -> R<Option<String>> {
        match self.opt(key) {
            Some(v) => self.check_str(key, v, max, form).map(Some),
            None => Ok(None),
        }
    }
    pub(crate) fn word<T>(&mut self, key: &str, from: fn(&str) -> Option<T>) -> R<T> {
        let s = self.str(key, 64, any)?;
        match from(&s) {
            Some(x) => Ok(x),
            None => self.err(key, Why::Format),
        }
    }
    pub(crate) fn hex32(&mut self, key: &str) -> R<[u8; 32]> {
        self.word(key, from_hex32)
    }
    pub(crate) fn check_num(&self, key: &str, v: &Value, min: u64, max: u64) -> R<u64> {
        if !matches!(v, Value::Num(_)) {
            return self.err(key, Why::Type);
        }
        match v.as_u64() {
            Some(n) if n >= min && n <= max => Ok(n),
            _ => self.err(key, Why::Range),
        }
    }
    pub(crate) fn num(&mut self, key: &str, max: u64) -> R<u64> {
        let v = self.req(key)?;
        self.check_num(key, v, 0, max)
    }
    pub(crate) fn num32(&mut self, key: &str) -> R<u32> {
        self.num(key, u32::MAX as u64).map(|n| n as u32)
    }
    pub(crate) fn boolean(&mut self, key: &str) -> R<bool> {
        match self.req(key)? {
            Value::Bool(b) => Ok(*b),
            _ => self.err(key, Why::Type),
        }
    }
    pub(crate) fn list(&mut self, key: &str, max: usize) -> R<&'a [Value]> {
        match self.req(key)? {
            Value::Arr(a) if a.len() <= max => Ok(a),
            Value::Arr(_) => self.err(key, Why::TooMany),
            _ => self.err(key, Why::Type),
        }
    }
    pub(crate) fn strings(
        &mut self,
        key: &str,
        max_items: usize,
        max: usize,
        form: fn(&str) -> bool,
    ) -> R<Vec<String>> {
        let list = self.list(key, max_items)?;
        list.iter()
            .map(|v| self.check_str(key, v, max, form))
            .collect()
    }
    pub(crate) fn child(&mut self, key: &'static str) -> R<Rd<'a>> {
        let v = self.req(key)?;
        Rd::new(self.t, key, key, v, self.lenient)
    }
    /// Each entry of a list of objects, read by `f`.
    pub(crate) fn objects<T>(
        &mut self,
        key: &'static str,
        max: usize,
        f: impl Fn(&mut Rd<'a>) -> R<T>,
    ) -> R<Vec<T>> {
        let list = self.list(key, max)?;
        list.iter()
            .map(|v| {
                let mut rd = Rd::new(self.t, key, key, v, self.lenient)?;
                let x = f(&mut rd)?;
                rd.end()?;
                Ok(x)
            })
            .collect()
    }
    pub(crate) fn end(self) -> R<()> {
        if self.lenient {
            return Ok(());
        }
        match self.seen.iter().position(|s| !s) {
            // The peer's key, cut short and made printable: it goes to a log
            Some(i) => {
                let key: String = self.items[i]
                    .0
                    .chars()
                    .take(32)
                    .map(|c| if c.is_ascii_graphic() { c } else { '?' })
                    .collect();
                self.err(&key, Why::Unknown)
            }
            None => Ok(()),
        }
    }
}

/// The object and its `t`, as one of `known`.
fn open<'a>(
    payload: &'a Value,
    known: &[&'static str],
) -> R<(&'static str, &'a [(String, Value)])> {
    let Value::Obj(items) = payload else {
        return Err(MsgError::NotObject);
    };
    let t = match items.iter().find(|(k, _)| k == "t") {
        Some((_, Value::Str(t))) => t,
        _ => return Err(MsgError::NoType),
    };
    match known.iter().find(|k| *k == t) {
        Some(k) => Ok((k, items)),
        None => {
            let id = items
                .iter()
                .find(|(k, _)| k == "id")
                .and_then(|(_, v)| v.as_u64())
                .filter(|n| *n >= 1);
            Err(MsgError::UnknownType { id })
        }
    }
}

// --- Host to phone ---------------------------------------------------------

/// `hello`, from the host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostHello {
    /// The protocol numbers the host speaks: 1 to 16 of them, each 1 to 65535.
    pub proto: Vec<u32>,
    pub installer_name: String,
    pub installer_version: String,
    /// 1024 to 4194304.
    pub max_data: u32,
    /// 32 random bytes, drawn anew for every `hello` sent (§5.3): the phone
    /// echoes them, and the host takes no frame for an answer until it does.
    pub nonce: [u8; 32],
}

/// `factory-begin`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FactoryBegin {
    pub size: u64,
    pub sha256: [u8; 32],
    pub build: String,
    /// Shown in a log line; never used to open anything.
    pub name: String,
}

/// What the host can say.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostMsg {
    Hello(HostHello),
    Ping,
    Pair { code: Code },
    Resume { token: Token },
    Bye,
    Status,
    Answers(Answers),
    FirstbootRetry,
    BackupList,
    BackupRead { name: String, offset: u64 },
    BackupVerify { name: String, sha256: [u8; 32] },
    BackupCommit,
    Abort,
    FactoryBegin(FactoryBegin),
    FactoryEnd,
    Provision,
    Restart,
    Smoke,
    Finish,
}

/// The host's message types, as `t` spells them.
pub const HOST_TYPES: [&str; 19] = [
    "hello",
    "ping",
    "pair",
    "resume",
    "bye",
    "status",
    "answers",
    "firstboot-retry",
    "backup-list",
    "backup-read",
    "backup-verify",
    "backup-commit",
    "abort",
    "factory-begin",
    "factory-end",
    "provision",
    "restart",
    "smoke",
    "finish",
];

impl HostMsg {
    pub fn t(&self) -> &'static str {
        match self {
            HostMsg::Hello(_) => "hello",
            HostMsg::Ping => "ping",
            HostMsg::Pair { .. } => "pair",
            HostMsg::Resume { .. } => "resume",
            HostMsg::Bye => "bye",
            HostMsg::Status => "status",
            HostMsg::Answers(_) => "answers",
            HostMsg::FirstbootRetry => "firstboot-retry",
            HostMsg::BackupList => "backup-list",
            HostMsg::BackupRead { .. } => "backup-read",
            HostMsg::BackupVerify { .. } => "backup-verify",
            HostMsg::BackupCommit => "backup-commit",
            HostMsg::Abort => "abort",
            HostMsg::FactoryBegin(_) => "factory-begin",
            HostMsg::FactoryEnd => "factory-end",
            HostMsg::Provision => "provision",
            HostMsg::Restart => "restart",
            HostMsg::Smoke => "smoke",
            HostMsg::Finish => "finish",
        }
    }
}

/// One message from the host: its number and what it says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostFrame {
    /// A whole number from 1 to 2^53 - 1; the host counts up (§5.2).
    pub id: u64,
    pub msg: HostMsg,
}

fn finish_json<T: PartialEq>(t: &'static str, json: String, me: &T, back: R<T>) -> R<String> {
    if json.len() > MAX_MESSAGE {
        return Err(MsgError::TooLong { t, len: json.len() });
    }
    match back {
        Ok(ref x) if x == me => Ok(json),
        Ok(_) => Err(MsgError::Field {
            t,
            field: String::new(),
            why: Why::Format,
        }),
        Err(e) => Err(e),
    }
}

impl HostFrame {
    /// The message as its JSON payload, or why the phone would refuse it.
    pub fn to_json(&self) -> R<String> {
        let mut w = ObjWriter::new();
        w.str("t", self.msg.t()).num("id", self.id);
        match &self.msg {
            HostMsg::Hello(h) => {
                w.raw("proto", &array(h.proto.iter().map(u32::to_string)));
                let mut i = ObjWriter::new();
                i.str("name", &h.installer_name)
                    .str("version", &h.installer_version);
                w.raw("installer", &i.end())
                    .num("max_data", h.max_data as u64)
                    .str("nonce", &to_hex(&h.nonce));
            }
            HostMsg::Pair { code } => {
                w.str("code", code.digits());
            }
            HostMsg::Resume { token } => {
                w.str("token", &to_hex(&token.0));
            }
            HostMsg::Answers(a) => {
                w.str("username", &a.username)
                    .str("password_hash", &a.password_hash);
                let mut opt = |k: &str, v: &Option<String>| {
                    if let Some(v) = v {
                        w.str(k, v);
                    }
                };
                opt("credential", &a.credential);
                opt("hostname", &a.hostname);
                opt("timezone", &a.timezone);
                opt("wifi_ssid", &a.wifi_ssid);
                opt("wifi_psk", &a.wifi_psk);
                w.raw("ssh_keys", &array(a.ssh_keys.iter().map(|k| quoted(k))));
                let mut opt = |k: &str, v: &Option<String>| {
                    if let Some(v) = v {
                        w.str(k, v);
                    }
                };
                opt("full_name", &a.full_name);
                opt("email", &a.email);
            }
            HostMsg::BackupRead { name, offset } => {
                w.str("name", name).num("offset", *offset);
            }
            HostMsg::BackupVerify { name, sha256 } => {
                w.str("name", name).str("sha256", &to_hex(sha256));
            }
            HostMsg::FactoryBegin(b) => {
                w.num("size", b.size)
                    .str("sha256", &to_hex(&b.sha256))
                    .str("build", &b.build)
                    .str("name", &b.name);
            }
            HostMsg::Ping
            | HostMsg::Bye
            | HostMsg::Status
            | HostMsg::FirstbootRetry
            | HostMsg::BackupList
            | HostMsg::BackupCommit
            | HostMsg::Abort
            | HostMsg::FactoryEnd
            | HostMsg::Provision
            | HostMsg::Restart
            | HostMsg::Smoke
            | HostMsg::Finish => {}
        }
        let json = w.end();
        let back = HostFrame::parse(json.as_bytes());
        finish_json(self.msg.t(), json, self, back)
    }

    /// A message frame's payload as a host message.
    pub fn parse(payload: &[u8]) -> R<HostFrame> {
        let v = json::parse(payload).map_err(MsgError::Json)?;
        let (t, _) = open(&v, &HOST_TYPES)?;
        let mut rd = Rd::new(t, "", "", &v, t == "hello")?;
        rd.opt("t");
        let v = rd.req("id")?;
        let id = rd.check_num("id", v, 1, MAX_NUMBER)?;
        let msg = match t {
            "hello" => {
                let list = rd.list("proto", MAX_PROTOS)?;
                let mut proto = Vec::new();
                for v in list {
                    let n = rd.check_num("proto", v, 1, 65535)? as u32;
                    if proto.contains(&n) {
                        return rd.err("proto", Why::Format);
                    }
                    proto.push(n);
                }
                if proto.is_empty() {
                    return rd.err("proto", Why::Missing);
                }
                let mut i = rd.child("installer")?;
                let installer_name = i.str("name", MAX_TEXT, is_text1)?;
                let installer_version = i.str("version", MAX_TEXT, is_text1)?;
                i.end()?;
                let v = rd.req("max_data")?;
                let max_data =
                    rd.check_num("max_data", v, MIN_MAX_DATA as u64, MAX_DATA_LIMIT as u64)? as u32;
                HostMsg::Hello(HostHello {
                    proto,
                    installer_name,
                    installer_version,
                    max_data,
                    nonce: rd.hex32("nonce")?,
                })
            }
            "ping" => HostMsg::Ping,
            "pair" => HostMsg::Pair {
                code: rd.word("code", Code::parse)?,
            },
            "resume" => HostMsg::Resume {
                token: Token(rd.hex32("token")?),
            },
            "bye" => HostMsg::Bye,
            "status" => HostMsg::Status,
            "answers" => {
                // Types only: the values are first boot's to judge, field by
                // field (Answers::validate), and the message's size bounds them.
                let mut a = Answers {
                    username: rd.str("username", MAX_MESSAGE, any)?,
                    password_hash: rd.str("password_hash", MAX_MESSAGE, any)?,
                    ..Answers::default()
                };
                a.credential = rd.opt_str("credential", MAX_MESSAGE, any)?;
                a.hostname = rd.opt_str("hostname", MAX_MESSAGE, any)?;
                a.timezone = rd.opt_str("timezone", MAX_MESSAGE, any)?;
                a.wifi_ssid = rd.opt_str("wifi_ssid", MAX_MESSAGE, any)?;
                a.wifi_psk = rd.opt_str("wifi_psk", MAX_MESSAGE, any)?;
                a.ssh_keys = rd.strings("ssh_keys", MAX_MESSAGE, MAX_MESSAGE, any)?;
                a.full_name = rd.opt_str("full_name", MAX_MESSAGE, any)?;
                a.email = rd.opt_str("email", MAX_MESSAGE, any)?;
                HostMsg::Answers(a)
            }
            "firstboot-retry" => HostMsg::FirstbootRetry,
            "backup-list" => HostMsg::BackupList,
            "backup-read" => HostMsg::BackupRead {
                name: rd.str("name", MAX_PART_NAME, is_part_name)?,
                offset: rd.num("offset", MAX_NUMBER)?,
            },
            "backup-verify" => HostMsg::BackupVerify {
                name: rd.str("name", MAX_PART_NAME, is_part_name)?,
                sha256: rd.hex32("sha256")?,
            },
            "backup-commit" => HostMsg::BackupCommit,
            "abort" => HostMsg::Abort,
            "factory-begin" => {
                let v = rd.req("size")?;
                HostMsg::FactoryBegin(FactoryBegin {
                    size: rd.check_num("size", v, 1, MAX_NUMBER)?,
                    sha256: rd.hex32("sha256")?,
                    build: rd.str("build", MAX_TEXT, is_text1)?,
                    name: rd.str("name", MAX_FILE_NAME, no_control)?,
                })
            }
            "factory-end" => HostMsg::FactoryEnd,
            "provision" => HostMsg::Provision,
            "restart" => HostMsg::Restart,
            "smoke" => HostMsg::Smoke,
            _ => HostMsg::Finish,
        };
        rd.end()?;
        Ok(HostFrame { id, msg })
    }
}

// --- Phone to host ---------------------------------------------------------

/// `hello`, the phone's reply.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PhoneHello {
    /// The highest number in both lists.
    pub proto: u32,
    /// The daemon's version.
    pub link: String,
    /// The codename.
    pub device: String,
    /// The smaller of the two sides'.
    pub max_data: u32,
    /// New per session.
    pub nonce: [u8; 32],
    /// The nonce of the host's `hello` this one answers, echoed.
    pub host_nonce: [u8; 32],
    pub stage: Stage,
    pub pair: PairMode,
    /// Seconds until `pair` may be tried. 0 when `pair` is not `locked`;
    /// with `locked`, 0 means until someone acts at the phone or it restarts.
    pub wait_s: u32,
}

/// `status.firstboot`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FirstbootStatus {
    pub state: JobState,
    /// The step's name as `omamobile-firstboot` logs it; empty when none runs.
    pub step: String,
    pub n: u32,
    pub of: u32,
    /// The names of the steps that failed.
    pub failed: Vec<String>,
}

/// One partition in `status.backup.parts`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PartStatus {
    pub name: String,
    pub size: u64,
    pub verified: bool,
}

/// One partition in `backup-parts`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackupPart {
    pub name: String,
    pub size: u64,
    /// In the descriptor's `NV_PARTS`: its checksum is kept at `backup-commit`.
    pub checked: bool,
}

/// `status`: nothing in it names a user, a host, a network, a serial or an
/// address, so it is answered unpaired too (§5.4).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Status {
    pub stage: Stage,
    /// 1 before the planned restart, 2 after.
    pub boot: u8,
    pub release: String,
    pub kernel: String,
    pub device: String,
    pub firstboot: FirstbootStatus,
    pub backup_state: BackupState,
    pub backup_parts: Vec<PartStatus>,
    pub factory_state: FactoryState,
    pub factory_have: u64,
    pub factory_size: u64,
    pub provision_state: JobState,
    pub provision_step: String,
    pub provision_n: u32,
    pub provision_of: u32,
    /// The optional steps that failed in a run that succeeded (§6.6): what
    /// an installer that lost `provision-done` still has to show.
    pub provision_optional_failed: Vec<String>,
    /// Setup boots left (§2.2); 3 while none is counted.
    pub link_boots_left: u8,
    /// Seconds until the link closes for this boot; 0 while that clock is
    /// not running.
    pub link_closes_s: u32,
    /// Percent.
    pub battery: u8,
    pub charging: bool,
    /// Bytes free on the root.
    pub free: u64,
}

/// `progress`: `total` 0 means unknown.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Progress {
    pub what: What,
    pub step: String,
    pub done: u64,
    pub total: u64,
}

/// One step in `provision-done`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StepResult {
    pub step: String,
    pub state: StepState,
}

/// `error.code`, with what each code carries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ErrorCode {
    Protocol,
    /// With the phone's protocol numbers, and the nonce of the `hello` it
    /// answers.
    Version {
        proto: Vec<u32>,
        host_nonce: [u8; 32],
    },
    Unknown,
    NotAllowed,
    NotPaired,
    /// With the tries left before the code is replaced.
    BadCode {
        left: u32,
    },
    /// With the seconds to wait; 0 is until someone acts at the phone or it
    /// restarts.
    Locked {
        wait_s: u32,
    },
    BadToken,
    Problems {
        problems: Vec<Problem>,
    },
    Busy,
    /// With the partitions that are writable, mounted, missing or doubled.
    BackupPrecondition {
        parts: Vec<String>,
    },
    BackupMismatch,
    NoSpace {
        need: u64,
        free: u64,
    },
    TooLarge,
    FactoryMismatch,
    ProvisionFailed {
        step: String,
    },
    FirstbootFailed {
        steps: Vec<String>,
    },
    Battery,
    Internal,
}

impl ErrorCode {
    pub fn as_str(&self) -> &'static str {
        match self {
            ErrorCode::Protocol => "protocol",
            ErrorCode::Version { .. } => "version",
            ErrorCode::Unknown => "unknown",
            ErrorCode::NotAllowed => "not-allowed",
            ErrorCode::NotPaired => "not-paired",
            ErrorCode::BadCode { .. } => "bad-code",
            ErrorCode::Locked { .. } => "locked",
            ErrorCode::BadToken => "bad-token",
            ErrorCode::Problems { .. } => "problems",
            ErrorCode::Busy => "busy",
            ErrorCode::BackupPrecondition { .. } => "backup-precondition",
            ErrorCode::BackupMismatch => "backup-mismatch",
            ErrorCode::NoSpace { .. } => "no-space",
            ErrorCode::TooLarge => "too-large",
            ErrorCode::FactoryMismatch => "factory-mismatch",
            ErrorCode::ProvisionFailed { .. } => "provision-failed",
            ErrorCode::FirstbootFailed { .. } => "firstboot-failed",
            ErrorCode::Battery => "battery",
            ErrorCode::Internal => "internal",
        }
    }

    /// The fixed English text for a log. The installer shows its own words,
    /// chosen by the code.
    pub fn text(&self) -> &'static str {
        match self {
            ErrorCode::Protocol => "a frame or a message was not protocol 1; the session is over",
            ErrorCode::Version { .. } => "no protocol number in common",
            ErrorCode::Unknown => "unknown message type",
            ErrorCode::NotAllowed => "not allowed now; nothing changed",
            ErrorCode::NotPaired => "pair first",
            ErrorCode::BadCode { .. } => "wrong code",
            ErrorCode::Locked { .. } => "pairing is locked",
            ErrorCode::BadToken => "wrong token",
            ErrorCode::Problems { .. } => "the answers were refused; nothing was kept",
            ErrorCode::Busy => "another step is running",
            ErrorCode::BackupPrecondition { .. } => {
                "a radio partition is not read-only, unmounted and found once"
            }
            ErrorCode::BackupMismatch => "the computer's copy differs from a second read",
            ErrorCode::NoSpace { .. } => "not enough free space",
            ErrorCode::TooLarge => "larger than a factory image may be",
            ErrorCode::FactoryMismatch => "the file is not the one announced; it was deleted",
            ErrorCode::ProvisionFailed { .. } => "provisioning stopped",
            ErrorCode::FirstbootFailed { .. } => "first-boot setup failed",
            ErrorCode::Battery => "charge the phone, or plug it in",
            ErrorCode::Internal => "the phone could not do it",
        }
    }
}

/// `error`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ErrorMsg {
    pub code: ErrorCode,
    /// Fixed English text for a log.
    pub message: String,
    pub stage: Stage,
}

/// What the phone can say.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PhoneMsg {
    Hello(PhoneHello),
    Pong,
    /// `token` after `pair`, none after `resume`.
    Paired {
        token: Option<Token>,
        serial_proof: [u8; 32],
    },
    Bye,
    Status(Box<Status>),
    AnswersOk,
    BackupParts {
        parts: Vec<BackupPart>,
    },
    DataBegin {
        name: String,
        size: u64,
        offset: u64,
    },
    DataEnd {
        name: String,
    },
    BackupVerified,
    BackupDone,
    Aborted,
    /// `offset`: where the transfer goes on. `ack_every`: the bytes between
    /// acknowledgements. `window`: the host never has bytes sent beyond the
    /// last acknowledged offset it has read plus this (§5.7).
    FactoryReady {
        offset: u64,
        ack_every: u64,
        window: u64,
    },
    Ack {
        offset: u64,
    },
    FactoryVerified,
    ProvisionStarted,
    Progress(Progress),
    ProvisionDone {
        steps: Vec<StepResult>,
    },
    Restarting,
    /// `phone-smoke --here --json`'s output, as it was written.
    SmokeReport {
        report: Value,
    },
    Finished,
    Error(ErrorMsg),
}

/// The phone's message types, as `t` spells them.
pub const PHONE_TYPES: [&str; 22] = [
    "hello",
    "pong",
    "paired",
    "bye",
    "status",
    "answers-ok",
    "backup-parts",
    "data-begin",
    "data-end",
    "backup-verified",
    "backup-done",
    "aborted",
    "factory-ready",
    "ack",
    "factory-verified",
    "provision-started",
    "progress",
    "provision-done",
    "restarting",
    "smoke-report",
    "finished",
    "error",
];

/// Does a message of this type answer one of the host's (`re`), come by
/// itself (no `re`), or either?
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Re {
    Reply,
    Event,
    Either,
}

impl PhoneMsg {
    pub fn t(&self) -> &'static str {
        match self {
            PhoneMsg::Hello(_) => "hello",
            PhoneMsg::Pong => "pong",
            PhoneMsg::Paired { .. } => "paired",
            PhoneMsg::Bye => "bye",
            PhoneMsg::Status(_) => "status",
            PhoneMsg::AnswersOk => "answers-ok",
            PhoneMsg::BackupParts { .. } => "backup-parts",
            PhoneMsg::DataBegin { .. } => "data-begin",
            PhoneMsg::DataEnd { .. } => "data-end",
            PhoneMsg::BackupVerified => "backup-verified",
            PhoneMsg::BackupDone => "backup-done",
            PhoneMsg::Aborted => "aborted",
            PhoneMsg::FactoryReady { .. } => "factory-ready",
            PhoneMsg::Ack { .. } => "ack",
            PhoneMsg::FactoryVerified => "factory-verified",
            PhoneMsg::ProvisionStarted => "provision-started",
            PhoneMsg::Progress(_) => "progress",
            PhoneMsg::ProvisionDone { .. } => "provision-done",
            PhoneMsg::Restarting => "restarting",
            PhoneMsg::SmokeReport { .. } => "smoke-report",
            PhoneMsg::Finished => "finished",
            PhoneMsg::Error(_) => "error",
        }
    }

    /// §5.2: `progress`, `ack` and the events that end a long step carry no
    /// `re`; every answer does; `error` can be either.
    pub fn re_rule(t: &str) -> Re {
        match t {
            "progress" | "ack" | "data-end" | "provision-done" => Re::Event,
            "error" => Re::Either,
            _ => Re::Reply,
        }
    }
}

/// One message from the phone: what it answers, if anything, and what it says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PhoneFrame {
    pub re: Option<u64>,
    pub msg: PhoneMsg,
}

fn part_names(rd: &mut Rd, key: &str) -> R<Vec<String>> {
    rd.strings(key, MAX_PARTS, MAX_PART_NAME, is_part_name)
}

impl PhoneFrame {
    /// The message as its JSON payload, or why the host would refuse it.
    pub fn to_json(&self) -> R<String> {
        let mut w = ObjWriter::new();
        w.str("t", self.msg.t());
        if let Some(re) = self.re {
            w.num("re", re);
        }
        match &self.msg {
            PhoneMsg::Hello(h) => {
                w.num("proto", h.proto as u64)
                    .str("link", &h.link)
                    .str("device", &h.device)
                    .num("max_data", h.max_data as u64)
                    .str("nonce", &to_hex(&h.nonce))
                    .str("host_nonce", &to_hex(&h.host_nonce))
                    .str("stage", h.stage.as_str())
                    .str("pair", h.pair.as_str())
                    .num("wait_s", h.wait_s as u64);
            }
            PhoneMsg::Paired {
                token,
                serial_proof,
            } => {
                if let Some(t) = token {
                    w.str("token", &to_hex(&t.0));
                }
                w.str("serial_proof", &to_hex(serial_proof));
            }
            PhoneMsg::Status(s) => {
                w.str("stage", s.stage.as_str())
                    .num("boot", s.boot as u64)
                    .str("release", &s.release)
                    .str("kernel", &s.kernel)
                    .str("device", &s.device);
                let mut f = ObjWriter::new();
                f.str("state", s.firstboot.state.as_str())
                    .str("step", &s.firstboot.step)
                    .num("n", s.firstboot.n as u64)
                    .num("of", s.firstboot.of as u64)
                    .raw(
                        "failed",
                        &array(s.firstboot.failed.iter().map(|x| quoted(x))),
                    );
                w.raw("firstboot", &f.end());
                let parts = array(s.backup_parts.iter().map(|p| {
                    ObjWriter::new()
                        .str("name", &p.name)
                        .num("size", p.size)
                        .bool("verified", p.verified)
                        .end()
                }));
                let mut b = ObjWriter::new();
                b.str("state", s.backup_state.as_str()).raw("parts", &parts);
                w.raw("backup", &b.end());
                let mut f = ObjWriter::new();
                f.str("state", s.factory_state.as_str())
                    .num("have", s.factory_have)
                    .num("size", s.factory_size);
                w.raw("factory", &f.end());
                let mut p = ObjWriter::new();
                p.str("state", s.provision_state.as_str())
                    .str("step", &s.provision_step)
                    .num("n", s.provision_n as u64)
                    .num("of", s.provision_of as u64)
                    .raw(
                        "optional_failed",
                        &array(s.provision_optional_failed.iter().map(|x| quoted(x))),
                    );
                let mut l = ObjWriter::new();
                l.num("boots_left", s.link_boots_left as u64)
                    .num("closes_s", s.link_closes_s as u64);
                w.raw("provision", &p.end())
                    .raw("link", &l.end())
                    .num("battery", s.battery as u64)
                    .bool("charging", s.charging)
                    .num("free", s.free);
            }
            PhoneMsg::BackupParts { parts } => {
                w.raw(
                    "parts",
                    &array(parts.iter().map(|p| {
                        ObjWriter::new()
                            .str("name", &p.name)
                            .num("size", p.size)
                            .bool("checked", p.checked)
                            .end()
                    })),
                );
            }
            PhoneMsg::DataBegin { name, size, offset } => {
                w.str("name", name)
                    .num("size", *size)
                    .num("offset", *offset);
            }
            PhoneMsg::DataEnd { name } => {
                w.str("name", name);
            }
            PhoneMsg::FactoryReady {
                offset,
                ack_every,
                window,
            } => {
                w.num("offset", *offset)
                    .num("ack_every", *ack_every)
                    .num("window", *window);
            }
            PhoneMsg::Ack { offset } => {
                w.num("offset", *offset);
            }
            PhoneMsg::Progress(p) => {
                w.str("what", p.what.as_str())
                    .str("step", &p.step)
                    .num("done", p.done)
                    .num("total", p.total);
            }
            PhoneMsg::ProvisionDone { steps } => {
                w.raw(
                    "steps",
                    &array(steps.iter().map(|s| {
                        ObjWriter::new()
                            .str("step", &s.step)
                            .str("state", s.state.as_str())
                            .end()
                    })),
                );
            }
            PhoneMsg::SmokeReport { report } => {
                w.raw("report", &report.to_json());
            }
            PhoneMsg::Error(e) => {
                w.str("code", e.code.as_str())
                    .str("message", &e.message)
                    .str("stage", e.stage.as_str());
                match &e.code {
                    ErrorCode::Version { proto, host_nonce } => {
                        w.raw("proto", &array(proto.iter().map(u32::to_string)))
                            .str("host_nonce", &to_hex(host_nonce));
                    }
                    ErrorCode::BadCode { left } => {
                        w.num("left", *left as u64);
                    }
                    ErrorCode::Locked { wait_s } => {
                        w.num("wait_s", *wait_s as u64);
                    }
                    ErrorCode::Problems { problems } => {
                        w.raw(
                            "problems",
                            &array(problems.iter().map(|p| {
                                ObjWriter::new()
                                    .str("field", p.field.as_str())
                                    .str("reason", &p.reason)
                                    .end()
                            })),
                        );
                    }
                    ErrorCode::BackupPrecondition { parts } => {
                        w.raw("parts", &array(parts.iter().map(|x| quoted(x))));
                    }
                    ErrorCode::NoSpace { need, free } => {
                        w.num("need", *need).num("free", *free);
                    }
                    ErrorCode::ProvisionFailed { step } => {
                        w.str("step", step);
                    }
                    ErrorCode::FirstbootFailed { steps } => {
                        w.raw("steps", &array(steps.iter().map(|x| quoted(x))));
                    }
                    _ => {}
                }
            }
            PhoneMsg::Pong
            | PhoneMsg::Bye
            | PhoneMsg::AnswersOk
            | PhoneMsg::BackupVerified
            | PhoneMsg::BackupDone
            | PhoneMsg::Aborted
            | PhoneMsg::FactoryVerified
            | PhoneMsg::ProvisionStarted
            | PhoneMsg::Restarting
            | PhoneMsg::Finished => {}
        }
        let json = w.end();
        let back = PhoneFrame::parse(json.as_bytes());
        finish_json(self.msg.t(), json, self, back)
    }

    /// A message frame's payload as a phone message.
    pub fn parse(payload: &[u8]) -> R<PhoneFrame> {
        let v = json::parse(payload).map_err(MsgError::Json)?;
        let (t, items) = open(&v, &PHONE_TYPES)?;
        let version_error = t == "error"
            && items
                .iter()
                .any(|(k, v)| k == "code" && *v == Value::Str("version".into()));
        let mut rd = Rd::new(t, "", "", &v, t == "hello" || version_error)?;
        rd.opt("t");
        let re = match (PhoneMsg::re_rule(t), rd.opt("re")) {
            (Re::Event, Some(_)) => return rd.err("re", Why::Unknown),
            (Re::Reply, None) => return rd.err("re", Why::Missing),
            (_, Some(v)) => Some(rd.check_num("re", v, 1, MAX_NUMBER)?),
            (_, None) => None,
        };
        let msg = match t {
            "hello" => {
                let v = rd.req("proto")?;
                let proto = rd.check_num("proto", v, 1, 65535)? as u32;
                let link = rd.str("link", MAX_TEXT, is_text1)?;
                let device = rd.str("device", 32, is_device)?;
                let v = rd.req("max_data")?;
                let max_data =
                    rd.check_num("max_data", v, MIN_MAX_DATA as u64, MAX_DATA_LIMIT as u64)? as u32;
                PhoneMsg::Hello(PhoneHello {
                    proto,
                    link,
                    device,
                    max_data,
                    nonce: rd.hex32("nonce")?,
                    host_nonce: rd.hex32("host_nonce")?,
                    stage: rd.word("stage", Stage::from_wire)?,
                    pair: rd.word("pair", PairMode::from_wire)?,
                    wait_s: rd.num32("wait_s")?,
                })
            }
            "pong" => PhoneMsg::Pong,
            "paired" => PhoneMsg::Paired {
                token: match rd.opt("token") {
                    Some(v) => {
                        let s = rd.check_str("token", v, 64, any)?;
                        match from_hex32(&s) {
                            Some(t) => Some(Token(t)),
                            None => return rd.err("token", Why::Format),
                        }
                    }
                    None => None,
                },
                serial_proof: rd.hex32("serial_proof")?,
            },
            "bye" => PhoneMsg::Bye,
            "status" => {
                let stage = rd.word("stage", Stage::from_wire)?;
                let v = rd.req("boot")?;
                let boot = rd.check_num("boot", v, 1, 2)? as u8;
                let release = rd.str("release", MAX_TEXT, is_text)?;
                let kernel = rd.str("kernel", MAX_KERNEL, is_text)?;
                let device = rd.str("device", 32, is_device)?;
                let mut f = rd.child("firstboot")?;
                let firstboot = FirstbootStatus {
                    state: f.word("state", JobState::from_wire)?,
                    step: f.str("step", MAX_TEXT, is_text)?,
                    n: f.num32("n")?,
                    of: f.num32("of")?,
                    failed: f.strings("failed", MAX_STEPS, MAX_TEXT, is_text1)?,
                };
                f.end()?;
                let mut b = rd.child("backup")?;
                let backup_state = b.word("state", BackupState::from_wire)?;
                let backup_parts = b.objects("parts", MAX_PARTS, |p| {
                    Ok(PartStatus {
                        name: p.str("name", MAX_PART_NAME, is_part_name)?,
                        size: p.num("size", MAX_NUMBER)?,
                        verified: p.boolean("verified")?,
                    })
                })?;
                b.end()?;
                let mut f = rd.child("factory")?;
                let factory_state = f.word("state", FactoryState::from_wire)?;
                let factory_have = f.num("have", MAX_NUMBER)?;
                let factory_size = f.num("size", MAX_NUMBER)?;
                f.end()?;
                let mut p = rd.child("provision")?;
                let provision_state = p.word("state", JobState::from_wire)?;
                let provision_step = p.str("step", MAX_TEXT, is_text)?;
                let provision_n = p.num32("n")?;
                let provision_of = p.num32("of")?;
                let provision_optional_failed =
                    p.strings("optional_failed", MAX_STEPS, MAX_TEXT, is_text1)?;
                p.end()?;
                let mut l = rd.child("link")?;
                let link_boots_left = l.num("boots_left", 255)? as u8;
                let link_closes_s = l.num32("closes_s")?;
                l.end()?;
                PhoneMsg::Status(Box::new(Status {
                    stage,
                    boot,
                    release,
                    kernel,
                    device,
                    firstboot,
                    backup_state,
                    backup_parts,
                    factory_state,
                    factory_have,
                    factory_size,
                    provision_state,
                    provision_step,
                    provision_n,
                    provision_of,
                    provision_optional_failed,
                    link_boots_left,
                    link_closes_s,
                    battery: rd.num("battery", 100)? as u8,
                    charging: rd.boolean("charging")?,
                    free: rd.num("free", MAX_NUMBER)?,
                }))
            }
            "answers-ok" => PhoneMsg::AnswersOk,
            "backup-parts" => PhoneMsg::BackupParts {
                parts: rd.objects("parts", MAX_PARTS, |p| {
                    Ok(BackupPart {
                        name: p.str("name", MAX_PART_NAME, is_part_name)?,
                        size: p.num("size", MAX_NUMBER)?,
                        checked: p.boolean("checked")?,
                    })
                })?,
            },
            "data-begin" => PhoneMsg::DataBegin {
                name: rd.str("name", MAX_PART_NAME, is_part_name)?,
                size: rd.num("size", MAX_NUMBER)?,
                offset: rd.num("offset", MAX_NUMBER)?,
            },
            "data-end" => PhoneMsg::DataEnd {
                name: rd.str("name", MAX_PART_NAME, is_part_name)?,
            },
            "backup-verified" => PhoneMsg::BackupVerified,
            "backup-done" => PhoneMsg::BackupDone,
            "aborted" => PhoneMsg::Aborted,
            "factory-ready" => {
                let offset = rd.num("offset", MAX_NUMBER)?;
                let v = rd.req("ack_every")?;
                let ack_every = rd.check_num("ack_every", v, 1, MAX_NUMBER)?;
                let v = rd.req("window")?;
                // A window smaller than the distance between two
                // acknowledgements could never be left
                let window = rd.check_num("window", v, ack_every, MAX_NUMBER)?;
                PhoneMsg::FactoryReady {
                    offset,
                    ack_every,
                    window,
                }
            }
            "ack" => PhoneMsg::Ack {
                offset: rd.num("offset", MAX_NUMBER)?,
            },
            "factory-verified" => PhoneMsg::FactoryVerified,
            "provision-started" => PhoneMsg::ProvisionStarted,
            "progress" => PhoneMsg::Progress(Progress {
                what: rd.word("what", What::from_wire)?,
                step: rd.str("step", MAX_TEXT, is_text)?,
                done: rd.num("done", MAX_NUMBER)?,
                total: rd.num("total", MAX_NUMBER)?,
            }),
            "provision-done" => PhoneMsg::ProvisionDone {
                steps: rd.objects("steps", MAX_STEPS, |s| {
                    Ok(StepResult {
                        step: s.str("step", MAX_TEXT, is_text1)?,
                        state: s.word("state", StepState::from_wire)?,
                    })
                })?,
            },
            "restarting" => PhoneMsg::Restarting,
            "smoke-report" => PhoneMsg::SmokeReport {
                report: rd.req("report")?.clone(),
            },
            "finished" => PhoneMsg::Finished,
            _ => {
                let code = rd.str("code", 32, any)?;
                let message = rd.str("message", MAX_ERROR_TEXT, is_text)?;
                let stage = rd.word("stage", Stage::from_wire)?;
                let code = match code.as_str() {
                    "protocol" => ErrorCode::Protocol,
                    "version" => {
                        let list = rd.list("proto", MAX_PROTOS)?;
                        let mut proto = Vec::new();
                        for v in list {
                            proto.push(rd.check_num("proto", v, 1, 65535)? as u32);
                        }
                        ErrorCode::Version {
                            proto,
                            host_nonce: rd.hex32("host_nonce")?,
                        }
                    }
                    "unknown" => ErrorCode::Unknown,
                    "not-allowed" => ErrorCode::NotAllowed,
                    "not-paired" => ErrorCode::NotPaired,
                    "bad-code" => ErrorCode::BadCode {
                        left: rd.num("left", 20)? as u32,
                    },
                    "locked" => ErrorCode::Locked {
                        wait_s: rd.num32("wait_s")?,
                    },
                    "bad-token" => ErrorCode::BadToken,
                    "problems" => ErrorCode::Problems {
                        problems: rd.objects("problems", 32, |p| {
                            Ok(Problem {
                                field: p.word("field", Field::from_wire)?,
                                reason: p.str("reason", 32, is_reason)?,
                            })
                        })?,
                    },
                    "busy" => ErrorCode::Busy,
                    "backup-precondition" => ErrorCode::BackupPrecondition {
                        parts: part_names(&mut rd, "parts")?,
                    },
                    "backup-mismatch" => ErrorCode::BackupMismatch,
                    "no-space" => ErrorCode::NoSpace {
                        need: rd.num("need", MAX_NUMBER)?,
                        free: rd.num("free", MAX_NUMBER)?,
                    },
                    "too-large" => ErrorCode::TooLarge,
                    "factory-mismatch" => ErrorCode::FactoryMismatch,
                    "provision-failed" => ErrorCode::ProvisionFailed {
                        step: rd.str("step", MAX_TEXT, is_text)?,
                    },
                    "firstboot-failed" => ErrorCode::FirstbootFailed {
                        steps: rd.strings("steps", MAX_STEPS, MAX_TEXT, is_text1)?,
                    },
                    "battery" => ErrorCode::Battery,
                    "internal" => ErrorCode::Internal,
                    _ => return rd.err("code", Why::Format),
                };
                PhoneMsg::Error(ErrorMsg {
                    code,
                    message,
                    stage,
                })
            }
        };
        rd.end()?;
        Ok(PhoneFrame { re, msg })
    }
}

impl ErrorMsg {
    /// The error with its fixed text.
    pub fn new(code: ErrorCode, stage: Stage) -> ErrorMsg {
        ErrorMsg {
            message: code.text().to_string(),
            code,
            stage,
        }
    }
}

/// A line for a log about a payload: the type and the length, nothing of the
/// content (§9). `None` when the payload has no readable type.
pub fn log_type(payload: &[u8]) -> Option<String> {
    let v = json::parse(payload).ok()?;
    let Value::Obj(items) = &v else { return None };
    let (_, Value::Str(t)) = items.iter().find(|(k, _)| k == "t")? else {
        return None;
    };
    HOST_TYPES
        .iter()
        .chain(PHONE_TYPES.iter())
        .find(|k| *k == t)
        .map(|t| format!("{t} ({} bytes)", payload.len()))
}
