// SPDX-License-Identifier: MIT
// A pretend phone in fastboot mode behind a pretend WebUSB `USBDevice`, for
// the write preview's tests. Test code only: no build made for a real phone
// carries it.
//
// The phone is the repository's pretend phone (core/tests/support/fake.rs)
// in JavaScript: it speaks the fastboot protocol against partitions held in
// memory, applies what is flashed (sparse images included, parsed here on
// their own), and has the same switches for the ways a real one can refuse,
// plus the ones only a USB device has: a transfer that takes fewer bytes, a
// device that leaves the bus in the middle of a transfer, and an answer that
// never comes while the device stays on the bus.
//
// The device records every call made on it, so a test can say what the page
// did at the USB level and what it never did.

export const SENTINEL = 0xa5;
// Placeholders, never real serial numbers
export const SERIAL = "FAKESERIAL0001";
export const OTHER_SERIAL = "ANOTHERPHONE0002";
export const INTERFACE = 2;
export const EP_OUT = 1;
export const EP_IN = 1;
export const PACKET = 512;

const SLOTTED = [
  ["boot", 16 << 10],
  ["init_boot", 8 << 10],
  ["vendor_boot", 16 << 10],
  ["vendor_kernel_boot", 32 << 10],
  ["dtbo", 8 << 10],
  ["vbmeta", 4 << 10],
  ["bootloader", 8 << 10],
  ["modem", 8 << 10],
];
const PLAIN = [
  ["userdata", 8 << 20],
  ["persist", 8 << 10],
  ["efs", 8 << 10],
  ["efs_backup", 8 << 10],
  ["devinfo", 4 << 10],
  ["misc", 4 << 10],
];
const UNLOCK_QUESTION = "flashing get_unlock_ability";

const text = new TextEncoder();
const le32 = (b, at = 0) => (b[at] | (b[at + 1] << 8) | (b[at + 2] << 16) | (b[at + 3] << 24)) >>> 0;
const le16 = (b, at = 0) => b[at] | (b[at + 1] << 8);

// Apply an Android sparse image to `target`, as a bootloader does. Returns
// why it could not, or nothing.
export function unsparse(image, target) {
  if (image.length < 28 || le32(image) !== 0xed26ff3a) return "not a sparse image";
  const [major, fileHdr, chunkHdr] = [le16(image, 4), le16(image, 8), le16(image, 10)];
  const [blk, totalBlks, chunks] = [le32(image, 12), le32(image, 16), le32(image, 20)];
  if (major !== 1 || fileHdr !== 28 || chunkHdr !== 12 || blk === 0 || blk % 4 !== 0) return "unsupported sparse header";
  if (totalBlks * blk > target.length) return "sparse image larger than the partition";
  let at = fileHdr;
  let block = 0;
  for (let c = 0; c < chunks; c++) {
    if (image.length < at + 12) return "truncated chunk header";
    const [kind, n, totalSz] = [le16(image, at), le32(image, at + 4), le32(image, at + 8)];
    if (totalSz < 12 || image.length < at + totalSz) return "truncated chunk";
    const data = image.subarray(at + 12, at + totalSz);
    if (block + n > totalBlks) return "chunks run past total_blks";
    const out = target.subarray(block * blk, (block + n) * blk);
    if (kind === 0xcac1 && data.length === n * blk) out.set(data);
    else if (kind === 0xcac2 && data.length === 4) for (let i = 0; i < out.length; i += 4) out.set(data, i);
    else if (kind === 0xcac3 && data.length === 0) {
      // DONT_CARE: skipped
    } else return `bad chunk 0x${kind.toString(16)}`;
    block += n;
    at += totalSz;
  }
  if (block !== totalBlks || at !== image.length) return "chunks don't add up to total_blks";
  return undefined;
}

// The name and the words Chromium gives a transfer when its device is gone
// (third_party/blink/renderer/modules/webusb/usb_device.cc)
const gone = () => new DOMException("The device was disconnected.", "NotFoundError");

