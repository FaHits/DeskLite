use desklite_core::Rect;

use crate::LegacyError;

pub const SEP: &str = "<|>";
pub const BOX_CON_FIELDS: usize = 11;
pub const BOX_XML_FIELDS: usize = 24;
pub const ITEM_CON_FIELDS: usize = 5;
pub const ITEM_XML_FIELDS: usize = 6;
pub const SCREEN_XML_FIELDS: usize = 17;

fn parse_i32(raw: &str, label: &str) -> Result<i32, LegacyError> {
    raw.parse::<i32>()
        .map_err(|e| LegacyError::Record(format!("invalid i32 for {label} ({raw:?}): {e}")))
}

fn parse_u32(raw: &str, label: &str) -> Result<u32, LegacyError> {
    raw.parse::<u32>()
        .map_err(|e| LegacyError::Record(format!("invalid u32 for {label} ({raw:?}): {e}")))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LegacyBoxRecord {
    pub tag: String,
    pub id: i32,
    pub name: String,
    pub rect: Rect,
    pub reserved_6_7: [i32; 2],
    pub kind: i32,
    pub reserved_9_22: [i32; 14],
    pub tail: String,
}

impl LegacyBoxRecord {
    pub fn from_raw(tag: impl Into<String>, raw: &str) -> Result<Self, LegacyError> {
        let mut fields: Vec<&str> = raw.split(SEP).collect();
        while fields.len() < BOX_XML_FIELDS {
            if fields.len() < BOX_XML_FIELDS - 1 {
                fields.push("0");
            } else {
                fields.push("");
            }
        }
        let id = parse_i32(fields[0], "box.id")?;
        let name = fields[1].to_string();
        let rect = Rect::new(
            parse_i32(fields[2], "box.left")?,
            parse_i32(fields[3], "box.top")?,
            parse_i32(fields[4], "box.right")?,
            parse_i32(fields[5], "box.bottom")?,
        );
        let reserved_6_7 = [
            parse_i32(fields[6], "box.f6")?,
            parse_i32(fields[7], "box.f7")?,
        ];
        let kind = parse_i32(fields[8], "box.kind")?;
        let mut reserved_9_22 = [0i32; 14];
        for (idx, slot) in reserved_9_22.iter_mut().enumerate() {
            *slot = parse_i32(fields[9 + idx], &format!("box.f{}", 9 + idx))?;
        }
        let tail = fields[23].to_string();
        Ok(Self {
            tag: tag.into(),
            id,
            name,
            rect,
            reserved_6_7,
            kind,
            reserved_9_22,
            tail,
        })
    }

    pub fn all_fields(&self) -> Vec<String> {
        let mut out = Vec::with_capacity(BOX_XML_FIELDS);
        out.push(self.id.to_string());
        out.push(self.name.clone());
        out.push(self.rect.left.to_string());
        out.push(self.rect.top.to_string());
        out.push(self.rect.right.to_string());
        out.push(self.rect.bottom.to_string());
        out.push(self.reserved_6_7[0].to_string());
        out.push(self.reserved_6_7[1].to_string());
        out.push(self.kind.to_string());
        for v in self.reserved_9_22 {
            out.push(v.to_string());
        }
        out.push(self.tail.clone());
        out
    }

    pub fn to_xml_record(&self) -> String {
        self.all_fields().join(SEP)
    }

    pub fn to_con_record(&self) -> String {
        self.all_fields()[..BOX_CON_FIELDS].join(SEP)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LegacyItemRecord {
    pub tag: String,
    pub display_name: String,
    pub box_id: i32,
    pub order: i32,
    pub kind: u32,
    pub path: String,
    pub reserved: i32,
}

impl LegacyItemRecord {
    pub fn from_raw(tag: impl Into<String>, raw: &str) -> Result<Self, LegacyError> {
        let mut fields: Vec<&str> = raw.split(SEP).collect();
        while fields.len() < ITEM_XML_FIELDS {
            fields.push("0");
        }
        Ok(Self {
            tag: tag.into(),
            display_name: fields[0].to_string(),
            box_id: parse_i32(fields[1], "item.box_id")?,
            order: parse_i32(fields[2], "item.order")?,
            kind: parse_u32(fields[3], "item.kind")?,
            path: fields[4].to_string(),
            reserved: parse_i32(fields[5], "item.reserved")?,
        })
    }

    pub fn all_fields(&self) -> Vec<String> {
        vec![
            self.display_name.clone(),
            self.box_id.to_string(),
            self.order.to_string(),
            self.kind.to_string(),
            self.path.clone(),
            self.reserved.to_string(),
        ]
    }

    pub fn to_xml_record(&self) -> String {
        self.all_fields().join(SEP)
    }

    pub fn to_con_record(&self) -> String {
        self.all_fields()[..ITEM_CON_FIELDS].join(SEP)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LegacyScreenRecord {
    pub tag: String,
    pub box_id: i32,
    pub monitor: String,
    pub unknown_2_3: [i32; 2],
    pub screen_w: i32,
    pub screen_h: i32,
    pub unknown_6_7: [i32; 2],
    pub work_w: i32,
    pub work_h: i32,
    pub unknown_10: i32,
    pub reserved_11_16: [i32; 6],
}

impl LegacyScreenRecord {
    pub fn from_raw(tag: impl Into<String>, raw: &str) -> Result<Self, LegacyError> {
        let mut fields: Vec<&str> = raw.split(SEP).collect();
        while fields.len() < SCREEN_XML_FIELDS {
            fields.push("-1");
        }
        let mut reserved_11_16 = [-1i32; 6];
        for (idx, slot) in reserved_11_16.iter_mut().enumerate() {
            *slot = parse_i32(fields[11 + idx], &format!("screen.f{}", 11 + idx))?;
        }
        Ok(Self {
            tag: tag.into(),
            box_id: parse_i32(fields[0], "screen.box_id")?,
            monitor: fields[1].to_string(),
            unknown_2_3: [
                parse_i32(fields[2], "screen.f2")?,
                parse_i32(fields[3], "screen.f3")?,
            ],
            screen_w: parse_i32(fields[4], "screen.w")?,
            screen_h: parse_i32(fields[5], "screen.h")?,
            unknown_6_7: [
                parse_i32(fields[6], "screen.f6")?,
                parse_i32(fields[7], "screen.f7")?,
            ],
            work_w: parse_i32(fields[8], "screen.work_w")?,
            work_h: parse_i32(fields[9], "screen.work_h")?,
            unknown_10: parse_i32(fields[10], "screen.f10")?,
            reserved_11_16,
        })
    }

    pub fn to_xml_record(&self) -> String {
        let mut out = Vec::with_capacity(SCREEN_XML_FIELDS);
        out.push(self.box_id.to_string());
        out.push(self.monitor.clone());
        out.push(self.unknown_2_3[0].to_string());
        out.push(self.unknown_2_3[1].to_string());
        out.push(self.screen_w.to_string());
        out.push(self.screen_h.to_string());
        out.push(self.unknown_6_7[0].to_string());
        out.push(self.unknown_6_7[1].to_string());
        out.push(self.work_w.to_string());
        out.push(self.work_h.to_string());
        out.push(self.unknown_10.to_string());
        for v in self.reserved_11_16 {
            out.push(v.to_string());
        }
        out.join(SEP)
    }
}
