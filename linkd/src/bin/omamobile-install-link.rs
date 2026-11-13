// SPDX-License-Identifier: MIT
//! `omamobile-install-link status|reopen [--root DIR]`
#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("omamobile-install-link runs on the phone: Linux only");
    std::process::exit(2);
}

#[cfg(target_os = "linux")]
fn main() {
    std::process::exit(omamobile_linkd::command::main(
        std::env::args().skip(1).collect(),
    ));
}
