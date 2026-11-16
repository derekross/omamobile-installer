// SPDX-License-Identifier: MIT
//! A host written out by hand, for tests that drive the phone's machine
//! frame by frame with no installer machine in between.
use super::rig::{factory_begin, image, parts, MAX_DATA, SERIAL};
use omamobile_link::digest::{serial_proof, sha256};
use omamobile_link::frame::{self, Kind};
use omamobile_link::msg::*;
use omamobile_link::phone::PhoneInput;
use omamobile_link::sim::SimPhone;

/// A host written out by hand: it numbers its frames and messages and reads
/// what comes back.
pub struct Client {
    pub seq: u32,
    pub id: u64,
    pub rx: u32,
    /// The nonce of this client's last `hello`, and the phone's in answer.
    pub host_nonce: [u8; 32],
    pub nonce: [u8; 32],
    pub token: Option<Token>,
    pub now: u64,
    hellos: u8,
}

#[derive(Debug, PartialEq)]
pub enum Got {
    Msg(PhoneFrame),
    Data { offset: u64, len: usize },
}

impl Got {
    /// The message's type, or `error:<code>`.
    pub fn name(&self) -> String {
        match self {
            Got::Msg(PhoneFrame {
                msg: PhoneMsg::Error(e),
                ..
            }) => format!("error:{}", e.code.as_str()),
            Got::Msg(f) => f.msg.t().to_string(),
            Got::Data { .. } => "data".to_string(),
        }
    }
}

impl Client {
    pub fn new() -> Client {
        Client {
            seq: 0,
            id: 0,
            rx: 0,
            host_nonce: [0; 32],
            nonce: [0; 32],
            token: None,
            now: 0,
            hellos: 0,
        }
    }

    pub fn read(&mut self, frames: Vec<Vec<u8>>) -> Vec<Got> {
        frames
            .iter()
            .map(|b| {
                let f = frame::decode(b, MAX_DATA).expect("the phone sends valid frames");
                assert_eq!(f.seq, self.rx, "the phone's frames are numbered one by one");
                self.rx += 1;
                match f.kind {
                    Kind::Message => Got::Msg(
                        PhoneFrame::parse(f.payload).expect("the phone sends valid messages"),
                    ),
                    Kind::Data => Got::Data {
                        offset: f.offset,
                        len: f.payload.len(),
                    },
                }
            })
            .collect()
    }

    pub fn raw(&mut self, p: &mut SimPhone, bytes: &[u8]) -> Vec<Got> {
        let frames = p.feed(self.now, PhoneInput::Frame(bytes));
        self.read(frames)
    }

    pub fn payload(&mut self, p: &mut SimPhone, json: &str) -> Vec<Got> {
        let bytes = frame::encode(Kind::Message, self.seq, 0, json.as_bytes(), MAX_DATA).unwrap();
        self.seq += 1;
        self.raw(p, &bytes)
    }

    pub fn send(&mut self, p: &mut SimPhone, msg: HostMsg) -> Vec<Got> {
        self.id += 1;
        let id = self.id;
        let json = HostFrame { id, msg }.to_json().unwrap();
        let got = self.payload(p, &json);
        // An answer names the message it answers
        for g in &got {
            if let Got::Msg(PhoneFrame { re: Some(re), .. }) = g {
                assert_eq!(*re, id);
            }
        }
        got
    }

    pub fn data(&mut self, p: &mut SimPhone, offset: u64, bytes: &[u8]) -> Vec<Got> {
        let f = frame::encode(Kind::Data, self.seq, offset, bytes, MAX_DATA).unwrap();
        self.seq += 1;
        self.raw(p, &f)
    }

    /// A `hello` as this client says it: frame 0, a nonce it has not used.
    pub fn hello_msg(&mut self) -> HostMsg {
        self.hellos = self.hellos.wrapping_add(1);
        self.host_nonce = [self.hellos; 32];
        HostMsg::Hello(HostHello {
            proto: vec![1],
            installer_name: "test".into(),
            installer_version: "1".into(),
            max_data: 4096,
            nonce: self.host_nonce,
        })
    }

    pub fn hello(&mut self, p: &mut SimPhone) -> PhoneHello {
        self.seq = 0;
        self.rx = 0;
        let h = self.hello_msg();
        match self.send(p, h).pop() {
            Some(Got::Msg(PhoneFrame {
                msg: PhoneMsg::Hello(h),
                ..
            })) => {
                assert_eq!(h.host_nonce, self.host_nonce, "the phone echoes the nonce");
                self.nonce = h.nonce;
                h
            }
            other => panic!("no hello: {other:?}"),
        }
    }

    /// The serial proof this session's `paired` must carry.
    pub fn proof(&self) -> [u8; 32] {
        serial_proof(&self.host_nonce, &self.nonce, SERIAL)
    }

    pub fn pair(&mut self, p: &mut SimPhone) {
        let code = p.phone.code().expect("a code on the screen");
        match self.send(p, HostMsg::Pair { code }).pop() {
            Some(Got::Msg(PhoneFrame {
                msg:
                    PhoneMsg::Paired {
                        token: Some(t),
                        serial_proof: proof,
                    },
                ..
            })) => {
                assert_eq!(proof, self.proof());
                self.token = Some(t);
            }
            other => panic!("not paired: {other:?}"),
        }
    }

    /// `resume` with the token this client holds; the reply's name.
    pub fn resume(&mut self, p: &mut SimPhone) -> String {
        let token = self.token.expect("a token");
        match self.send(p, HostMsg::Resume { token }).pop() {
            Some(Got::Msg(PhoneFrame {
                msg:
                    PhoneMsg::Paired {
                        token: None,
                        serial_proof: proof,
                    },
                ..
            })) => {
                assert_eq!(proof, self.proof());
                "paired".to_string()
            }
            Some(other) => other.name(),
            None => "nothing".to_string(),
        }
    }

    pub fn one(&mut self, p: &mut SimPhone, msg: HostMsg) -> String {
        let got = self.send(p, msg);
        got.first().map_or("nothing".to_string(), Got::name)
    }
}

pub fn begin(image: &[u8]) -> HostMsg {
    HostMsg::FactoryBegin(factory_begin(image))
}

pub fn backup_all(p: &mut SimPhone, c: &mut Client) {
    assert_eq!(c.one(p, HostMsg::BackupList), "backup-parts");
    for (found, bytes) in parts() {
        let got = c.send(
            p,
            HostMsg::BackupRead {
                name: found.name.clone(),
                offset: 0,
            },
        );
        assert_eq!(got.first().unwrap().name(), "data-begin");
        assert_eq!(got.last().unwrap().name(), "data-end");
        let read: usize = got
            .iter()
            .map(|g| {
                if let Got::Data { len, .. } = g {
                    *len
                } else {
                    0
                }
            })
            .sum();
        assert_eq!(read, bytes.len());
        assert_eq!(
            c.one(
                p,
                HostMsg::BackupVerify {
                    name: found.name,
                    sha256: sha256(&bytes)
                }
            ),
            "backup-verified"
        );
    }
}

pub fn send_image(p: &mut SimPhone, c: &mut Client) {
    let img = image();
    assert_eq!(c.one(p, begin(&img)), "factory-ready");
    for (i, chunk) in img.chunks(MAX_DATA as usize).enumerate() {
        c.data(p, i as u64 * MAX_DATA as u64, chunk);
    }
}
