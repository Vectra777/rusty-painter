//! A minimal ZIP archive of stored (uncompressed) entries: just what an
//! OpenRaster container needs. The PNGs and the project payload are
//! compressed already, so nothing is deflated.

const LOCAL_HEADER: u32 = 0x0403_4b50;
const CENTRAL_HEADER: u32 = 0x0201_4b50;
const END_OF_DIRECTORY: u32 = 0x0605_4b50;
/// Version 2.0: the lowest that other readers expect.
const ZIP_VERSION: u16 = 20;
/// Names are UTF-8.
const UTF8_NAMES: u16 = 1 << 11;
/// 1980-01-01 00:00 in MS-DOS time: no timestamps, so saves are repeatable.
const DOS_DATE: u16 = (1 << 5) | 1;

pub(crate) const SIGNATURE: &[u8; 4] = b"PK\x03\x04";

struct Entry {
    name: String,
    crc: u32,
    size: u32,
    offset: u32,
}

#[derive(Default)]
pub(crate) struct ZipWriter {
    out: Vec<u8>,
    entries: Vec<Entry>,
}

fn put16(out: &mut Vec<u8>, v: u16) {
    out.extend_from_slice(&v.to_le_bytes());
}

fn put32(out: &mut Vec<u8>, v: u32) {
    out.extend_from_slice(&v.to_le_bytes());
}

impl ZipWriter {
    /// Add a stored entry. The first entry's data starts right after its
    /// 30-byte header and name (no extra field), where OpenRaster's
    /// `mimetype` has to be.
    pub fn add(&mut self, name: &str, data: &[u8]) -> Result<(), String> {
        let too_big = || "Project is too large to save (over 4 GB)".to_string();
        let size = u32::try_from(data.len()).map_err(|_| too_big())?;
        let offset = u32::try_from(self.out.len()).map_err(|_| too_big())?;
        let crc = crc32fast::hash(data);
        let out = &mut self.out;
        put32(out, LOCAL_HEADER);
        put16(out, ZIP_VERSION);
        put16(out, UTF8_NAMES);
        put16(out, 0); // stored
        put16(out, 0); // time
        put16(out, DOS_DATE);
        put32(out, crc);
        put32(out, size);
        put32(out, size);
        put16(out, name.len() as u16);
        put16(out, 0); // no extra field
        out.extend_from_slice(name.as_bytes());
        out.extend_from_slice(data);
        u32::try_from(out.len()).map_err(|_| too_big())?;
        self.entries.push(Entry {
            name: name.to_string(),
            crc,
            size,
            offset,
        });
        Ok(())
    }

    pub fn finish(mut self) -> Result<Vec<u8>, String> {
        let directory_start = self.out.len() as u32;
        let out = &mut self.out;
        for e in &self.entries {
            put32(out, CENTRAL_HEADER);
            put16(out, ZIP_VERSION);
            put16(out, ZIP_VERSION);
            put16(out, UTF8_NAMES);
            put16(out, 0);
            put16(out, 0);
            put16(out, DOS_DATE);
            put32(out, e.crc);
            put32(out, e.size);
            put32(out, e.size);
            put16(out, e.name.len() as u16);
            put16(out, 0); // extra
            put16(out, 0); // comment
            put16(out, 0); // disk
            put16(out, 0); // internal attributes
            put32(out, 0); // external attributes
            put32(out, e.offset);
            out.extend_from_slice(e.name.as_bytes());
        }
        let directory_len = u32::try_from(out.len() - directory_start as usize)
            .map_err(|_| "Project is too large to save".to_string())?;
        let count = u16::try_from(self.entries.len())
            .map_err(|_| "Too many entries in the project".to_string())?;
        put32(out, END_OF_DIRECTORY);
        put16(out, 0);
        put16(out, 0);
        put16(out, count);
        put16(out, count);
        put32(out, directory_len);
        put32(out, directory_start);
        put16(out, 0); // comment
        Ok(self.out)
    }
}

fn get16(bytes: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(bytes.get(at..at + 2)?.try_into().ok()?))
}

fn get32(bytes: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(bytes.get(at..at + 4)?.try_into().ok()?))
}

