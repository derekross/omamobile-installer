# Stand-ins for the steps after the write

Scripts that take the place of Omarchy Mobile's tools and of this
computer's programs in the tests of the steps after a write (docs/DESIGN.md
§13). None of them reaches a phone, a network or this computer's settings:
they read and write files in the directory `$OMAINSTALL_PRETEND_PHONE`
names, which the test makes, and fail when it is not set.

- `tools/`: the release bundle's tools, as `tools/make-bundle` ships them
  (`phone-ssh`, `nv-backup`, `phone-provision`), and `phone-smoke`. They are
  signed into `fixtures/after/` with the test key
  (`cargo test -p omamobile-installer-core --test fastboot_install -- --ignored`).
- `bin/`: `nmcli` and `ssh-keygen` (a new made-up key each time, random
  characters in an Ed25519 key's form), handed to a pretend run with
  `--fake-after DIR` (a `fake-phone` build only).

The pretend phone's directory holds:

| File | Meaning |
| --- | --- |
| `log` | every call, one line each |
| `id` | what the phone prints for its serial number's digest |
| `boot_id` | the current boot's id |
| `up` | the phone answers over SSH |
| `up_after` | answers once this many more calls have been made (the first start) |
| `firstboot_done` | the first start's marker exists |
| `refuse_key` | SSH refuses this computer's key |
| `userdata.img`, `fs` | what the pretend run wrote to `userdata`, and where its file system keeps each placeholder's file (`PATH OFFSET LENGTH`); written by the pretend run when its write is done, which also clears `firstboot_done`, `authorized_keys`, `account` and `firstboot_ran`, as the write erases them. With them the phone's first start is the stand-in's: it reads `firstboot.conf` from that block |
| `firstboot_steps` | how many more answers the first start runs before root has its key (default 2): until then the key is refused |
| `authorized_keys` | root's keys, the `SSH_KEY` lines of the answers; with `userdata.img`, only a key in it is taken |
| `account`, `firstboot_ran` | the user the answers named; the first start has run |
| `firstboot_hangs` | the first start stops after it gave root the key: no done marker |
| `system` | what `systemctl is-system-running` says (default `running`) |
| `fail_backup`, `fail_provision` | the tool fails, with this file's text |
| `smoke.json`, `smoke_rc` | phone-smoke's report and exit status |
| `nm_connections` | NetworkManager's saved connections, one name a line |
| `nm_refuse` | `nmcli connection add` is refused (no permission) |
