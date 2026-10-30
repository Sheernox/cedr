//! Complete-line cursors validate file identity and the preceding tail before resuming.
use crate::Result;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs::File,
    io::{BufRead, BufReader, Read, Seek, SeekFrom},
    path::Path,
};
pub const MAX_LINE: usize = 8 * 1024 * 1024;
#[derive(Default, Serialize, Deserialize)]
pub struct Cursor {
    pub device: u64,
    pub inode: u64,
    pub size: u64,
    pub mtime: u128,
    pub offset: u64,
    pub hash: String,
    pub state: serde_json::Value,
    pub malformed: u64,
}
fn hash(file: &mut File, offset: u64) -> Result<String> {
    let start = offset.saturating_sub(4096);
    file.seek(SeekFrom::Start(start))?;
    let mut b = vec![0; (offset - start) as usize];
    file.read_exact(&mut b)?;
    Ok(Sha256::digest(b)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect())
}
pub fn read(
    path: &Path,
    mut c: Cursor,
    mut visit: impl FnMut(serde_json::Value, u64, &mut serde_json::Value) -> Result<bool>,
) -> Result<Cursor> {
    let mut f = File::open(path)?;
    let m = f.metadata()?;
    #[cfg(unix)]
    let (device, inode) = {
        use std::os::unix::fs::MetadataExt;
        (m.dev(), m.ino())
    };
    #[cfg(not(unix))]
    let (device, inode) = (0, 0);
    let mtime = m
        .modified()?
        .duration_since(std::time::UNIX_EPOCH)?
        .as_nanos();
    if (m.len() <= c.size && mtime != c.mtime)
        || c.device != device
        || c.inode != inode
        || c.offset > m.len()
        || (c.offset > 0 && hash(&mut f, c.offset)? != c.hash)
    {
        c = Cursor::default();
    }
    f.seek(SeekFrom::Start(c.offset))?;
    let mut reader = BufReader::new(f);
    let mut line = Vec::new();
    let mut consumed = 0u64;
    let mut oversized = false;
    loop {
        let chunk = reader.fill_buf()?;
        if chunk.is_empty() {
            break;
        }
        let n = chunk
            .iter()
            .position(|b| *b == b'\n')
            .map_or(chunk.len(), |i| i + 1);
        let complete = chunk[n - 1] == b'\n';
        consumed += n as u64;
        if line.len() + n <= MAX_LINE && !oversized {
            line.extend_from_slice(&chunk[..n]);
        } else {
            oversized = true;
            line.clear();
        }
        reader.consume(n);
        if complete {
            c.offset += consumed;
            consumed = 0;
            if oversized {
                c.malformed += 1;
            } else {
                match serde_json::from_slice(&line) {
                    Ok(v) => {
                        if !visit(v, c.offset, &mut c.state)? {
                            c.malformed += 1;
                        }
                    }
                    Err(_) => c.malformed += 1,
                }
            }
            oversized = false;
            line.clear();
        }
    }
    let mut f = reader.into_inner();
    c.hash = hash(&mut f, c.offset)?;
    c.device = device;
    c.inode = inode;
    c.size = m.len();
    c.mtime = m
        .modified()?
        .duration_since(std::time::UNIX_EPOCH)?
        .as_nanos();
    Ok(c)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn resume_truncate_and_oversized() {
        use std::io::Write;
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("log");
        std::fs::write(&p, b"{}\n").unwrap();
        let mut seen = 0;
        let c = read(&p, Cursor::default(), |_, _, _| {
            seen += 1;
            Ok(true)
        })
        .unwrap();
        assert_eq!(seen, 1);
        let mut f = std::fs::OpenOptions::new().append(true).open(&p).unwrap();
        f.write_all(b"{}\n{").unwrap();
        let c = read(&p, c, |_, _, _| {
            seen += 1;
            Ok(true)
        })
        .unwrap();
        assert_eq!(seen, 2);
        assert_eq!(c.offset, 6);
        std::fs::write(&p, b"{}\n").unwrap();
        let c = read(&p, c, |_, _, _| {
            seen += 1;
            Ok(true)
        })
        .unwrap();
        assert_eq!(seen, 3);
        assert_eq!(c.offset, 3);
        std::fs::write(&p, vec![b'x'; MAX_LINE + 1]).unwrap();
        let mut f = std::fs::OpenOptions::new().append(true).open(&p).unwrap();
        f.write_all(b"\n{}\n").unwrap();
        let c = read(&p, c, |_, _, _| Ok(true)).unwrap();
        assert_eq!(c.malformed, 1);
        assert_eq!(c.offset, (MAX_LINE + 5) as u64);
    }
    #[test]
    fn partial_and_rewrite() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("a");
        std::fs::write(&p, b"{}\n{").unwrap();
        let c = read(&p, Cursor::default(), |_, _, _| Ok(true)).unwrap();
        assert_eq!(c.offset, 3);
        std::fs::write(&p, b"{\"a\":1}\n").unwrap();
        let c = read(&p, c, |_, _, _| Ok(true)).unwrap();
        assert_eq!(c.offset, 8);
    }
}