/// The data of entry `name`, which must be stored (not compressed).
pub(crate) fn read_entry<'a>(bytes: &'a [u8], name: &str) -> Result<&'a [u8], String> {
    let broken = || "Damaged project file".to_string();
    // The end record is the last 22 bytes, plus a comment of up to 64 KB.
    let search_from = bytes.len().saturating_sub(22 + usize::from(u16::MAX));
    let end = (search_from..=bytes.len().saturating_sub(22))
        .rev()
        .find(|&at| get32(bytes, at) == Some(END_OF_DIRECTORY))
        .ok_or_else(broken)?;
    let count = get16(bytes, end + 10).ok_or_else(broken)?;
    let mut at = get32(bytes, end + 16).ok_or_else(broken)? as usize;
    for _ in 0..count {
        if get32(bytes, at) != Some(CENTRAL_HEADER) {
            return Err(broken());
        }
        let method = get16(bytes, at + 10).ok_or_else(broken)?;
        let crc = get32(bytes, at + 16).ok_or_else(broken)?;
        let size = get32(bytes, at + 20).ok_or_else(broken)? as usize;
        let name_len = get16(bytes, at + 28).ok_or_else(broken)? as usize;
        let extra_len = get16(bytes, at + 30).ok_or_else(broken)? as usize;
        let comment_len = get16(bytes, at + 32).ok_or_else(broken)? as usize;
        let offset = get32(bytes, at + 42).ok_or_else(broken)? as usize;
        let entry_name = bytes.get(at + 46..at + 46 + name_len).ok_or_else(broken)?;
        if entry_name == name.as_bytes() {
            if method != 0 {
                return Err(format!("Unsupported compression for {name}"));
            }
            if get32(bytes, offset) != Some(LOCAL_HEADER) {
                return Err(broken());
            }
            let local_name = get16(bytes, offset + 26).ok_or_else(broken)? as usize;
            let local_extra = get16(bytes, offset + 28).ok_or_else(broken)? as usize;
            let start = offset + 30 + local_name + local_extra;
            let data = bytes.get(start..start + size).ok_or_else(broken)?;
            if crc32fast::hash(data) != crc {
                return Err(broken());
            }
            return Ok(data);
        }
        at += 46 + name_len + extra_len + comment_len;
    }
    Err(format!("{name} is missing from the project file"))
}

/// The data of stored entry `name` in the archive at `path`, reading only
/// the directory and that entry (a project's thumbnail without loading the
/// whole project).
pub(crate) fn read_entry_from_file(path: &std::path::Path, name: &str) -> Result<Vec<u8>, String> {
    use std::io::{Read, Seek, SeekFrom};
    let broken = || "Damaged project file".to_string();
    let io = |e: std::io::Error| e.to_string();
    let mut file = std::fs::File::open(path).map_err(io)?;
    let len = file.metadata().map_err(io)?.len();
    let mut read_at = |at: u64, n: usize| -> Result<Vec<u8>, String> {
        let mut buf = vec![0; n];
        file.seek(SeekFrom::Start(at)).map_err(io)?;
        file.read_exact(&mut buf).map_err(io)?;
        Ok(buf)
    };
    let tail_len = len.min(22 + u64::from(u16::MAX));
    let tail = read_at(len - tail_len, tail_len as usize)?;
    let end = (0..=tail.len().saturating_sub(22))
        .rev()
        .find(|&at| get32(&tail, at) == Some(END_OF_DIRECTORY))
        .ok_or_else(broken)?;
    let count = get16(&tail, end + 10).ok_or_else(broken)?;
    let dir_len = get32(&tail, end + 12).ok_or_else(broken)? as usize;
    let dir_start = get32(&tail, end + 16).ok_or_else(broken)?;
    let dir = read_at(u64::from(dir_start), dir_len)?;
    let mut at = 0;
    for _ in 0..count {
        if get32(&dir, at) != Some(CENTRAL_HEADER) {
            return Err(broken());
        }
        let method = get16(&dir, at + 10).ok_or_else(broken)?;
        let crc = get32(&dir, at + 16).ok_or_else(broken)?;
        let size = get32(&dir, at + 20).ok_or_else(broken)? as usize;
        let name_len = get16(&dir, at + 28).ok_or_else(broken)? as usize;
        let extra_len = get16(&dir, at + 30).ok_or_else(broken)? as usize;
        let comment_len = get16(&dir, at + 32).ok_or_else(broken)? as usize;
        let offset = u64::from(get32(&dir, at + 42).ok_or_else(broken)?);
        if dir.get(at + 46..at + 46 + name_len) == Some(name.as_bytes()) {
            if method != 0 {
                return Err(format!("Unsupported compression for {name}"));
            }
            let local = read_at(offset, 30)?;
            if get32(&local, 0) != Some(LOCAL_HEADER) {
                return Err(broken());
            }
            let skip = 30
                + get16(&local, 26).ok_or_else(broken)? as u64
                + get16(&local, 28).ok_or_else(broken)? as u64;
            let data = read_at(offset + skip, size)?;
            if crc32fast::hash(&data) != crc {
                return Err(broken());
            }
            return Ok(data);
        }
        at += 46 + name_len + extra_len + comment_len;
    }
    Err(format!("{name} is missing from the project file"))
}

