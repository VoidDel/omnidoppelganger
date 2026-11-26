//! The text of documents the model cannot read as they are: PDF, Word,
//! Excel, and PowerPoint files. Used by `read_file`, by files attached to a
//! message, and by `fetch_url`.
//!
//! Only the words (and a spreadsheet's values) are taken out; pictures,
//! charts, formatting, and layout are not kept. What a file is, is told by
//! what it holds, not by its name, except for the old binary Office formats,
//! which all start alike. Of those, an old spreadsheet (.xls) is read; an old
//! Word or PowerPoint file (.doc, .ppt) is refused, with how to make it readable.

use std::collections::VecDeque;
use std::io::{Read, Seek};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime};

use serde::{Deserialize, Serialize};

/// The largest document read: manuals and decks of pictures run to tens of megabytes, far more than their words.
pub const DOCUMENT_BYTES: u64 = 50 * 1024 * 1024;
/// Maximum number of ZIP central-directory entries inspected for an Office/OpenDocument archive.
pub const MAX_ZIP_ENTRIES: usize = 20_000;
/// The most text kept of one document: a read shows a part of it at a time, but all of it is held while it is read.
pub(crate) const TEXT_BYTES: usize = 20 * 1024 * 1024;

/// What kind of document a file is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Pdf,
    Word,
    Excel,
    PowerPoint,
}

/// The words of a document: of what kind it is, how many pages, sheets, or
/// slides it has (none counted for Word, whose pages are made when it is
/// laid out), and the text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Extracted {
    pub kind: Kind,
    pub parts: Option<usize>,
    pub text: String,
}

/// `1 page`, `3 pages`.
fn count(number: usize, one: &str) -> String {
    if number == 1 {
        format!("1 {one}")
    } else {
        format!("{number} {one}s")
    }
}

/// What a document is, in a few words: `a PDF of 3 pages`, `a Word document`.
pub fn name(kind: Kind, parts: Option<usize>) -> String {
    let parts = parts.unwrap_or(0);
    match kind {
        Kind::Pdf => format!("a PDF of {}", count(parts, "page")),
        Kind::Word => "a Word document".to_string(),
        Kind::Excel => format!("a spreadsheet of {}", count(parts, "sheet")),
        Kind::PowerPoint => format!("a presentation of {}", count(parts, "slide")),
    }
}

/// What a document is, and how its text was taken out of it and is laid out, for the model.
pub fn describe(kind: Kind, parts: Option<usize>) -> String {
    let how = match kind {
        Kind::Pdf => "This is its text, taken out page by page. Its drawings, pictures, and layout are not kept.",
        Kind::Word => {
            "This is its text: paragraphs in order, headings marked with #, list items with -, and tables as rows of cells split by |. Its pictures, charts, and formatting are not kept."
        }
        Kind::Excel => {
            "These are the values in its cells as last calculated, not their formulas: each sheet under a line that names it and the rows and columns it fills, then a row to a line, starting with the row's number, its cells split by tabs. Charts and formatting are not kept."
        }
        Kind::PowerPoint => {
            "This is the text on each slide, in the order they are shown, with the speaker's notes after it. Pictures, charts, and layout are not kept."
        }
    };
    format!("{}. {how}", name(kind, parts))
}

impl Extracted {
    /// See [`describe`].
    pub fn describe(&self) -> String {
        describe(self.kind, self.parts)
    }
}

/// Why a document's words could not be had.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DocError {
    /// Not a document this can make sense of: damaged, cut short, or of a kind not understood.
    Unreadable,
    /// Locked with a password.
    Locked,
    /// Nothing to read in it: a PDF of scanned pages, a document of pictures only.
    NoText,
    /// An old binary Word or PowerPoint file (.doc, .ppt), which is not read here.
    OldFormat,
}

