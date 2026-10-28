// SPDX-License-Identifier: GPL-3.0-or-later
//! Importing an app's config file from a legacy backup zip (one the app made
//! itself, found under appdata by the backup scan).

use std::fs::File;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

use super::host;
use crate::error::{Error, Result};

/// Open a legacy backup zip once, after checking it's a file inside appdata,
/// so what's read later is what was checked.
pub(super) fn open_zip(zpath: &str, appdata_real: &Path) -> Result<zip::ZipArchive<File>> {
    let real = host::resolve(Path::new(zpath));
    if !Path::new(zpath).is_file() || !real.starts_with(appdata_real) {
        return Err(Error::invalid(format!("{zpath}: not inside appdata")));
    }
    let f = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32)
        .open(&real)
        .ok()
        .filter(|f| f.metadata().is_ok_and(|m| m.is_file()))
        .ok_or_else(|| Error::invalid(format!("{zpath}: not inside appdata")))?;
    zip::ZipArchive::new(f).map_err(|_| Error::invalid(format!("{zpath}: not a zip file")))
}

/// The config file `want` (case aside) from a legacy backup zip, and the mode
/// to give it: the one the zip recorded, else owner-only (it holds API keys).
pub(super) fn member(zf: &mut zip::ZipArchive<File>, zpath: &str, want: &str) -> Result<Option<(Vec<u8>, u32)>> {
    let member = zf.file_names().filter_map(|n| n.ok().map(|n| n.into_owned())).find(|n| n.to_lowercase() == want);
    let Some(member) = member else { return Ok(None) };
    let mut m = zf.by_name(&member).map_err(|e| Error::internal(format!("{zpath}: {e}")))?;
    let mode = m.unix_mode().map(|m| m & 0o777).filter(|m| m & 0o600 == 0o600).unwrap_or(0o600);
    let mut data = vec![];
    std::io::Read::read_to_end(&mut m, &mut data)?;
    Ok(Some((data, mode)))
}