/// Largest entry inflated from another app's archive (a brush bundle).
const MAX_INFLATED: usize = 256 << 20;

/// Every entry of a ZIP archive, stored or deflated (other apps' archives,
/// such as `.bundle` brush sets, compress theirs): `(name, data)`.
pub(crate) fn read_all(bytes: &[u8]) -> Result<Vec<(String, Vec<u8>)>, String> {
    let broken = || "Damaged archive".to_string();
    let search_from = bytes.len().saturating_sub(22 + usize::from(u16::MAX));
    let end = (search_from..=bytes.len().saturating_sub(22))
        .rev()
        .find(|&at| get32(bytes, at) == Some(END_OF_DIRECTORY))
        .ok_or_else(broken)?;
    let count = get16(bytes, end + 10).ok_or_else(broken)?;
    let mut at = get32(bytes, end + 16).ok_or_else(broken)? as usize;
    let mut out = Vec::with_capacity(count as usize);
    for _ in 0..count {
        if get32(bytes, at) != Some(CENTRAL_HEADER) {
            return Err(broken());
        }
        let method = get16(bytes, at + 10).ok_or_else(broken)?;
        let crc = get32(bytes, at + 16).ok_or_else(broken)?;
        let packed = get32(bytes, at + 20).ok_or_else(broken)? as usize;
        let size = get32(bytes, at + 24).ok_or_else(broken)? as usize;
        let name_len = get16(bytes, at + 28).ok_or_else(broken)? as usize;
        let extra_len = get16(bytes, at + 30).ok_or_else(broken)? as usize;
        let comment_len = get16(bytes, at + 32).ok_or_else(broken)? as usize;
        let offset = get32(bytes, at + 42).ok_or_else(broken)? as usize;
        let name = bytes.get(at + 46..at + 46 + name_len).ok_or_else(broken)?;
        let name = String::from_utf8_lossy(name).into_owned();
        at += 46 + name_len + extra_len + comment_len;
        if name.ends_with('/') {
            continue;
        }
        if get32(bytes, offset) != Some(LOCAL_HEADER) {
            return Err(broken());
        }
        let local_name = get16(bytes, offset + 26).ok_or_else(broken)? as usize;
        let local_extra = get16(bytes, offset + 28).ok_or_else(broken)? as usize;
        let start = offset + 30 + local_name + local_extra;
        let raw = bytes.get(start..start + packed).ok_or_else(broken)?;
        let data = match method {
            0 => raw.to_vec(),
            8 => {
                if size > MAX_INFLATED {
                    return Err(format!("{name} is too large"));
                }
                miniz_oxide::inflate::decompress_to_vec_with_limit(raw, MAX_INFLATED)
                    .map_err(|_| broken())?
            }
            _ => return Err(format!("Unsupported compression for {name}")),
        };
        if crc32fast::hash(&data) != crc {
            return Err(broken());
        }
        out.push((name, data));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    #[test]
    fn an_entry_reads_from_the_file_alone() {
        let mut zip = super::ZipWriter::default();
        zip.add("big", &vec![7u8; 100_000]).unwrap();
        zip.add("small", b"thumb").unwrap();
        let path = std::env::temp_dir().join(format!("rp-zip-{}.zip", std::process::id()));
        std::fs::write(&path, zip.finish().unwrap()).unwrap();
        assert_eq!(
            super::read_entry_from_file(&path, "small").unwrap(),
            b"thumb"
        );
        assert!(super::read_entry_from_file(&path, "none").is_err());
        let _ = std::fs::remove_file(&path);
    }

    use super::*;

    #[test]
    fn entries_round_trip() {
        let mut zip = ZipWriter::default();
        zip.add("mimetype", b"image/openraster").unwrap();
        zip.add("a/b.bin", &[1, 2, 3]).unwrap();
        zip.add("empty", &[]).unwrap();
        let bytes = zip.finish().unwrap();
        assert!(bytes.starts_with(SIGNATURE));
        assert_eq!(read_entry(&bytes, "a/b.bin").unwrap(), &[1, 2, 3]);
        assert_eq!(read_entry(&bytes, "empty").unwrap(), &[] as &[u8]);
        assert!(read_entry(&bytes, "nope").is_err());
    }

    #[test]
    fn deflated_entries_are_read() {
        // A one-entry archive with a deflated entry, as other apps write.
        let text = b"hello hello hello hello, deflate me".repeat(4);
        let packed = miniz_oxide::deflate::compress_to_vec(&text, 6);
        let name = b"dir/a.txt";
        let crc = crc32fast::hash(&text);
        let mut z = Vec::new();
        let mut local = Vec::new();
        put32(&mut local, LOCAL_HEADER);
        put16(&mut local, ZIP_VERSION);
        put16(&mut local, 0);
        put16(&mut local, 8);
        put16(&mut local, 0);
        put16(&mut local, DOS_DATE);
        put32(&mut local, crc);
        put32(&mut local, packed.len() as u32);
        put32(&mut local, text.len() as u32);
        put16(&mut local, name.len() as u16);
        put16(&mut local, 0);
        local.extend_from_slice(name);
        z.extend_from_slice(&local);
        z.extend_from_slice(&packed);
        let dir = z.len() as u32;
        put32(&mut z, CENTRAL_HEADER);
        put16(&mut z, ZIP_VERSION);
        put16(&mut z, ZIP_VERSION);
        put16(&mut z, 0);
        put16(&mut z, 8);
        put16(&mut z, 0);
        put16(&mut z, DOS_DATE);
        put32(&mut z, crc);
        put32(&mut z, packed.len() as u32);
        put32(&mut z, text.len() as u32);
        put16(&mut z, name.len() as u16);
        put16(&mut z, 0);
        put16(&mut z, 0);
        put16(&mut z, 0);
        put16(&mut z, 0);
        put32(&mut z, 0);
        put32(&mut z, 0);
        z.extend_from_slice(name);
        let dir_len = z.len() as u32 - dir;
        put32(&mut z, END_OF_DIRECTORY);
        put16(&mut z, 0);
        put16(&mut z, 0);
        put16(&mut z, 1);
        put16(&mut z, 1);
        put32(&mut z, dir_len);
        put32(&mut z, dir);
        put16(&mut z, 0);
        let entries = read_all(&z).unwrap();
        assert_eq!(entries, vec![("dir/a.txt".to_string(), text.clone())]);
        // Our own stored archives read the same way.
        let mut w = ZipWriter::default();
        w.add("x", b"stored").unwrap();
        assert_eq!(read_all(&w.finish().unwrap()).unwrap()[0].1, b"stored");
        // Damage is caught.
        let mut bad = z.clone();
        bad[40] ^= 0xFF;
        assert!(read_all(&bad).is_err());
    }

    #[test]
    fn damage_is_caught() {
        let mut zip = ZipWriter::default();
        zip.add("data", &[7; 100]).unwrap();
        let mut bytes = zip.finish().unwrap();
        bytes[40] ^= 1;
        assert!(read_entry(&bytes, "data").is_err());
        assert!(read_entry(&bytes[..bytes.len() - 3], "data").is_err());
    }
}
