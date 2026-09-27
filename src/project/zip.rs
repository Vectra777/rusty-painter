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

pub(super) const SIGNATURE: &[u8; 4] = b"PK\x03\x04";

struct Entry {
    name: String,
    crc: u32,
    size: u32,
    offset: u32,
}

#[derive(Default)]
pub(super) struct ZipWriter {
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
pub(super) fn read_entry<'a>(bytes: &'a [u8], name: &str) -> Result<&'a [u8], String> {
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

#[cfg(test)]
mod tests {
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
    fn damage_is_caught() {
        let mut zip = ZipWriter::default();
        zip.add("data", &[7; 100]).unwrap();
        let mut bytes = zip.finish().unwrap();
        bytes[40] ^= 1;
        assert!(read_entry(&bytes, "data").is_err());
        assert!(read_entry(&bytes[..bytes.len() - 3], "data").is_err());
    }
}