export class FakePhone {
  // An unlocked phone of this product on slot a, reporting both bootloader
  // slots, as the flow needs ("ready" in the Rust pretend phone).
  constructor(product = "cheetah") {
    this.vars = new Map([
      ["product", product],
      ["serialno", SERIAL],
      ["unlocked", "yes"],
      ["current-slot", "a"],
      ["version-bootloader", "fake-1.0"],
      ["version-bootloader-a", "fake-1.0"],
      ["version-bootloader-b", "fake-1.0"],
      ["version-baseband", "fake-radio-1.0"],
      ["slot-count", "2"],
    ]);
    this.partitions = new Map();
    for (const [name, size] of SLOTTED) {
      this.partitions.set(`${name}_a`, new Uint8Array(size).fill(SENTINEL));
      this.partitions.set(`${name}_b`, new Uint8Array(size).fill(SENTINEL));
    }
    for (const [name, size] of PLAIN) this.partitions.set(name, new Uint8Array(size).fill(SENTINEL));
    this.log = []; // every command received, in order
    this.activeSlot = "a";
    this.rebooted = false;
    this.maxDownload = 256 << 10;
    this.unlockAbility = null; // the INFO line that answers the unlock question
    // Faults
    this.failOn = []; // answer FAIL to a command starting with any of these
    this.shortData = false; // answer download: with a smaller DATA size
    this.takeAtMost = null; // take at most this many bytes of each data transfer
    this.swapSerialAfter = null; // after this many commands, report another serial number
    this.unplugAt = null; // the cable is pulled once this many commands were received
    this.loseAnswerAt = null; // command N is carried out; the cable is pulled before its answer
    this.silentAt = null; // command N is carried out; its answer never comes, and the device stays
    this.vanishInData = null; // the cable is pulled once this many bytes of a download arrived
    this.unplugged = false;
    // Protocol state
    this.staged = null;
    this.expecting = 0;
    this.incoming = [];
    this.incomingLength = 0;
    this.replies = [];
    // The USB side
    this.usb = new FakeUsbDevice(this);
  }

  // The owner plugs the cable back in, with the phone still in Fastboot Mode
  replug() {
    this.unplugAt = this.loseAnswerAt = this.silentAt = this.vanishInData = null;
    this.unplugged = false;
    this.staged = null;
    this.expecting = 0;
    this.incoming = [];
    this.incomingLength = 0;
    this.replies = [];
    this.usb = new FakeUsbDevice(this);
    return this;
  }

  // What was received that is neither getvar: nor the one read-only question
  notReads() {
    return this.log.filter((c) => !c.startsWith("getvar:") && c !== UNLOCK_QUESTION);
  }
  // Names of the partitions that no longer hold only the sentinel
  written() {
    return [...this.partitions].filter(([, d]) => d.some((b) => b !== SENTINEL)).map(([n]) => n);
  }

  reply(kind, words = "") {
    this.replies.push(text.encode(kind + words));
  }

  getvar(name) {
    if (name === "serialno" && this.swapSerialAfter !== null && this.log.length > this.swapSerialAfter) return OTHER_SERIAL;
    if (name === "max-download-size") return `0x${this.maxDownload.toString(16)}`;
    if (name.startsWith("partition-size:")) {
      const p = this.partitions.get(name.slice("partition-size:".length));
      return p && `0x${p.length.toString(16)}`;
    }
    return this.vars.get(name);
  }

  command(cmd) {
    this.log.push(cmd);
    if (this.failOn.some((p) => cmd.startsWith(p))) return this.reply("FAIL", "injected failure");
    if (cmd === "getvar:all") {
      for (const [k, v] of this.vars) this.reply("INFO", `${k}:${v}`);
      this.reply("OKAY");
    } else if (cmd.startsWith("getvar:")) {
      const v = this.getvar(cmd.slice(7));
      if (v === undefined) this.reply("FAIL", "unknown variable");
      else this.reply("OKAY", v);
    } else if (cmd.startsWith("download:")) {
      const hex = cmd.slice(9);
      const n = /^[0-9a-fA-F]{8}$/.test(hex) ? parseInt(hex, 16) : NaN;
      if (Number.isNaN(n)) this.reply("FAIL", "bad download size");
      else if (n > this.maxDownload) this.reply("FAIL", "data too large");
      else {
        const take = this.shortData ? n >> 1 : n;
        this.expecting = take;
        this.incoming = [];
        this.incomingLength = 0;
        this.staged = null;
        this.reply("DATA", take.toString(16).padStart(8, "0"));
      }
    } else if (cmd.startsWith("flash:")) {
      const data = this.staged;
      this.staged = null;
      if (!data) return this.reply("FAIL", "nothing downloaded");
      if (this.vars.get("unlocked") !== "yes") return this.reply("FAIL", "device is locked");
      const target = this.partitions.get(cmd.slice(6));
      if (!target) return this.reply("FAIL", "partition does not exist");
      let why;
      if (data.length >= 4 && le32(data) === 0xed26ff3a) why = unsparse(data, target);
      else if (data.length <= target.length) target.set(data);
      else why = "image larger than the partition";
      if (why) this.reply("FAIL", why);
      else {
        this.reply("INFO", "writing");
        this.reply("OKAY");
      }
    } else if (cmd.startsWith("set_active:")) {
      this.activeSlot = cmd.slice(11);
      this.vars.set("current-slot", this.activeSlot);
      this.reply("OKAY");
    } else if (cmd === "reboot") {
      this.rebooted = true;
      this.reply("OKAY");
    } else if (cmd === UNLOCK_QUESTION && this.unlockAbility !== null) {
      this.reply("INFO", this.unlockAbility);
      this.reply("OKAY");
    } else {
      // erase:, format:, oem, flashing unlock, ...: a test that sees one fails
      this.reply("FAIL", "unknown command");
    }
  }

