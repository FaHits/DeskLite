use std::io::{Cursor, Read, Write};

use cfb::{CompoundFile, Version};

use crate::LegacyError;
use crate::rc4::{DESKGO_RC4_KEY, rc4_apply};
use crate::records::{LegacyBoxRecord, LegacyItemRecord};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConStream {
    pub name: String,
    pub plain: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ConFile {
    pub streams: Vec<ConStream>,
}

impl ConFile {
    pub fn get_raw(&self, key: &str) -> Option<&[u8]> {
        self.streams
            .iter()
            .find(|s| s.name == key)
            .map(|s| s.plain.as_slice())
    }

    pub fn set_raw(&mut self, key: impl Into<String>, plain: Vec<u8>) {
        let key = key.into();
        if let Some(slot) = self.streams.iter_mut().find(|s| s.name == key) {
            slot.plain = plain;
        } else {
            self.streams.push(ConStream { name: key, plain });
        }
    }

    pub fn get_int(&self, key: &str, default: i32) -> i32 {
        let Some(b) = self.get_raw(key) else {
            return default;
        };
        if b.len() < 4 {
            return default;
        }
        i32::from_le_bytes([b[0], b[1], b[2], b[3]])
    }

    pub fn set_int(&mut self, key: impl Into<String>, value: i32) {
        self.set_raw(key, value.to_le_bytes().to_vec());
    }

    pub fn get_str(&self, key: &str) -> Option<String> {
        let b = self.get_raw(key)?;
        if b.len() < 2 {
            return Some(String::new());
        }
        let mut units = Vec::with_capacity(b.len() / 2);
        for &chunk in b.as_chunks::<2>().0 {
            let u = u16::from_le_bytes(chunk);
            if u == 0 {
                break;
            }
            units.push(u);
        }
        Some(String::from_utf16_lossy(&units))
    }

    pub fn set_str(&mut self, key: impl Into<String>, value: &str) {
        let mut bytes = Vec::with_capacity((value.len() + 1) * 2);
        for u in value.encode_utf16() {
            bytes.extend_from_slice(&u.to_le_bytes());
        }
        bytes.extend_from_slice(&0u16.to_le_bytes());
        self.set_raw(key, bytes);
    }

    pub fn boxes(&self) -> Result<Vec<LegacyBoxRecord>, LegacyError> {
        let count = self.get_int("CFG_FENCESBOX_COUNT", 0).max(0) as usize;
        let mut out = Vec::with_capacity(count);
        for i in 0..count {
            let key = format!("CFG_FENCESBOX_INDEX{i}");
            if let Some(rec) = self.get_str(&key) {
                out.push(LegacyBoxRecord::from_raw("Box", &rec)?);
            }
        }
        Ok(out)
    }

    pub fn items(&self) -> Result<Vec<LegacyItemRecord>, LegacyError> {
        let count = self.get_int("CFG_FENCESITEM_COUNT", 0).max(0) as usize;
        let mut out = Vec::with_capacity(count);
        for i in 0..count {
            let key = format!("CFG_FENCESITEM_INDEX{i}");
            if let Some(rec) = self.get_str(&key) {
                out.push(LegacyItemRecord::from_raw("Item", &rec)?);
            }
        }
        Ok(out)
    }

    /// Rewrites box and item index streams atomically in memory while preserving all unknown streams verbatim.
    pub fn sync_boxes_and_items(&mut self, boxes: &[LegacyBoxRecord], items: &[LegacyItemRecord]) {
        self.streams.retain(|s| {
            !is_indexed_stream(&s.name, "CFG_FENCESBOX_INDEX")
                && !is_indexed_stream(&s.name, "CFG_FENCESITEM_INDEX")
        });

        self.set_int("CFG_FENCES_DEFAULTUSEDATA", 1);
        if self.get_raw("CFG_FENCES_CONFIGVERSION").is_none() {
            self.set_int("CFG_FENCES_CONFIGVERSION", 30);
        }
        self.set_int("CFG_FENCES_SAVEINGDATA", 0);

        self.set_int("CFG_FENCESBOX_COUNT", boxes.len() as i32);
        for (i, b) in boxes.iter().enumerate() {
            self.set_str(format!("CFG_FENCESBOX_INDEX{i}"), &b.to_con_record());
        }

        self.set_int("CFG_FENCESITEM_COUNT", items.len() as i32);
        for (i, it) in items.iter().enumerate() {
            self.set_str(format!("CFG_FENCESITEM_INDEX{i}"), &it.to_con_record());
        }
    }
}

fn is_indexed_stream(name: &str, prefix: &str) -> bool {
    name.strip_prefix(prefix)
        .is_some_and(|rest| !rest.is_empty() && rest.chars().all(|c| c.is_ascii_digit()))
}

pub fn read_con_file(bytes: &[u8]) -> Result<ConFile, LegacyError> {
    let cursor = Cursor::new(bytes);
    let mut comp = CompoundFile::open(cursor).map_err(|e| LegacyError::Cfb(e.to_string()))?;

    let mut names = Vec::new();
    for entry in comp.walk() {
        if entry.is_stream() {
            let path_str = entry.path().to_string_lossy().into_owned();
            let normalized = path_str.replace('\\', "/");
            let trimmed = normalized.trim_start_matches('/');
            if let Some(name) = trimmed.strip_prefix("Data/") {
                names.push((path_str, name.to_string()));
            }
        }
    }

    let mut streams = Vec::with_capacity(names.len());
    for (full_path, name) in names {
        let mut s = comp
            .open_stream(&full_path)
            .map_err(|e| LegacyError::Cfb(e.to_string()))?;
        let mut cipher = Vec::new();
        s.read_to_end(&mut cipher)?;
        streams.push(ConStream {
            name,
            plain: rc4_apply(DESKGO_RC4_KEY, &cipher),
        });
    }
    Ok(ConFile { streams })
}

pub fn write_con_file(con: &ConFile) -> Result<Vec<u8>, LegacyError> {
    let cursor = Cursor::new(Vec::new());
    let mut comp = CompoundFile::create_with_version(Version::V3, cursor)
        .map_err(|e| LegacyError::Cfb(e.to_string()))?;
    comp.create_storage("/Data")
        .map_err(|e| LegacyError::Cfb(e.to_string()))?;

    for s in &con.streams {
        let full_path = format!("/Data/{}", s.name);
        let cipher = rc4_apply(DESKGO_RC4_KEY, &s.plain);
        let mut stream = comp
            .create_stream(&full_path)
            .map_err(|e| LegacyError::Cfb(e.to_string()))?;
        stream.write_all(&cipher)?;
    }
    comp.flush().map_err(|e| LegacyError::Cfb(e.to_string()))?;
    Ok(comp.into_inner().into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn con_file_roundtrip_preserves_opaque_and_typed_streams() {
        let mut con = ConFile::default();
        con.set_int("CFG_FENCES_CONFIGVERSION", 30);
        con.set_raw("CFG_SETTING_PERIOD_REPORT", vec![1, 2, 3, 4, 5, 6, 7, 8]);
        con.set_raw("TIPS_POPUP_TIME_002006", vec![0xAA; 112]);
        let b = LegacyBoxRecord::from_raw(
            "Box",
            "101<|>Demo<|>10<|>20<|>310<|>420<|>0<|>0<|>3<|>4<|>1",
        )
        .unwrap();
        let it = LegacyItemRecord::from_raw(
            "Item",
            "Readme.md<|>101<|>0<|>4<|>C:\\Users\\Demo\\Desktop\\Readme.md<|>0",
        )
        .unwrap();
        con.sync_boxes_and_items(&[b], &[it]);

        let raw = write_con_file(&con).unwrap();
        let parsed = read_con_file(&raw).unwrap();
        assert_eq!(
            parsed.get_raw("CFG_SETTING_PERIOD_REPORT"),
            Some([1, 2, 3, 4, 5, 6, 7, 8].as_slice())
        );
        assert_eq!(parsed.get_raw("TIPS_POPUP_TIME_002006").unwrap().len(), 112);
        assert_eq!(parsed.boxes().unwrap().len(), 1);
        assert_eq!(parsed.items().unwrap().len(), 1);
    }
}