impl DocError {
    /// The word for it among the reasons an attachment is refused; see [`crate::events::Picked`].
    pub fn word(self) -> &'static str {
        match self {
            DocError::Unreadable => "doc_unreadable",
            DocError::Locked => "doc_locked",
            DocError::NoText => "doc_no_text",
            DocError::OldFormat => "doc_old_format",
        }
    }

    /// What it means, for the model, after the document's name.
    pub fn describe(self) -> &'static str {
        match self {
            DocError::Unreadable => "is a document whose text cannot be read: it may be damaged or cut short",
            DocError::Locked => "is a document locked with a password, so its text cannot be read",
            DocError::NoText => {
                "is a document with no text in it: a PDF of scanned pages, or pictures only, cannot be read here"
            }
            DocError::OldFormat => {
                "is a Word or PowerPoint file in the old binary format (.doc, .ppt), which cannot be read here; saved as .docx or .pptx, it could be"
            }
        }
    }
}

const ZIP: &[u8] = b"PK\x03\x04";
/// How every old binary Office file starts, and an Office file locked with a password too.
const OLE: &[u8] = &[0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1];

/// What kind of document a zip is, by the parts it holds; `None` for any other zip.
/// Archives exceeding [`MAX_ZIP_ENTRIES`] are rejected without further inspection.
fn zipped<R: Read + Seek>(mut archive: zip::ZipArchive<R>) -> Option<Result<Kind, DocError>> {
    if archive.len() > MAX_ZIP_ENTRIES {
        let has_office = archive.index_for_name("word/document.xml").is_some()
            || archive.index_for_name("xl/workbook.xml").is_some()
            || archive.index_for_name("xl/workbook.bin").is_some()
            || archive.index_for_name("ppt/presentation.xml").is_some();
        return has_office.then_some(Err(DocError::Unreadable));
    }
    let has = |name: &str| archive.index_for_name(name).is_some();
    if has("word/document.xml") {
        return Some(Ok(Kind::Word));
    }
    if has("xl/workbook.xml") || has("xl/workbook.bin") {
        return Some(Ok(Kind::Excel));
    }
    if has("ppt/presentation.xml") {
        return Some(Ok(Kind::PowerPoint));
    }
    // An OpenDocument file names its kind first thing; only its spreadsheets are read here.
    let mut kind = String::new();
    archive
        .by_name("mimetype")
        .ok()?
        .take(100)
        .read_to_string(&mut kind)
        .ok()?;
    (kind.trim() == "application/vnd.oasis.opendocument.spreadsheet").then_some(Ok(Kind::Excel))
}

/// Fast header + extension classification for files exceeding [`DOCUMENT_BYTES`] without opening `ZipArchive`.
pub fn sniff_oversized_kind(path: &Path) -> Option<Kind> {
    let mut head = [0u8; 1024];
    let read = std::fs::File::open(path)
        .and_then(|mut file| file.read(&mut head))
        .ok()?;
    let head = &head[..read];
    if crate::pdf::is_pdf(head) {
        Some(Kind::Pdf)
    } else if head.starts_with(ZIP) {
        match path.extension().and_then(|e| e.to_str())?.to_ascii_lowercase().as_str() {
            "docx" | "docm" => Some(Kind::Word),
            "xlsx" | "xlsm" | "xlsb" | "ods" => Some(Kind::Excel),
            "pptx" | "pptm" => Some(Kind::PowerPoint),
            _ => None,
        }
    } else if head.starts_with(OLE) {
        binary_office(&path.to_string_lossy()).and_then(Result::ok)
    } else {
        None
    }
}

/// What an old binary Office file is, by its name's ending, which is all that tells them apart.
fn binary_office(name: &str) -> Option<Result<Kind, DocError>> {
    let ending = name.rsplit_once('.').map(|(_, ending)| ending.to_ascii_lowercase())?;
    match ending.as_str() {
        "xls" | "xlt" => Some(Ok(Kind::Excel)),
        "doc" | "dot" | "ppt" | "pps" | "pot" => Some(Err(DocError::OldFormat)),
        // A file of the new formats locked with a password is wrapped in the old one.
        "docx" | "docm" | "xlsx" | "xlsm" | "pptx" | "pptm" => Some(Err(DocError::Locked)),
        _ => None,
    }
}