  // Bytes on the OUT endpoint. Returns how many were taken, or throws when
  // the phone is no longer on the bus.
  out(data) {
    if (this.rebooted || this.unplugged) throw gone();
    if (this.expecting === 0 && this.unplugAt !== null && this.log.length >= this.unplugAt) {
      this.unplugged = true;
      throw gone();
    }
    if (this.expecting > 0) {
      if (this.vanishInData !== null && this.incomingLength + data.length > this.vanishInData) {
        this.unplugged = true;
        throw gone();
      }
      const take = Math.min(data.length, this.takeAtMost ?? Infinity, this.expecting - this.incomingLength);
      this.incoming.push(data.slice(0, take));
      this.incomingLength += take;
      if (this.incomingLength === this.expecting) {
        const whole = new Uint8Array(this.expecting);
        let at = 0;
        for (const part of this.incoming) {
          whole.set(part, at);
          at += part.length;
        }
        this.expecting = 0;
        this.incoming = [];
        this.incomingLength = 0;
        this.staged = whole;
        this.reply("OKAY");
      }
      return take;
    }
    const index = this.log.length;
    let cmd = null;
    if (data.length <= 64) {
      try {
        cmd = new TextDecoder("utf-8", { fatal: true }).decode(data);
      } catch {
        cmd = null;
      }
    }
    if (cmd === null) this.reply("FAIL", "not a command");
    else this.command(cmd);
    if (this.loseAnswerAt === index) {
      this.unplugged = true;
      this.expecting = 0;
      this.replies = [];
    }
    if (this.silentAt === index) {
      // Carried out, and nothing is ever said about it
      this.expecting = 0;
      this.replies = [];
    }
    return data.length;
  }
}

// What a page can reach of a USB device through WebUSB, as far as this
// installer uses it. Every call is recorded in `calls`.
class FakeUsbDevice {
  constructor(phone) {
    this.phone = phone;
    this.calls = [];
    this.opened = false;
    this.claimed = false;
    this.pendingIn = []; // IN requests nothing has answered yet
    this.failNext = null; // "stall" or "babble": how the next OUT transfer ends
    this.claimRefused = false; // another program holds the interface
    this.hasConfiguration = true;
    // What the browser reports from the device's descriptors. A test can
    // make it differ from what the phone answers to getvar:serialno
    this.serialNumber = phone.vars.get("serialno");
    this.productName = "pretend phone";
    this.vendorId = 0x18d1;
    this.productId = 0x4ee0;
    const device = this;
    const alternate = {
      alternateSetting: 0,
      interfaceClass: 0xff,
      interfaceSubclass: 0x42,
      interfaceProtocol: 0x03,
      interfaceName: "fastboot",
      endpoints: [
        { endpointNumber: EP_IN, direction: "in", type: "bulk", packetSize: PACKET },
        { endpointNumber: EP_OUT, direction: "out", type: "bulk", packetSize: PACKET },
      ],
    };
    // Another interface before it, which is not fastboot's
    const other = { alternateSetting: 0, interfaceClass: 0xff, interfaceSubclass: 0x42, interfaceProtocol: 0x01, interfaceName: "not fastboot", endpoints: alternate.endpoints };
    this.layout = {
      configurationValue: 1,
      configurationName: null,
      interfaces: [
        { interfaceNumber: 0, alternate: other, alternates: [other], claimed: false },
        {
          interfaceNumber: INTERFACE,
          alternate,
          alternates: [alternate],
          get claimed() {
            return device.claimed;
          },
        },
      ],
    };
  }

  get configuration() {
    return this.hasConfiguration ? this.layout : null;
  }
  get configurations() {
    return [this.layout];
  }

