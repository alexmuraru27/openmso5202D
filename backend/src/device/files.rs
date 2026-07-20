//! Filesystem types and directory-listing parsing.
//!
//! Downloading a file is a protocol operation ([`crate::Device::download`], selector
//! `0x10`); *listing* a directory has no protocol selector, so it goes through the shell
//! channel and comes back as `ls -la` text. The parsing of that text is pure logic and
//! lives here.

/// One entry in a directory listing on the scope's filesystem.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileEntry {
    /// File name, without any directory component.
    pub name: String,
    /// Size in bytes as reported by `ls`.
    pub size: u64,
    /// Whether the entry is a directory.
    pub is_dir: bool,
}

impl FileEntry {
    /// Join this entry's name onto `dir` to form an absolute path.
    pub fn path_in(&self, dir: &str) -> String {
        format!("{}/{}", dir.trim_end_matches('/'), self.name)
    }
}

/// Parse the output of `ls -la <dir>` (BusyBox format) into entries.
///
/// Skips the `total N` header and the `.`/`..` entries. Lines that do not look like a
/// listing row are ignored rather than failing the whole parse — the shell channel
/// occasionally interleaves noise, and one bad line should not lose a directory.
///
/// Expected row shape (9+ whitespace-separated fields):
/// `perms links owner group size month day time name`
pub fn parse_ls(output: &str) -> Vec<FileEntry> {
    output.lines().filter_map(parse_ls_line).collect()
}

/// Parse a single `ls -la` row, or `None` if it is not one.
fn parse_ls_line(line: &str) -> Option<FileEntry> {
    let fields: Vec<&str> = line.split_whitespace().collect();
    if fields.len() < 9 {
        return None;
    }
    let perms = fields[0];
    // A permissions field always starts with the entry type and has 10 characters.
    if perms.len() < 10 || !perms.is_char_boundary(1) {
        return None;
    }
    let size = fields[4].parse::<u64>().ok()?;
    // The name is everything from field 8 on, so names containing spaces survive.
    let name = fields[8..].join(" ");
    if name == "." || name == ".." {
        return None;
    }
    Some(FileEntry {
        name,
        size,
        is_dir: perms.starts_with('d'),
    })
}