/// What kind of document the file at `path` is: `None` when it is not one of
/// these kinds, `Some(Err)` when it is one that cannot be read.
pub fn kind_of_file(path: &Path) -> Option<Result<Kind, DocError>> {
    std::panic::catch_unwind(|| {
        let meta = std::fs::metadata(path).ok()?;
        if meta.len() > DOCUMENT_BYTES {
            return sniff_oversized_kind(path).map(Ok);
        }
        let mut head = [0u8; 1024];
        let read = std::fs::File::open(path)
            .and_then(|mut file| file.read(&mut head))
            .ok()?;
        let head = &head[..read];
        if crate::pdf::is_pdf(head) {
            Some(Ok(Kind::Pdf))
        } else if head.starts_with(ZIP) {
            zipped(zip::ZipArchive::new(std::io::BufReader::new(std::fs::File::open(path).ok()?)).ok()?)
        } else if head.starts_with(OLE) {
            binary_office(&path.to_string_lossy())
        } else {
            None
        }
    })
    .unwrap_or(Some(Err(DocError::Unreadable)))
}

/// [`kind_of_file`] for a download, with `name` the address it came from, for its ending.
pub fn kind_of_bytes(bytes: &[u8], name: &str) -> Option<Result<Kind, DocError>> {
    std::panic::catch_unwind(|| {
        if crate::pdf::is_pdf(bytes) {
            Some(Ok(Kind::Pdf))
        } else if bytes.starts_with(ZIP) {
            zipped(zip::ZipArchive::new(std::io::Cursor::new(bytes)).ok()?)
        } else if bytes.starts_with(OLE) {
            binary_office(name.split(['?', '#']).next().unwrap_or(name))
        } else {
            None
        }
    })
    .unwrap_or(Some(Err(DocError::Unreadable)))
}

/// The words of a document of this kind.
pub fn text(bytes: &[u8], kind: Kind) -> Result<Extracted, DocError> {
    // The readers are new to some files they are given, and may panic on one: that is a file they cannot read, not a crash.
    // On Windows the default thread stack is only 1 MB, which deeply nested PDFs can overflow in `lopdf` / `pdf-extract`,
    // so PDF extraction runs on an 8 MB scoped worker thread.
    let read: Result<(Option<usize>, String), DocError> = if kind == Kind::Pdf {
        std::thread::scope(|scope| {
            std::thread::Builder::new()
                .name("odg-pdf".to_string())
                .stack_size(8 * 1024 * 1024)
                .spawn_scoped(scope, || {
                    std::panic::catch_unwind(|| crate::pdf::read(bytes).map(|(pages, text)| (Some(pages), text)))
                        .unwrap_or(Err(DocError::Unreadable))
                })
                .ok()
                .and_then(|handle| handle.join().ok())
                .unwrap_or(Err(DocError::Unreadable))
        })
    } else {
        std::panic::catch_unwind(|| match kind {
            Kind::Pdf => unreachable!(),
            Kind::Word => crate::office::word(bytes).map(|text| (None, text)),
            Kind::Excel => crate::office::sheets(bytes).map(|(sheets, text)| (Some(sheets), text)),
            Kind::PowerPoint => crate::office::slides(bytes).map(|(slides, text)| (Some(slides), text)),
        })
        .unwrap_or(Err(DocError::Unreadable))
    };
    let (parts, mut text) = read?;
    if text.trim().is_empty() {
        return Err(DocError::NoText);
    }
    if text.len() > TEXT_BYTES {
        let mut end = TEXT_BYTES;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        text.truncate(end);
        text.push_str(&format!(
            "\n[the rest is cut: the text runs past {} MB]",
            TEXT_BYTES / 1024 / 1024
        ));
    }
    Ok(Extracted { kind, parts, text })
}

const DOC_CACHE_SLOTS: usize = 4;
const DOC_CACHE_MAX_SLOT_BYTES: usize = 8 * 1024 * 1024;
const DOC_CACHE_IDLE_TTL: Duration = Duration::from_secs(5 * 60);

struct CachedDocument {
    path: PathBuf,
    bytes: u64,
    modified: Option<SystemTime>,
    extracted: Arc<Extracted>,
    last_accessed: Instant,
}

#[derive(Default)]
struct DocumentLruCache {
    entries: VecDeque<CachedDocument>,
}

