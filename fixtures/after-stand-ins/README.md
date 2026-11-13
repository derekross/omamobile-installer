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
- `bin/`: `nmcli` and `ssh-keygen`, handed to a pretend run with
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
| `system` | what `systemctl is-system-running` says (default `running`) |
| `fail_backup`, `fail_provision` | the tool fails, with this file's text |
| `smoke.json`, `smoke_rc` | phone-smoke's report and exit status |
| `nm_connections` | NetworkManager's saved connections, one name a line |
| `nm_refuse` | `nmcli connection add` is refused (no permission) |
