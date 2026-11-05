//! The database text encoding (fileformat2 §1.3.13, header offset 56) as read from
//! the database header. The [`TextEncoding`] type itself (and the UTF-16 <-> UTF-8
//! conversion) lives in `minisqlite-types` so the expression evaluator can apply the
//! same encoding to TEXT <-> BLOB conversions; it is re-exported here so the record
//! codec and its callers keep naming it from this crate.

use crate::header::DatabaseHeader;
pub use minisqlite_types::TextEncoding;

impl DatabaseHeader {
    /// This database's text encoding as a typed [`TextEncoding`] (offset 56, §1.3.13).
    /// The one place the raw `text_encoding` code is interpreted, so the read path can
    /// thread a single value instead of re-deciding the mapping at each decode site.
    pub fn text_encoding_kind(&self) -> TextEncoding {
        TextEncoding::from_code(self.text_encoding)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::header::{TEXT_ENCODING_UTF16BE, TEXT_ENCODING_UTF16LE};

    #[test]
    fn header_kind_reads_offset_56() {
        let mut h = DatabaseHeader::default();
        assert_eq!(h.text_encoding_kind(), TextEncoding::Utf8);
        h.text_encoding = TEXT_ENCODING_UTF16LE;
        assert_eq!(h.text_encoding_kind(), TextEncoding::Utf16le);
        h.text_encoding = TEXT_ENCODING_UTF16BE;
        assert_eq!(h.text_encoding_kind(), TextEncoding::Utf16be);
    }
}