impl DocumentLruCache {
    fn prune_expired(&mut self, now: Instant) {
        self.entries
            .retain(|entry| now.saturating_duration_since(entry.last_accessed) <= DOC_CACHE_IDLE_TTL);
    }

    fn get(
        &mut self,
        path: &Path,
        bytes: u64,
        modified: Option<SystemTime>,
        kind: Kind,
        now: Instant,
    ) -> Option<Arc<Extracted>> {
        self.prune_expired(now);
        modified?;
        let pos = self.entries.iter().position(|entry| {
            entry.path == path && entry.bytes == bytes && entry.modified == modified && entry.extracted.kind == kind
        })?;
        let mut hit = self.entries.remove(pos)?;
        hit.last_accessed = now;
        let out = hit.extracted.clone();
        self.entries.push_back(hit);
        Some(out)
    }

    fn insert(
        &mut self,
        path: PathBuf,
        bytes: u64,
        modified: Option<SystemTime>,
        extracted: Arc<Extracted>,
        now: Instant,
    ) {
        self.prune_expired(now);
        self.entries.retain(|entry| entry.path != path);
        if modified.is_none() || extracted.text.len() > DOC_CACHE_MAX_SLOT_BYTES {
            return;
        }
        while self.entries.len() >= DOC_CACHE_SLOTS {
            self.entries.pop_front();
        }
        self.entries.push_back(CachedDocument {
            path,
            bytes,
            modified,
            extracted,
            last_accessed: now,
        });
    }
}

fn doc_cache() -> &'static Mutex<DocumentLruCache> {
    static CACHE: OnceLock<Mutex<DocumentLruCache>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(DocumentLruCache::default()))
}

/// The words of the document at `path`, of this kind, which must be
/// [`DOCUMENT_BYTES`] or smaller. `Err` carries what to tell of a file that
/// cannot be read at all. Cached in a bounded 4-slot, <= 8 MB/slot, 5-minute idle TTL LRU.
pub fn file_text(path: &Path, kind: Kind) -> Result<Result<Arc<Extracted>, DocError>, std::io::Error> {
    let metadata = std::fs::metadata(path)?;
    let (bytes, modified) = (metadata.len(), metadata.modified().ok());
    let now = Instant::now();
    if let Some(hit) = doc_cache()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .get(path, bytes, modified, kind, now)
    {
        return Ok(Ok(hit));
    }
    let read = text(&std::fs::read(path)?, kind).map(Arc::new);
    if let Ok(extracted) = &read {
        doc_cache()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(path.to_path_buf(), bytes, modified, extracted.clone(), Instant::now());
    }
    Ok(read)
}

#[cfg(test)]
pub(crate) mod tests {
    use std::io::Write;

    use super::*;

    /// A zip of these parts, as an Office file is.
    pub(crate) fn package(parts: &[(&str, &str)]) -> Vec<u8> {
        let mut zipped = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        for (name, content) in parts {
            zipped
                .start_file(*name, zip::write::SimpleFileOptions::default())
                .unwrap();
            zipped.write_all(content.as_bytes()).unwrap();
        }
        zipped.finish().unwrap().into_inner()
    }

    const W: &str = r#"xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" xmlns:mc="http://schemas.openxmlformats.org/markup-compatibility/2006""#;

