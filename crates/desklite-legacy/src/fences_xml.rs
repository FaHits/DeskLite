use std::collections::HashSet;

use crate::LegacyError;
use crate::rc4::{DESKGO_RC4_KEY, rc4_apply};
use crate::records::{LegacyBoxRecord, LegacyItemRecord, LegacyScreenRecord};

pub const XML_HEAD: &str = "<?xml version=\"1.0\" encoding=\"UTF-8\" ?>\n<root>\n";
pub const XML_TAIL: &str = "</root>\n\n\0";

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct FencesXmlModel {
    pub boxes: Vec<LegacyBoxRecord>,
    pub screens: Vec<LegacyScreenRecord>,
    pub items: Vec<LegacyItemRecord>,
}

impl FencesXmlModel {
    /// Strips non-standard boxes such as `Box_ai_cloud` and their linked items/screen entries.
    pub fn strip_unknown_boxes(&mut self) {
        let mut dropped_ids = HashSet::new();
        self.boxes.retain(|b| {
            let keep = b.tag == "Box";
            if !keep {
                dropped_ids.insert(b.id);
            }
            keep
        });
        if !dropped_ids.is_empty() {
            self.screens.retain(|s| !dropped_ids.contains(&s.box_id));
            self.items.retain(|it| !dropped_ids.contains(&it.box_id));
        }
    }
}

fn extract_section<'a>(text: &'a str, section: &str) -> &'a str {
    let open = format!("<{section}>");
    let close = format!("</{section}>");
    let Some(start) = text.find(&open) else {
        return "";
    };
    let rest = &text[start + open.len()..];
    let Some(end) = rest.find(&close) else {
        return "";
    };
    &rest[..end]
}

fn extract_cdata_entries(section_body: &str) -> Result<Vec<(String, String)>, LegacyError> {
    let mut out = Vec::new();
    let mut cursor = section_body;
    while let Some(open_lt) = cursor.find('<') {
        let after_lt = &cursor[open_lt + 1..];
        let Some(open_gt) = after_lt.find('>') else {
            break;
        };
        let tag = after_lt[..open_gt].trim();
        if tag.is_empty() || tag.starts_with('/') || tag.ends_with('/') || tag.starts_with('!') {
            cursor = &after_lt[open_gt + 1..];
            continue;
        }
        let close_tag = format!("</{tag}>");
        let inner_start = &after_lt[open_gt + 1..];
        let Some(close_idx) = inner_start.find(&close_tag) else {
            return Err(LegacyError::Xml(format!("missing closing tag {close_tag}")));
        };
        let block = &inner_start[..close_idx];
        let cdata_start = block
            .find("<![CDATA[")
            .ok_or_else(|| LegacyError::Xml(format!("missing CDATA inside <{tag}>")))?;
        let cdata_body = &block[cdata_start + 9..];
        let cdata_end = cdata_body
            .find("]]>")
            .ok_or_else(|| LegacyError::Xml(format!("unterminated CDATA inside <{tag}>")))?;
        out.push((tag.to_string(), cdata_body[..cdata_end].to_string()));
        cursor = &inner_start[close_idx + close_tag.len()..];
    }
    Ok(out)
}

pub fn parse_fences_xml(text: &str) -> Result<FencesXmlModel, LegacyError> {
    let clean = text.trim_end_matches('\0');
    let mut boxes = Vec::new();
    for (tag, rec) in extract_cdata_entries(extract_section(clean, "BoxList"))? {
        boxes.push(LegacyBoxRecord::from_raw(tag, &rec)?);
    }
    let mut screens = Vec::new();
    for (tag, rec) in extract_cdata_entries(extract_section(clean, "BoxListScreenInfo"))? {
        screens.push(LegacyScreenRecord::from_raw(tag, &rec)?);
    }
    let mut items = Vec::new();
    for (tag, rec) in extract_cdata_entries(extract_section(clean, "ItemList"))? {
        items.push(LegacyItemRecord::from_raw(tag, &rec)?);
    }
    Ok(FencesXmlModel {
        boxes,
        screens,
        items,
    })
}

fn push_rec_block(out: &mut String, tag: &str, desc: &str, rec: &str) {
    out.push_str("\t\t<");
    out.push_str(tag);
    out.push_str(">\n\t\t\t<");
    out.push_str(desc);
    out.push_str(">\n\t\t\t\t<![CDATA[");
    out.push_str(rec);
    out.push_str("]]>\n\t\t\t</");
    out.push_str(desc);
    out.push_str(">\n\t\t</");
    out.push_str(tag);
    out.push_str(">\n");
}

