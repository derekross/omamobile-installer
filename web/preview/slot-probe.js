// SPDX-License-Identifier: MIT
// The write preview: the core's measurement of the phone's other bootloader
// slot (docs/DESIGN.md §6), with the restarts it asks for watched in the
// browser. Not in the page as it ships (docs/WEB-WRITE.md).
//
// Like write-walk.js it has no document in it, so the tests run it as it
// is, in node, against a pretend phone behind a pretend WebUSB device.
//
// What it decides: nothing about the phone. The wasm module runs the core's
// measurement, which sends every command and judges every answer. This file
// watches the bus between two stretches of it: WebUSB's `disconnect` and
// `connect` events for the device with the serial number the browser gave
// when the person picked it (a browser fires them only for a device the
// page was allowed to use), within the core's limits, and says what it saw.
// "Back" is only ever a `connect` seen after a `disconnect`: a phone that
// never left the bus has not restarted, whatever it would answer.

const SAID = {
  "left-and-back": "The phone left the bus and came back in Fastboot Mode.",
  "never-left": "The phone did not leave the bus.",
  "not-back": "The phone left the bus and did not come back in time.",
};

// The events for one phone, from the moment the watch is armed.
export class RestartWatch {
  constructor(usb, serial) {
    this.usb = usb;
    this.serial = serial;
    this.events = [];
    this.wake = null;
    const mine = (e) => e.device && e.device.serialNumber === serial;
    this.onDisconnect = (e) => {
      if (mine(e)) this.events.push({ kind: "gone" });
      this.wake?.();
    };
    this.onConnect = (e) => {
      if (mine(e)) this.events.push({ kind: "back", device: e.device });
      this.wake?.();
    };
    usb.addEventListener("disconnect", this.onDisconnect);
    usb.addEventListener("connect", this.onConnect);
  }
  arm() {
    this.events = [];
  }
  stop() {
    this.usb.removeEventListener("disconnect", this.onDisconnect);
    this.usb.removeEventListener("connect", this.onConnect);
  }
  // A `connect` after the first `disconnect`, if there was one
  back() {
    const gone = this.events.findIndex((e) => e.kind === "gone");
    return gone < 0 ? null : this.events.slice(gone + 1).find((e) => e.kind === "back") ?? null;
  }
  until(seen, ms) {
    return new Promise((resolve) => {
      if (seen()) return resolve(true);
      const timer = setTimeout(() => {
        this.wake = null;
        resolve(false);
      }, ms);
      this.wake = () => {
        if (!seen()) return;
        clearTimeout(timer);
        this.wake = null;
        resolve(true);
      };
    });
  }
  // What was seen of a restart, within the core's limits (seconds;
  // `second` is how many milliseconds make one, for the tests)
  async seen(leaveWithin, backWithin, second) {
    const left = await this.until(() => this.events.some((e) => e.kind === "gone"), leaveWithin * second);
    if (!left) return { seen: "never-left" };
    const came = await this.until(() => this.back() !== null, backWithin * second);
    return came ? { seen: "left-and-back", device: this.back().device } : { seen: "not-back" };
  }
}

// Run the measurement (or the putting back of slot A) the flow gave, after
// `flow.slot_probe_answer` said one is under way. `phone` is the one the
// person picked. `say` gets each line for the person, `keep` the record to
// keep for the next run after every step (undefined: delete it). Returns
// the phone as the browser has it now: the one that came back, or the same
// one, or none.
export async function runProbe(flow, core, usb, phone, { say = () => {}, keep = () => {}, second = 1000 } = {}) {
  const watch = new RestartWatch(usb, phone.serial_number());
  let current = phone;
  try {
    for (;;) {
      watch.arm();
      const next = JSON.parse(await flow.slot_probe_step(current ? current.again() : undefined));
      for (const line of next.lines) say(line);
      keep(flow.slot_probe_kept());
      if (next.next === "finished") break;
      say(next.note);
      const { seen, device } = await watch.seen(next.leave_within, next.back_within, second);
      say(SAID[seen]);
      flow.slot_probe_restart_seen(seen);
      keep(flow.slot_probe_kept());
      if (device) current = core.phone_of_device(device);
      else if (seen === "not-back") current = null;
    }
  } finally {
    watch.stop();
  }
  flow.slot_probe_done();
  keep(flow.slot_probe_record());
  return current;
}