  note(method, detail = {}) {
    this.calls.push({ method, ...detail });
  }
  // The calls of one kind, or the names of all that were made
  made(method) {
    return method ? this.calls.filter((c) => c.method === method) : [...new Set(this.calls.map((c) => c.method))];
  }

  async open() {
    this.note("open");
    if (this.phone.unplugged || this.phone.rebooted) throw gone();
    this.opened = true;
  }
  async close() {
    this.note("close");
    this.opened = false;
    this.claimed = false;
    // The browser ends the transfers that were pending
    for (const { reject } of this.pendingIn.splice(0)) reject(new DOMException("The transfer was cancelled.", "AbortError"));
  }
  async selectConfiguration(value) {
    this.note("selectConfiguration", { value });
    this.hasConfiguration = true;
  }
  async claimInterface(number) {
    this.note("claimInterface", { number });
    if (this.claimRefused) throw new DOMException("Unable to claim interface.", "NetworkError");
    if (!this.opened) throw new DOMException("The device must be opened first.", "InvalidStateError");
    if (number !== INTERFACE) throw new DOMException("The interface number provided is not supported by the device in its current configuration.", "NotFoundError");
    this.claimed = true;
  }
  async releaseInterface(number) {
    this.note("releaseInterface", { number });
    this.claimed = false;
  }

  ready(endpoint, wanted) {
    if (!this.opened) throw new DOMException("The device must be opened first.", "InvalidStateError");
    if (!this.claimed) throw new DOMException("The specified endpoint is not part of a claimed and selected alternate interface.", "NotFoundError");
    if (endpoint !== wanted) throw new DOMException("The specified endpoint is not part of a claimed and selected alternate interface.", "NotFoundError");
  }

  async transferOut(endpoint, data) {
    const bytes = data instanceof Uint8Array ? data.slice() : new Uint8Array(data.buffer ?? data).slice();
    const during = this.phone.expecting > 0 ? "data" : "command";
    // A command is kept as it was sent, for the tests to read
    this.note("transferOut", { endpoint, length: bytes.length, during, ...(during === "command" ? { command: new TextDecoder().decode(bytes) } : {}) });
    this.ready(endpoint, EP_OUT);
    if (this.failNext) {
      // One transfer the browser reports as ended by the device
      const status = this.failNext;
      this.failNext = null;
      return { status, bytesWritten: 0 };
    }
    const taken = this.phone.out(bytes);
    return { status: "ok", bytesWritten: taken };
  }

  async transferIn(endpoint, length) {
    this.note("transferIn", { endpoint, length });
    this.ready(endpoint, EP_IN);
    if (this.phone.unplugged) throw gone();
    const reply = this.phone.replies.shift();
    if (!reply) {
      // Nothing to say: the request stays pending, as on a real bus, until
      // the device is closed
      return new Promise((resolve, reject) => this.pendingIn.push({ resolve, reject }));
    }
    if (reply.length > length) return { status: "babble", data: new DataView(new ArrayBuffer(0)) };
    // Inside a larger buffer, as a browser may hand it over
    const buffer = new ArrayBuffer(reply.length + 16);
    new Uint8Array(buffer, 8, reply.length).set(reply);
    return { status: "ok", data: new DataView(buffer, 8, reply.length) };
  }

  // What this installer must never call: recorded, and refused
  refuse(method) {
    this.note(method);
    return Promise.reject(new DOMException(`${method} is not something this installer does`, "SecurityError"));
  }
  controlTransferIn() {
    return this.refuse("controlTransferIn");
  }
  controlTransferOut() {
    return this.refuse("controlTransferOut");
  }
  clearHalt() {
    return this.refuse("clearHalt");
  }
  reset() {
    return this.refuse("reset");
  }
  selectAlternateInterface() {
    return this.refuse("selectAlternateInterface");
  }
  isochronousTransferIn() {
    return this.refuse("isochronousTransferIn");
  }
  isochronousTransferOut() {
    return this.refuse("isochronousTransferOut");
  }
  forget() {
    return this.refuse("forget");
  }
}

// `navigator.usb`, with a chooser in which the pretend owner picks `phone`
// (or closes it, when there is none). Every request is recorded.
export class FakeUsb {
  constructor() {
    this.phone = null;
    this.requests = [];
    this.listed = 0;
  }
  async requestDevice(options) {
    // As a browser would see it: plain data
    this.requests.push(JSON.parse(JSON.stringify(options ?? null)));
    if (!this.phone) throw new DOMException("No device selected.", "NotFoundError");
    return this.phone.usb;
  }
  async getDevices() {
    this.listed += 1;
    return [];
  }
}
