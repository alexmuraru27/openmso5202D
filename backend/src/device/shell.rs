//! The `0x43` command channel: run a shell command on the scope's embedded Linux.
//!
//! The scope exposes a second frame leader, `0x43` (`'C'`), with its own selector map.
//! `43 | 0x11 | <command>` runs the command **as root** on the instrument's Linux 3.2
//! userland and returns its stdout.
//!
//! # Safety
//!
//! This is unfiltered root access on a device with no recovery path — a bad command can
//! brick it. Two guards apply here:
//!
//! - [`check_command`] refuses commands containing an obviously destructive program, and
//!   refuses output redirection to anywhere but the removable card.
//! - The scope runs a **watchdog** that reboots it if a command stalls the acquisition
//!   application or desynchronises USB. Keep commands short and read-only.
//!
//! The guard is a safety net against mistakes, not a sandbox: a determined command can
//! still do damage. Treat this channel as read-only.

use crate::error::{Error, Result};

/// Programs refused outright — they write, erase, or kill, and can brick the scope or
/// corrupt its flash.
///
/// Matched against the basename of every token, so `rm` and `/bin/rm` both trip. Note
/// `cp`, `mkdir` and `touch` are *allowed*: writing to the inserted card under
/// `/mnt/udisk` is the intended export path.
const DESTRUCTIVE: &[&str] = &[
    "rm", "rmdir", "mv", "dd", "mkfs", "mke2fs", "mkdosfs", "mkfs.vfat", "fdisk", "sfdisk",
    "format", "kill", "killall", "pkill", "reboot", "halt", "poweroff", "shutdown", "init",
    "ubiformat", "ubidetach", "flash_erase", "flash_eraseall", "nandwrite", "nanddump",
    "mtd_debug", "chmod", "chown", "chgrp", "ln", "truncate", "tee", "mknod", "insmod",
    "rmmod", "modprobe", "mount", "umount", "passwd",
];

/// The only path prefix a shell command may redirect output into.
const WRITABLE_PREFIX: &str = "/mnt/udisk";

/// Reject a command that looks destructive.
///
/// Returns `Ok(())` if the command is allowed, or [`Error::UnsafeCommand`] describing why
/// it was refused.
pub fn check_command(command: &str) -> Result<()> {
    // Look at the program name of every segment, not just the first: `ls; rm -rf /` must
    // trip on the second.
    for token in command.split([' ', '\t', '|', ';', '&', '(', ')']) {
        let program = token.rsplit('/').next().unwrap_or(token);
        if DESTRUCTIVE.contains(&program) {
            return Err(Error::UnsafeCommand(format!(
                "'{program}' is blocked (destructive / can brick the scope)"
            )));
        }
    }
    // Output redirection anywhere but the removable card would overwrite scope files.
    if let Some(index) = command.find('>') {
        let target = command[index..].trim_start_matches('>').trim();
        if !target.starts_with(WRITABLE_PREFIX) {
            return Err(Error::UnsafeCommand(format!(
                "output redirection is only allowed under {WRITABLE_PREFIX}"
            )));
        }
    }
    Ok(())
}

/// Wrap a command so its output can be reliably attributed to *this* request.
///
/// Two firmware quirks force this shape:
///
/// - The firmware appends its own `> msg` redirect to the whole string, so `a; b` would
///   capture only `b`'s output. A **brace group** makes the redirect capture everything.
/// - Replies can arrive one command behind. Echoing a **unique marker** lets the caller
///   recognise the reply that belongs to this request and retry otherwise.
pub fn wrap_command(command: &str, marker: &str) -> String {
    format!("{{ {command} ; echo '{marker}' ; }}")
}

/// Build the unique end-marker for request number `sequence`.
pub fn marker_for(sequence: u64) -> String {
    format!("__MSOEND{sequence}__")
}

/// Split a reply at the marker, returning the command output that precedes it.
///
/// `None` means the marker was absent — the reply belongs to an earlier command and the
/// request should be retried.
pub fn output_before_marker<'a>(reply: &'a str, marker: &str) -> Option<&'a str> {
    reply.split_once(marker).map(|(output, _)| output)
}