pub fn build_fences_xml(model: &FencesXmlModel) -> String {
    let mut out = String::with_capacity(4096);
    out.push_str(XML_HEAD);
    out.push_str("\t<BoxList>\n");
    for b in &model.boxes {
        push_rec_block(&mut out, &b.tag, "BoxDesc", &b.to_xml_record());
    }
    out.push_str("\t</BoxList>\n");
    out.push_str("\t<BoxDoc/>\n");
    out.push_str("\t<BoxListScreenInfo>\n");
    for s in &model.screens {
        push_rec_block(&mut out, &s.tag, "BoxDesc", &s.to_xml_record());
    }
    out.push_str("\t</BoxListScreenInfo>\n");
    out.push_str("\t<ItemList>\n");
    for it in &model.items {
        push_rec_block(&mut out, &it.tag, "ItemDesc", &it.to_xml_record());
    }
    out.push_str("\t</ItemList>\n");
    out.push_str("\t<ItemListDoc/>\n");
    out.push_str(XML_TAIL);
    out
}

pub fn decrypt_fences_bytes(cipher: &[u8]) -> Result<FencesXmlModel, LegacyError> {
    let plain_bytes = rc4_apply(DESKGO_RC4_KEY, cipher);
    let plain_str = String::from_utf8(plain_bytes)?;
    parse_fences_xml(&plain_str)
}

pub fn encrypt_fences_bytes(model: &FencesXmlModel) -> Vec<u8> {
    let xml = build_fences_xml(model);
    rc4_apply(DESKGO_RC4_KEY, xml.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;
    use desklite_core::Rect;

    #[test]
    fn xml_byte_exact_roundtrip_and_strip_cloud_box() {
        let model = FencesXmlModel {
            boxes: vec![
                LegacyBoxRecord::from_raw(
                    "Box",
                    "101<|>Projects<|>2286<|>10<|>2550<|>238<|>0<|>0<|>3<|>4<|>1<|>0<|>0<|>0<|>0<|>0<|>0<|>0<|>1<|>0<|>0<|>0<|>0<|>",
                )
                .unwrap(),
                LegacyBoxRecord::from_raw(
                    "Box_ai_cloud",
                    "1000000<|>CloudDocs<|>100<|>100<|>400<|>400<|>0<|>0<|>3<|>4<|>1<|>0<|>0<|>0<|>0<|>0<|>0<|>0<|>1<|>0<|>0<|>0<|>0<|>",
                )
                .unwrap(),
            ],
            screens: vec![
                LegacyScreenRecord::from_raw(
                    "Box",
                    "101<|>PrimaryDisplay:{00000000-0000-0000-0000-000000000000}/001<|>0<|>0<|>2560<|>1440<|>0<|>0<|>2560<|>1392<|>1<|>-1<|>-1<|>-1<|>-1<|>-1<|>-1",
                )
                .unwrap(),
                LegacyScreenRecord::from_raw(
                    "Box_ai_cloud",
                    "1000000<|><|>0<|>0<|>2560<|>1440<|>0<|>0<|>2560<|>1392<|>1<|>-1<|>-1<|>-1<|>-1<|>-1<|>-1",
                )
                .unwrap(),
            ],
            items: vec![
                LegacyItemRecord::from_raw(
                    "Item",
                    "Notes.txt<|>101<|>0<|>4<|>C:\\Users\\Demo\\Desktop\\Notes.txt<|>0",
                )
                .unwrap(),
                LegacyItemRecord::from_raw(
                    "Item",
                    "CloudItem<|>1000000<|>0<|>4<|>C:\\Users\\Demo\\Cloud\\Doc.txt<|>0",
                )
                .unwrap(),
            ],
        };

        let cipher = encrypt_fences_bytes(&model);
        let reparsed = decrypt_fences_bytes(&cipher).unwrap();
        assert_eq!(reparsed, model);
        assert_eq!(build_fences_xml(&reparsed), build_fences_xml(&model));
        assert!(build_fences_xml(&model).ends_with("</root>\n\n\0"));

        let mut stripped = reparsed;
        stripped.strip_unknown_boxes();
        assert_eq!(stripped.boxes.len(), 1);
        assert_eq!(stripped.boxes[0].rect, Rect::new(2286, 10, 2550, 238));
        assert_eq!(stripped.screens.len(), 1);
        assert_eq!(stripped.items.len(), 1);
    }
}
