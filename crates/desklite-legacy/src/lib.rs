pub mod con_file;
pub mod convert;
pub mod fences_xml;
pub mod rc4;
pub mod records;

use thiserror::Error;

#[derive(Debug, Error)]
pub enum LegacyError {
    #[error("core store error: {0}")]
    Store(#[from] desklite_core::StoreError),
    #[error("topology error: {0}")]
    Topology(#[from] desklite_core::TopologyError),
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("UTF-8 decode error in FencesDataFile: {0}")]
    Utf8(#[from] std::string::FromUtf8Error),
    #[error("XML structure error: {0}")]
    Xml(String),
    #[error("record parse error: {0}")]
    Record(String),
    #[error("CFB error: {0}")]
    Cfb(String),
}

pub use con_file::{ConFile, read_con_file, write_con_file};
pub use convert::{
    LegacyBundle, LegacyFixReport, LegacyOptions, legacy_to_native, load_legacy_dir,
    repair_legacy_dir, save_legacy_dir,
};
pub use fences_xml::{
    FencesXmlModel, build_fences_xml, decrypt_fences_bytes, encrypt_fences_bytes,
    parse_fences_xml,
};
pub use rc4::{DESKGO_RC4_KEY, rc4_apply};
pub use records::{LegacyBoxRecord, LegacyItemRecord, LegacyScreenRecord};