    /// A Word document of this body, with the heading styles a Chinese template has: ids that are numbers.
    pub(crate) fn word_document(body: &str) -> Vec<u8> {
        let styles = format!(
            r#"<w:styles {W}><w:style w:styleId="1"><w:name w:val="heading 1"/></w:style><w:style w:styleId="2"><w:name w:val="heading 2"/></w:style><w:style w:styleId="a3"><w:name w:val="Normal"/></w:style></w:styles>"#
        );
        package(&[
            (
                "word/document.xml",
                &format!(r#"<w:document {W}><w:body>{body}</w:body></w:document>"#),
            ),
            ("word/styles.xml", &styles),
        ])
    }

    /// A paragraph of Word, with a style or not.
    pub(crate) fn paragraph(style: Option<&str>, words: &str) -> String {
        let style = style
            .map(|id| format!(r#"<w:pPr><w:pStyle w:val="{id}"/></w:pPr>"#))
            .unwrap_or_default();
        format!("<w:p>{style}<w:r><w:t xml:space=\"preserve\">{words}</w:t></w:r></w:p>")
    }

    #[test]
    fn reads_a_word_document_as_paragraphs_headings_lists_and_tables() {
        let cell = |words: &str| format!("<w:tc>{}</w:tc>", paragraph(None, words));
        let body = [
            paragraph(Some("1"), "电源设计"),
            paragraph(None, "U8 降压到 5.10 V &amp; 给 U6 供电。"),
            r#"<w:p><w:pPr><w:numPr><w:ilvl w:val="0"/></w:numPr><w:tabs><w:tab w:val="left" w:pos="420"/></w:tabs></w:pPr><w:r><w:t>第一项</w:t><w:tab/><w:t>附注</w:t></w:r></w:p>"#.to_string(),
            paragraph(None, ""),
            format!("<w:tbl><w:tr>{}{}</w:tr><w:tr>{}{}</w:tr></w:tbl>", cell("位号"), cell("器件"), cell("U6"), cell("TLV1117 | 3.3 V")),
            paragraph(Some("2"), "小结"),
            // The same picture's words twice: the new way, and the old way after it for old readers.
            r#"<w:p><w:r><mc:AlternateContent><mc:Choice><w:t>once</w:t></mc:Choice><mc:Fallback><w:t>twice</w:t></mc:Fallback></mc:AlternateContent></w:r></w:p>"#.to_string(),
        ]
        .concat();
        let bytes = word_document(&body);
        assert_eq!(kind_of_bytes(&bytes, "x"), Some(Ok(Kind::Word)));
        let read = text(&bytes, Kind::Word).unwrap();
        assert_eq!(read.parts, None);
        assert_eq!(
            read.text,
            "# 电源设计\nU8 降压到 5.10 V & 给 U6 供电。\n- 第一项\t附注\n\n| 位号 | 器件 |\n| U6 | TLV1117 \\| 3.3 V |\n\n## 小结\nonce"
        );
    }

    #[test]
    fn reads_the_slides_of_a_presentation_in_the_order_they_are_shown() {
        const P: &str = r#"xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main" xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships""#;
        let rels = |links: &[(&str, &str, &str)]| {
            let links: String = links.iter().map(|(id, kind, target)| format!(r#"<Relationship Id="{id}" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/{kind}" Target="{target}"/>"#)).collect();
            format!(
                r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">{links}</Relationships>"#
            )
        };
        let words = |lines: &[&str]| -> String {
            lines
                .iter()
                .map(|line| format!("<a:p><a:r><a:t>{line}</a:t></a:r></a:p>"))
                .collect()
        };
        let slide = |attributes: &str, body: &str| {
            format!(
                r#"<p:sld {P}{attributes}><p:cSld><p:spTree><p:sp><p:txBody>{body}</p:txBody></p:sp></p:spTree></p:cSld></p:sld>"#
            )
        };
        // Two slides, listed in the other order than their files are numbered; the second with notes, the first hidden.
        let bytes = package(&[
            (
                "ppt/presentation.xml",
                &format!(
                    r#"<p:presentation {P}><p:sldIdLst><p:sldId id="256" r:id="rId3"/><p:sldId id="257" r:id="rId2"/></p:sldIdLst></p:presentation>"#
                ),
            ),
            (
                "ppt/_rels/presentation.xml.rels",
                &rels(&[
                    ("rId2", "slide", "slides/slide1.xml"),
                    ("rId3", "slide", "slides/slide2.xml"),
                ]),
            ),
            (
                "ppt/slides/slide1.xml",
                &slide(
                    "",
                    &format!(
                        "{}<a:p><a:fld type=\"slidenum\"><a:t>2</a:t></a:fld></a:p>",
                        words(&["方案对比", "成本 &lt; 50 元"])
                    ),
                ),
            ),
            (
                "ppt/slides/_rels/slide1.xml.rels",
                &rels(&[("rId1", "notesSlide", "../notesSlides/notesSlide1.xml")]),
            ),
            (
                "ppt/notesSlides/notesSlide1.xml",
                &format!(
                    r#"<p:notes {P}><p:cSld><p:spTree><p:sp><p:txBody>{}</p:txBody></p:sp></p:spTree></p:cSld></p:notes>"#,
                    words(&["讲到这里停一下"])
                ),
            ),
            ("ppt/slides/slide2.xml", &slide(r#" show="0""#, &words(&["封面"]))),
        ]);
        assert_eq!(kind_of_bytes(&bytes, "x"), Some(Ok(Kind::PowerPoint)));
        let read = text(&bytes, Kind::PowerPoint).unwrap();
        assert_eq!(read.parts, Some(2));
        assert_eq!(
            read.text,
            "[slide 1 of 2, hidden]\n封面\n\n[slide 2 of 2]\n方案对比\n成本 < 50 元\nNotes: 讲到这里停一下"
        );
    }

    #[test]
    fn reads_a_spreadsheet_as_the_values_of_its_rows() {
        // The smallest workbook Excel opens: one sheet, its strings shared, and a date by its style.
        const S: &str = r#"xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships""#;
        let bytes = package(&[
            (
                "[Content_Types].xml",
                r#"<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/><Override PartName="/xl/worksheets/sheet1.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/></Types>"#,
            ),
            (
                "_rels/.rels",
                r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="xl/workbook.xml"/></Relationships>"#,
            ),
            (
                "xl/workbook.xml",
                &format!(r#"<workbook {S}><sheets><sheet name="物料" sheetId="1" r:id="rId1"/></sheets></workbook>"#),
            ),
            (
                "xl/_rels/workbook.xml.rels",
                r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/><Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/sharedStrings" Target="sharedStrings.xml"/><Relationship Id="rId3" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles" Target="styles.xml"/></Relationships>"#,
            ),
            (
                "xl/sharedStrings.xml",
                &format!(r#"<sst {S}><si><t>位号</t></si><si><t>数量</t></si><si><t>R22</t></si></sst>"#),
            ),
            (
                "xl/styles.xml",
                &format!(
                    r#"<styleSheet {S}><cellXfs count="2"><xf numFmtId="0"/><xf numFmtId="14" applyNumberFormat="1"/></cellXfs></styleSheet>"#
                ),
            ),
            (
                "xl/worksheets/sheet1.xml",
                &format!(
                    r#"<worksheet {S}><sheetData><row r="2"><c r="B2" t="s"><v>0</v></c><c r="C2" t="s"><v>1</v></c></row><row r="3"><c r="B3" t="s"><v>2</v></c><c r="C3"><v>4</v></c><c r="D3"><v>0.25</v></c><c r="E3" s="1"><v>46000</v></c></row></sheetData></worksheet>"#
                ),
            ),
        ]);
        assert_eq!(kind_of_bytes(&bytes, "x"), Some(Ok(Kind::Excel)));
        let read = text(&bytes, Kind::Excel).unwrap();
        assert_eq!(read.parts, Some(1));
        assert_eq!(
            read.text,
            "[sheet 1 of 1: 物料; rows 2-3, columns B-E]\n2\t位号\t数量\n3\tR22\t4\t0.25\t2025-12-09"
        );
    }

    #[test]
    fn tells_a_document_by_what_it_holds_and_an_old_one_by_its_name() {
        let pdf = crate::pdf::tests::sample(&[&["words"]]);
        assert_eq!(kind_of_bytes(&pdf, "download"), Some(Ok(Kind::Pdf)));
        // A zip of something else, plain text, and the old binary formats, which only their names tell apart.
        assert_eq!(kind_of_bytes(&package(&[("readme.txt", "hi")]), "a.docx"), None);
        assert_eq!(kind_of_bytes(b"plain words", "a.pdf"), None);
        let old = [OLE, &[0; 8]].concat();
        assert_eq!(kind_of_bytes(&old, "https://x/sheet.XLS?v=2"), Some(Ok(Kind::Excel)));
        assert_eq!(kind_of_bytes(&old, "report.doc"), Some(Err(DocError::OldFormat)));
        assert_eq!(kind_of_bytes(&old, "deck.pptx"), Some(Err(DocError::Locked)));
        assert_eq!(kind_of_bytes(&old, "thing.bin"), None);
        // A document of nothing to read, and one that is not whole.
        assert_eq!(
            text(&word_document(&paragraph(None, "")), Kind::Word),
            Err(DocError::NoText)
        );
        assert_eq!(
            text(
                &package(&[("word/document.xml", "<w:document><w:body><w:p>")]),
                Kind::Word
            ),
            Err(DocError::Unreadable)
        );
        assert_eq!(text(&old, Kind::Excel), Err(DocError::Unreadable));
    }

    #[test]
    fn reads_a_file_again_only_when_it_has_changed() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("notes.docx");
        std::fs::write(&path, word_document(&paragraph(None, "first edition"))).unwrap();
        assert_eq!(kind_of_file(&path), Some(Ok(Kind::Word)));
        assert!(
            file_text(&path, Kind::Word)
                .unwrap()
                .unwrap()
                .text
                .contains("first edition")
        );
        std::fs::write(&path, word_document(&paragraph(None, "second edition, longer"))).unwrap();
        assert!(
            file_text(&path, Kind::Word)
                .unwrap()
                .unwrap()
                .text
                .contains("second edition")
        );
        assert!(file_text(&directory.path().join("gone.docx"), Kind::Word).is_err());
        assert_eq!(kind_of_file(&directory.path().join("gone.docx")), None);
    }

    #[test]
    fn document_lru_cache_enforces_four_slots_eight_mb_cap_and_five_minute_ttl() {
        let mut cache = DocumentLruCache::default();
        let t0 = Instant::now();
        let mtime = Some(SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000));
        let make_doc = |label: &str, bytes_len: usize| {
            Arc::new(Extracted {
                kind: Kind::Word,
                parts: None,
                text: if bytes_len <= label.len() {
                    label.to_string()
                } else {
                    format!("{label}{}", "x".repeat(bytes_len - label.len()))
                },
            })
        };

        // > 8 MB extracted text is never retained in cache.
        cache.insert(
            PathBuf::from("/d/huge.docx"),
            100,
            mtime,
            make_doc("huge", DOC_CACHE_MAX_SLOT_BYTES + 1),
            t0,
        );
        assert!(
            cache
                .get(Path::new("/d/huge.docx"), 100, mtime, Kind::Word, t0)
                .is_none()
        );

        // Fill 4 slots, touch `/d/1.docx` so `/d/2.docx` becomes LRU, then insert `/d/5.docx`.
        for i in 1..=4 {
            cache.insert(
                PathBuf::from(format!("/d/{i}.docx")),
                10,
                mtime,
                make_doc(&format!("doc-{i}"), 8),
                t0 + Duration::from_secs(i),
            );
        }
        assert!(
            cache
                .get(
                    Path::new("/d/1.docx"),
                    10,
                    mtime,
                    Kind::Word,
                    t0 + Duration::from_secs(5)
                )
                .is_some()
        );
        cache.insert(
            PathBuf::from("/d/5.docx"),
            10,
            mtime,
            make_doc("doc-5", 8),
            t0 + Duration::from_secs(6),
        );

        assert!(
            cache
                .get(
                    Path::new("/d/2.docx"),
                    10,
                    mtime,
                    Kind::Word,
                    t0 + Duration::from_secs(7)
                )
                .is_none(),
            "LRU slot 2 should be evicted"
        );
        for kept in ["/d/1.docx", "/d/3.docx", "/d/4.docx", "/d/5.docx"] {
            assert!(
                cache
                    .get(Path::new(kept), 10, mtime, Kind::Word, t0 + Duration::from_secs(7))
                    .is_some(),
                "{kept} should remain cached"
            );
        }

        // Idle past 5 minutes expires all untouched entries.
        let expired_at = t0 + Duration::from_secs(7) + DOC_CACHE_IDLE_TTL + Duration::from_secs(1);
        assert!(
            cache
                .get(Path::new("/d/5.docx"), 10, mtime, Kind::Word, expired_at)
                .is_none()
        );
    }
}
