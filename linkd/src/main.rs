// SPDX-License-Identifier: MIT
//! `omamobile-linkd [--root DIR] --transport ffs|stdio`
#[cfg(target_os = "linux")]
use omamobile_linkd::daemon::{run, Options, Transport};
#[cfg(target_os = "linux")]
use std::path::PathBuf;

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("omamobile-linkd runs on the phone: Linux only");
    std::process::exit(2);
}

#[cfg(target_os = "linux")]
const USAGE: &str = "usage: omamobile-linkd [--root DIR] --transport ffs|stdio
  --root DIR         a scratch directory in place of the phone's file system
  --transport ffs    the link is FunctionFS, mounted by omamobile-link-gadget add
                     at /run/omamobile-install-link/ffs (under the root)
  --transport stdio  the link is the SOCK_SEQPACKET socket on standard input
For tests, with a scratch root only: --clock-scale N, --poll-ms N,
--write-timeout-ms N, --max-data N, --ack-every N, --window N,
--working-space BYTES, --die-file FILE";

#[cfg(target_os = "linux")]
fn main() {
    std::process::exit(match parse(std::env::args().skip(1).collect()) {
        Ok(opts) => run(opts),
        Err(why) => {
            eprintln!("omamobile-linkd: {why}\n{USAGE}");
            2
        }
    });
}

#[cfg(target_os = "linux")]
fn parse(args: Vec<String>) -> Result<Options, String> {
    let mut opts = Options::new(PathBuf::from("/"));
    let mut knobs = false;
    let mut it = args.into_iter();
    while let Some(arg) = it.next() {
        let mut value = || it.next().ok_or(format!("{arg} needs a value"));
        let number = |v: String| v.parse::<u64>().map_err(|_| format!("not a number: {v}"));
        match arg.as_str() {
            "--root" => opts.root = PathBuf::from(value()?),
            "--transport" => match value()?.as_str() {
                "stdio" => opts.transport = Transport::Stdio,
                "ffs" => opts.transport = Transport::Ffs,
                other => return Err(format!("no such transport: {other}")),
            },
            "--clock-scale" => (opts.clock_scale, knobs) = (number(value()?)?, true),
            "--poll-ms" => (opts.poll_ms, knobs) = (number(value()?)?, true),
            "--write-timeout-ms" => (opts.write_timeout_ms, knobs) = (number(value()?)?, true),
            "--max-data" => (opts.max_data, knobs) = (Some(number(value()?)? as u32), true),
            "--ack-every" => (opts.ack_every, knobs) = (Some(number(value()?)?), true),
            "--window" => (opts.window, knobs) = (Some(number(value()?)?), true),
            "--working-space" => (opts.working_space, knobs) = (number(value()?)?, true),
            "--die-file" => (opts.die_file, knobs) = (Some(PathBuf::from(value()?)), true),
            other => return Err(format!("unknown argument: {other}")),
        }
    }
    if !opts.root.is_absolute() {
        return Err("--root must be an absolute path".into());
    }
    // What changes a number of the plan, or kills the daemon, is for a
    // scratch root: never for a phone
    if knobs && opts.root == std::path::Path::new("/") {
        return Err("the test options need a scratch --root".into());
    }
    Ok(opts)
}
