//! The words of Office documents: Word (.docx), PowerPoint (.pptx), and
//! spreadsheets (.xlsx, .xlsm, .xlsb, .xls, .ods). See [`crate::documents`],
//! which these are kinds of.
//!
//! A .docx or .pptx is a zip of XML parts; the words are streamed out of those
//! via `BufReader` + `quick_xml` without materializing huge XML DOM/strings in
//! memory. Spreadsheets stream sparse cells via `worksheet_cells_reader` for
//! `.xlsx`/`.xlsb` so sparse `A1`..`XFD1048576` sheets never allocate dense matrices.

use std::collections::{BTreeMap, HashMap};
use std::io::{BufRead, BufReader, Cursor, Read};
use std::ops::ControlFlow;

use quick_xml::Reader;
use quick_xml::events::{BytesStart, Event};
use zip::ZipArchive;

use crate::documents::{DocError, MAX_ZIP_ENTRIES, TEXT_BYTES};

/// Maximum uncompressed size of a single XML/binary part inside an Office archive (64 MB).
const PART_BYTES: u64 = 64 * 1024 * 1024;
/// Maximum number of non-empty cells extracted from a workbook before stopping early.
const MAX_SHEET_CELLS: usize = 250_000;
/// Maximum dense bounding-box cell count allowed for legacy `.xls` / `.ods` sheets.
const MAX_DENSE_DIMENSION_CELLS: u64 = 2_000_000;

type Archive<'a> = ZipArchive<Cursor<&'a [u8]>>;

fn archive(bytes: &[u8]) -> Result<Archive<'_>, DocError> {
    let zip = ZipArchive::new(Cursor::new(bytes)).map_err(|_| DocError::Unreadable)?;
    if zip.len() > MAX_ZIP_ENTRIES {
        return Err(DocError::Unreadable);
    }
    Ok(zip)
}

/// The local part of an element's or attribute's name: `t` of `w:t`.
fn local(name: &str) -> &str {
    name.rsplit(':').next().unwrap_or(name)
}

/// An attribute's value, by its local name; with `prefixed`, only one written with a prefix (`r:id`, not `id`).
fn attribute(element: &BytesStart, name: &str, prefixed: bool) -> Option<String> {
    element
        .attributes()
        .with_checks(false)
        .flatten()
        .find(|found| local(found.key.0) == name && (!prefixed || found.key.0.contains(':')))
        .and_then(|found| {
            found
                .normalized_value(quick_xml::XmlVersion::Implicit1_0)
                .ok()
                .map(|value| value.into_owned())
        })
}

/// One step through an XML part: an element opened, one that is open and
/// closed at once, one closed (by its local name), or text.
#[derive(Clone, Copy)]
enum Step<'e> {
    Open(&'e BytesStart<'e>),
    Leaf(&'e BytesStart<'e>),
    Close(&'e str),
    Text(&'e str),
}

/// Stream through an XML reader step by step using a bounded scratch buffer,
/// skipping `mc:Fallback` subtrees and stopping early when `visit` returns `ControlFlow::Break(())`.
fn walk_reader<R: BufRead>(
    mut reader: Reader<R>,
    mut visit: impl FnMut(Step) -> ControlFlow<()>,
) -> Result<(), DocError> {
    let mut buf = Vec::with_capacity(4096);
    let mut skipping = 0usize;
    let mut open = 0usize;
    loop {
        buf.clear();
        let event = reader.read_event_into(&mut buf).map_err(|_| DocError::Unreadable)?;
        match &event {
            Event::Start(_) => open += 1,
            Event::End(_) => open = open.saturating_sub(1),
            _ => {}
        }
        let flow = match event {
            Event::Start(element) => {
                if skipping > 0 || local(element.name().0) == "Fallback" {
                    skipping += 1;
                    ControlFlow::Continue(())
                } else {
                    visit(Step::Open(&element))
                }
            }
            Event::End(element) => {
                if skipping > 0 {
                    skipping -= 1;
                    ControlFlow::Continue(())
                } else {
                    visit(Step::Close(local(element.name().0)))
                }
            }
            Event::Empty(element) if skipping == 0 => visit(Step::Leaf(&element)),
            Event::Text(text) if skipping == 0 => visit(Step::Text(&text.xml10_content())),
            Event::CData(text) if skipping == 0 => visit(Step::Text(&text.into_inner())),
            Event::GeneralRef(reference) if skipping == 0 => {
                let character = match reference.resolve_char_ref() {
                    Ok(Some(character)) => Some(character),
                    _ => match &*reference.into_inner() {
                        "amp" => Some('&'),
                        "lt" => Some('<'),
                        "gt" => Some('>'),
                        "quot" => Some('"'),
                        "apos" => Some('\''),
                        _ => None,
                    },
                };
                if let Some(character) = character {
                    visit(Step::Text(character.encode_utf8(&mut [0; 4])))
                } else {
                    ControlFlow::Continue(())
                }
            }
            Event::Eof if open > 0 => return Err(DocError::Unreadable),
            Event::Eof => return Ok(()),
            _ => ControlFlow::Continue(()),
        };
        if flow.is_break() {
            return Ok(());
        }
    }
}

/// Stream an XML part directly out of `archive`, bounded at [`PART_BYTES`] (64 MB).
/// Returns `Ok(false)` when `name` is absent from the archive.
fn walk_part(
    archive: &mut Archive,
    name: &str,
    visit: impl FnMut(Step) -> ControlFlow<()>,
) -> Result<bool, DocError> {
    let Ok(file) = archive.by_name(name) else {
        return Ok(false);
    };
    if file.size() > PART_BYTES {
        return Err(DocError::Unreadable);
    }
    let reader = Reader::from_reader(BufReader::new(file.take(PART_BYTES)));
    walk_reader(reader, visit)?;
    Ok(true)
}

/// How [`flow_part`] reads a part: Word's body, or the drawings of a slide.
struct Flow<'a> {
    /// Word's heading styles, by id, and the level of each: a paragraph in one is marked with as many `#`.
    headings: Option<&'a HashMap<String, usize>>,
    /// Keep empty paragraphs, which in Word part what is written; on a slide they are only room.
    blank_lines: bool,
}

/// Stream the words of an archive XML part, stopping early once `out.len() > TEXT_BYTES`.
fn flow_part(archive: &mut Archive, name: &str, how: &Flow) -> Result<Option<(bool, String)>, DocError> {
    let mut out = String::new();
    let mut hidden_slide = false;
    let mut paragraphs: Vec<(String, String)> = Vec::new();
    let mut cells: Vec<String> = Vec::new();
    let mut rows: Vec<Vec<String>> = Vec::new();
    let (mut in_run, mut in_text, mut in_number) = (false, false, false);

    fn put(out: &mut String, cells: &mut [String], line: &str) {
        match cells.last_mut() {
            Some(cell) if !line.is_empty() => {
                if !cell.is_empty() {
                    cell.push(' ');
                }
                cell.push_str(&line.replace('\n', " "));
            }
            Some(_) => {}
            None => {
                out.push_str(line);
                out.push('\n');
            }
        }
    }

    let existed = walk_part(archive, name, |step| {
        match step {
            Step::Open(element) | Step::Leaf(element) => {
                let open = matches!(step, Step::Open(_));
                match local(element.name().0) {
                    "sld" if open => {
                        hidden_slide = attribute(element, "show", false).is_some_and(|show| show == "0");
                    }
                    "p" if open => paragraphs.push((String::new(), String::new())),
                    "pStyle" => {
                        let level = attribute(element, "val", false).and_then(|id| how.headings?.get(&id).copied());
                        if let (Some(level), Some((mark, _))) = (level, paragraphs.last_mut()) {
                            *mark = format!("{} ", "#".repeat(level));
                        }
                    }
                    "numPr" => {
                        if let Some((mark, _)) = paragraphs.last_mut().filter(|(mark, _)| mark.is_empty()) {
                            *mark = "- ".to_string();
                        }
                    }
                    "r" if open => in_run = true,
                    "t" if open => in_text = true,
                    "fld" if open => in_number = attribute(element, "type", false).is_some_and(|kind| kind == "slidenum"),
                    "tab" if in_run => paragraphs.last_mut().into_iter().for_each(|(_, words)| words.push('\t')),
                    "br" | "cr" => paragraphs.last_mut().into_iter().for_each(|(_, words)| words.push('\n')),
                    "tr" if open => rows.push(Vec::new()),
                    "tc" if open => cells.push(String::new()),
                    _ => {}
                }
            }
            Step::Close(tag) => match tag {
                "r" => in_run = false,
                "t" => in_text = false,
                "fld" => in_number = false,
                "p" => {
                    let (mark, words) = paragraphs.pop().unwrap_or_default();
                    let words = words.trim();
                    if !words.is_empty() {
                        put(&mut out, &mut cells, &format!("{mark}{words}"));
                    } else if how.blank_lines {
                        put(&mut out, &mut cells, "");
                    }
                }
                "tc" => {
                    let cell = cells.pop().unwrap_or_default().replace('|', "\\|");
                    if let Some(row) = rows.last_mut() {
                        row.push(cell);
                    }
                }
                "tr" => {
                    if let Some(row) = rows.pop() {
                        put(&mut out, &mut cells, &format!("| {} |", row.join(" | ")));
                    }
                }
                "tbl" if cells.is_empty() => out.push('\n'),
                _ => {}
            },
            Step::Text(text) if in_text && !in_number => {
                if let Some((_, words)) = paragraphs.last_mut() {
                    words.push_str(text);
                }
            }
            Step::Text(_) => {}
        }
        if out.len() > TEXT_BYTES {
            ControlFlow::Break(())
        } else {
            ControlFlow::Continue(())
        }
    })?;

    if !existed {
        return Ok(None);
    }
    Ok(Some((hidden_slide, tidy(&out))))
}

/// Lines without spaces at their ends, and no more than one empty line in a row.
fn tidy(text: &str) -> String {
    let mut lines: Vec<&str> = Vec::new();
    for line in text.lines().map(str::trim_end) {
        if !line.is_empty() || lines.last().is_some_and(|last| !last.is_empty()) {
            lines.push(line);
        }
    }
    lines.join("\n").trim().to_string()
}

/// Word's heading styles, by id, streamed from `word/styles.xml`.
fn heading_styles(archive: &mut Archive) -> HashMap<String, usize> {
    let mut levels = HashMap::new();
    let mut id: Option<String> = None;
    let _ = walk_part(archive, "word/styles.xml", |step| {
        match step {
            Step::Open(element) | Step::Leaf(element) if local(element.name().0) == "style" => {
                id = attribute(element, "styleId", false);
            }
            Step::Open(element) | Step::Leaf(element) if local(element.name().0) == "name" => {
                let name = attribute(element, "val", false).unwrap_or_default().to_lowercase();
                let level = if name == "title" {
                    Some(1)
                } else {
                    name.strip_prefix("heading").and_then(|level| level.trim().parse::<usize>().ok())
                };
                if let (Some(id), Some(level)) = (&id, level.filter(|level| (1..=6).contains(level))) {
                    levels.insert(id.clone(), level);
                }
            }
            Step::Close("style") => id = None,
            _ => {}
        }
        ControlFlow::Continue(())
    });
    levels
}

/// The words of a Word document (.docx): its body, in order.
pub(crate) fn word(bytes: &[u8]) -> Result<String, DocError> {
    let mut archive = archive(bytes)?;
    let headings = heading_styles(&mut archive);
    let (_, body) = flow_part(&mut archive, "word/document.xml", &Flow { headings: Some(&headings), blank_lines: true })?
        .ok_or(DocError::Unreadable)?;
    Ok(body)
}

/// Where a link points from a part in `folder`: `../notesSlides/notesSlide1.xml` from `ppt/slides` is `ppt/notesSlides/notesSlide1.xml`.
fn resolve(folder: &str, target: &str) -> String {
    let mut path: Vec<&str> = if target.starts_with('/') {
        Vec::new()
    } else {
        folder.split('/').filter(|step| !step.is_empty()).collect()
    };
    for step in target.split('/') {
        match step {
            "" | "." => {}
            ".." => {
                path.pop();
            }
            step => path.push(step),
        }
    }
    path.join("/")
}

/// The links of a part, by id: what each is (the end of its type) and the part it points to.
fn links(archive: &mut Archive, part_path: &str) -> HashMap<String, (String, String)> {
    let (folder, file) = part_path.rsplit_once('/').unwrap_or(("", part_path));
    let rels_name = format!("{folder}/_rels/{file}.rels");
    let mut found = HashMap::new();
    let _ = walk_part(archive, &rels_name, |step| {
        if let Step::Open(element) | Step::Leaf(element) = step
            && local(element.name().0) == "Relationship"
            && attribute(element, "TargetMode", false).is_none_or(|mode| mode != "External")
            && let (Some(id), Some(kind), Some(target)) = (
                attribute(element, "Id", false),
                attribute(element, "Type", false),
                attribute(element, "Target", false),
            )
        {
            let kind = kind.rsplit('/').next().unwrap_or_default().to_string();
            found.insert(id, (kind, resolve(folder, &target)));
        }
        ControlFlow::Continue(())
    });
    found
}

/// The words of a presentation (.pptx), slide by slide in the order they are
/// shown, each with its speaker's notes; and how many slides there are.
pub(crate) fn slides(bytes: &[u8]) -> Result<(usize, String), DocError> {
    let mut archive = archive(bytes)?;
    let linked = links(&mut archive, "ppt/presentation.xml");
    let mut order: Vec<String> = Vec::new();
    let has_presentation = walk_part(&mut archive, "ppt/presentation.xml", |step| {
        if let Step::Open(element) | Step::Leaf(element) = step
            && local(element.name().0) == "sldId"
            && let Some((kind, path)) = attribute(element, "id", true).and_then(|id| linked.get(&id))
            && kind == "slide"
        {
            order.push(path.clone());
        }
        ControlFlow::Continue(())
    })?;
    if !has_presentation {
        return Err(DocError::Unreadable);
    }
    // A presentation that does not list its slides: by the numbers in their names.
    if order.is_empty() {
        let mut numbered: Vec<(u32, String)> = archive
            .file_names()
            .filter_map(|name| {
                let name = name.ok()?;
                Some((
                    name.strip_prefix("ppt/slides/slide")?.strip_suffix(".xml")?.parse().ok()?,
                    name.to_string(),
                ))
            })
            .collect();
        numbered.sort();
        order = numbered.into_iter().map(|(_, name)| name).collect();
    }
    let drawing = Flow { headings: None, blank_lines: false };
    let mut out = String::new();
    for (at, path) in order.iter().enumerate() {
        let Some((hidden, words)) = flow_part(&mut archive, path, &drawing)? else { continue };
        out.push_str(&format!(
            "[slide {} of {}{}]\n",
            at + 1,
            order.len(),
            if hidden { ", hidden" } else { "" }
        ));
        out.push_str(if words.is_empty() { "(no text on this slide)" } else { &words });
        out.push('\n');
        let notes_path = links(&mut archive, path)
            .into_values()
            .find(|(kind, _)| kind == "notesSlide")
            .map(|(_, notes)| notes);
        if let Some(notes_path) = notes_path
            && let Some((_, notes)) = flow_part(&mut archive, &notes_path, &drawing)?
            && !notes.is_empty()
        {
            out.push_str(&format!("Notes: {notes}\n"));
        }
        out.push('\n');
        if out.len() > TEXT_BYTES {
            break;
        }
    }
    Ok((order.len(), out.trim_end().to_string()))
}

/// The name Excel gives a column counted from 0: A to Z, then AA.
fn column(mut at: u32) -> String {
    let mut name = Vec::new();
    loop {
        name.push(b'A' + (at % 26) as u8);
        if at < 26 {
            break;
        }
        at = at / 26 - 1;
    }
    name.reverse();
    String::from_utf8(name).unwrap_or_default()
}

/// Format a dense [`calamine::Data`] cell value onto one single-line string.
fn cell(value: &calamine::Data) -> String {
    use calamine::Data;
    let text = match value {
        Data::Empty => String::new(),
        Data::Float(number) if number.fract() == 0.0 && number.abs() < 1e15 => format!("{}", *number as i64),
        Data::DateTime(when) if when.is_datetime() => match when.as_datetime() {
            Some(at) if chrono::Datelike::year(&at) < 1900 => at.format("%H:%M:%S").to_string(),
            Some(at) if at.time() == chrono::NaiveTime::MIN => at.format("%Y-%m-%d").to_string(),
            Some(at) => at.format("%Y-%m-%d %H:%M:%S").to_string(),
            None => value.to_string(),
        },
        other => other.to_string(),
    };
    text.replace(['\t', '\n', '\r'], " ")
}

/// Format a borrowed [`calamine::DataRef`] cell value yielded by `worksheet_cells_reader`.
fn cell_ref(value: &calamine::DataRef<'_>) -> String {
    use calamine::DataRef;
    let text = match value {
        DataRef::Empty => String::new(),
        DataRef::Int(n) => n.to_string(),
        DataRef::Float(number) if number.fract() == 0.0 && number.abs() < 1e15 => format!("{}", *number as i64),
        DataRef::Float(number) => number.to_string(),
        DataRef::String(s) => s.clone(),
        DataRef::SharedString(s) => (*s).to_string(),
        DataRef::Bool(b) => b.to_string(),
        DataRef::DateTime(when) if when.is_datetime() => match when.as_datetime() {
            Some(at) if chrono::Datelike::year(&at) < 1900 => at.format("%H:%M:%S").to_string(),
            Some(at) if at.time() == chrono::NaiveTime::MIN => at.format("%Y-%m-%d").to_string(),
            Some(at) => at.format("%Y-%m-%d %H:%M:%S").to_string(),
            None => when.to_string(),
        },
        DataRef::DateTime(when) => when.to_string(),
        DataRef::DateTimeIso(s) | DataRef::DurationIso(s) => s.clone(),
        DataRef::Error(err) => err.to_string(),
    };
    text.replace(['\t', '\n', '\r'], " ")
}

struct SparseSheet {
    rows: BTreeMap<u32, Vec<(u32, String)>>,
    top: u32,
    bottom: u32,
    left: u32,
    right: u32,
    truncated_cells: bool,
}

impl SparseSheet {
    fn new() -> Self {
        Self {
            rows: BTreeMap::new(),
            top: u32::MAX,
            bottom: 0,
            left: u32::MAX,
            right: 0,
            truncated_cells: false,
        }
    }

    fn push(&mut self, row: u32, col: u32, val: String, total_cells: &mut usize, approx_bytes: &mut usize) -> bool {
        if val.is_empty() {
            return true;
        }
        self.top = self.top.min(row);
        self.bottom = self.bottom.max(row);
        self.left = self.left.min(col);
        self.right = self.right.max(col);
        *approx_bytes = approx_bytes.saturating_add(val.len() + 4);
        *total_cells += 1;
        self.rows.entry(row).or_default().push((col, val));
        if *total_cells >= MAX_SHEET_CELLS || *approx_bytes > TEXT_BYTES {
            self.truncated_cells = *total_cells >= MAX_SHEET_CELLS;
            return false;
        }
        true
    }

    fn render_into(self, head: &str, out: &mut String) -> bool {
        if self.rows.is_empty() {
            out.push_str(&format!("{head}]\n(empty)\n"));
            return true;
        }
        out.push_str(&format!(
            "{head}; rows {}-{}, columns {}-{}]\n",
            self.top + 1,
            self.bottom + 1,
            column(self.left),
            column(self.right)
        ));
        let wide_sparse = self.right.saturating_sub(self.left) > 256;
        let mut prev_row: Option<u32> = None;
        for (row_idx, mut row_cells) in self.rows {
            if let Some(prev) = prev_row
                && row_idx > prev + 1
            {
                out.push_str(&format!("... [rows {}-{} empty] ...\n", prev + 2, row_idx));
            }
            prev_row = Some(row_idx);
            row_cells.sort_by_key(|(c, _)| *c);

            out.push_str(&format!("{}\t", row_idx as usize + 1));
            if !wide_sparse {
                let last_col = row_cells.last().map_or(self.left, |(c, _)| *c);
                let mut dense_row = vec![String::new(); (last_col - self.left + 1) as usize];
                for (c, val) in row_cells {
                    dense_row[(c - self.left) as usize] = val;
                }
                out.push_str(&dense_row.join("\t"));
            } else {
                let mut cursor = self.left;
                for (pos, (c, val)) in row_cells.into_iter().enumerate() {
                    if pos > 0 || c > cursor {
                        let gap = c.saturating_sub(cursor);
                        if gap == 0 && pos > 0 {
                            out.push('\t');
                        } else if gap <= 16 {
                            for _ in 0..gap {
                                out.push('\t');
                            }
                        } else {
                            if pos > 0 {
                                out.push('\t');
                            }
                            out.push_str(&format!("... [columns {}-{} empty]\t", column(cursor), column(c - 1)));
                        }
                    }
                    out.push_str(&val);
                    cursor = c.saturating_add(1);
                }
            }
            out.push('\n');
            if out.len() > TEXT_BYTES {
                return false;
            }
        }
        if self.truncated_cells {
            out.push_str(&format!("... [stopped at {MAX_SHEET_CELLS} non-empty cells] ...\n"));
        }
        out.len() <= TEXT_BYTES
    }
}

fn validate_zip_spreadsheet_parts(bytes: &[u8]) -> Result<(), DocError> {
    if !bytes.starts_with(b"PK\x03\x04") {
        return Ok(());
    }
    let mut zip = archive(bytes)?;
    for idx in 0..zip.len() {
        let Ok(entry) = zip.by_index_raw(idx) else { continue };
        if entry.size() > PART_BYTES {
            return Err(DocError::Unreadable);
        }
    }
    Ok(())
}

fn render_dense_range(
    head: &str,
    range: &calamine::Range<calamine::Data>,
    total_cells: &mut usize,
    out: &mut String,
) -> bool {
    match (range.start(), range.end()) {
        (Some((top, left)), Some((bottom, right))) if !range.is_empty() => {
            let rows_span = u64::from(bottom.saturating_sub(top)) + 1;
            let cols_span = u64::from(right.saturating_sub(left)) + 1;
            if rows_span.saturating_mul(cols_span) > MAX_DENSE_DIMENSION_CELLS {
                out.push_str(&format!(
                    "{head}; rows {}-{}, columns {}-{}]\n(sheet dimensions exceed the {MAX_DENSE_DIMENSION_CELLS}-cell limit)\n",
                    top + 1,
                    bottom + 1,
                    column(left),
                    column(right)
                ));
                return true;
            }
            out.push_str(&format!(
                "{head}; rows {}-{}, columns {}-{}]\n",
                top + 1,
                bottom + 1,
                column(left),
                column(right)
            ));
            let mut prev_emitted_row: Option<usize> = None;
            for (offset, row) in range.rows().enumerate() {
                let cells: Vec<String> = row.iter().map(cell).collect();
                let non_empty_in_row = cells.iter().filter(|c| !c.is_empty()).count();
                let used = cells.iter().rposition(|cell| !cell.is_empty()).map_or(0, |last| last + 1);
                if used > 0 {
                    let row_1based = top as usize + 1 + offset;
                    if let Some(prev) = prev_emitted_row
                        && row_1based > prev + 1
                    {
                        out.push_str(&format!("... [rows {}-{} empty] ...\n", prev + 1, row_1based - 1));
                    }
                    prev_emitted_row = Some(row_1based);
                    out.push_str(&format!("{row_1based}\t{}\n", cells[..used].join("\t")));
                    *total_cells = total_cells.saturating_add(non_empty_in_row);
                }
                if out.len() > TEXT_BYTES || *total_cells >= MAX_SHEET_CELLS {
                    if *total_cells >= MAX_SHEET_CELLS && out.len() <= TEXT_BYTES {
                        out.push_str(&format!("... [stopped at {MAX_SHEET_CELLS} non-empty cells] ...\n"));
                    }
                    return false;
                }
            }
            true
        }
        _ => {
            out.push_str(&format!("{head}]\n(empty)\n"));
            true
        }
    }
}

/// The values of a spreadsheet's cells, sheet by sheet, each under a line
/// that names it and the rows and columns it fills, a row to a line that
/// starts with its number; and how many sheets there are.
pub(crate) fn sheets(bytes: &[u8]) -> Result<(usize, String), DocError> {
    use calamine::{Reader, SheetVisible, Sheets};

    validate_zip_spreadsheet_parts(bytes)?;
    // Guard against legacy `.xls` files whose raw stream exceeds our bounded part ceiling.
    if !bytes.starts_with(b"PK\x03\x04") && bytes.len() as u64 > PART_BYTES {
        return Err(DocError::Unreadable);
    }

    let mut book = calamine::open_workbook_auto_from_rs(Cursor::new(bytes)).map_err(|error| {
        if error.to_string().to_lowercase().contains("password") {
            DocError::Locked
        } else {
            DocError::Unreadable
        }
    })?;
    let sheets = book.sheets_metadata().to_vec();
    let mut out = String::new();
    let mut total_cells = 0usize;
    let mut approx_bytes = 0usize;

    for (at, sheet) in sheets.iter().enumerate() {
        let hidden = if matches!(sheet.visible, SheetVisible::Visible) { "" } else { ", hidden" };
        let head = format!("[sheet {} of {}: {}{hidden}", at + 1, sheets.len(), sheet.name);

        let keep_going = match &mut book {
            Sheets::Xlsx(xlsx) => match xlsx.worksheet_cells_reader(&sheet.name) {
                Ok(mut reader) => {
                    let mut sparse = SparseSheet::new();
                    let mut read_ok = true;
                    loop {
                        match reader.next_cell() {
                            Ok(Some(c)) => {
                                let (r, col) = c.get_position();
                                if !sparse.push(r, col, cell_ref(c.get_value()), &mut total_cells, &mut approx_bytes) {
                                    break;
                                }
                            }
                            Ok(None) => break,
                            Err(_) => {
                                read_ok = false;
                                break;
                            }
                        }
                    }
                    if read_ok || !sparse.rows.is_empty() {
                        sparse.render_into(&head, &mut out)
                    } else {
                        out.push_str(&format!("{head}]\n(not a sheet of cells: a chart, or a sheet of an old kind)\n"));
                        true
                    }
                }
                Err(_) => {
                    out.push_str(&format!("{head}]\n(not a sheet of cells: a chart, or a sheet of an old kind)\n"));
                    true
                }
            },
            Sheets::Xlsb(xlsb) => match xlsb.worksheet_cells_reader(&sheet.name) {
                Ok(mut reader) => {
                    let mut sparse = SparseSheet::new();
                    let mut read_ok = true;
                    loop {
                        match reader.next_cell() {
                            Ok(Some(c)) => {
                                let (r, col) = c.get_position();
                                if !sparse.push(r, col, cell_ref(c.get_value()), &mut total_cells, &mut approx_bytes) {
                                    break;
                                }
                            }
                            Ok(None) => break,
                            Err(_) => {
                                read_ok = false;
                                break;
                            }
                        }
                    }
                    if read_ok || !sparse.rows.is_empty() {
                        sparse.render_into(&head, &mut out)
                    } else {
                        out.push_str(&format!("{head}]\n(not a sheet of cells: a chart, or a sheet of an old kind)\n"));
                        true
                    }
                }
                Err(_) => {
                    out.push_str(&format!("{head}]\n(not a sheet of cells: a chart, or a sheet of an old kind)\n"));
                    true
                }
            },
            Sheets::Xls(xls) => match xls.worksheet_range(&sheet.name) {
                Ok(range) => render_dense_range(&head, &range, &mut total_cells, &mut out),
                Err(_) => {
                    out.push_str(&format!("{head}]\n(not a sheet of cells: a chart, or a sheet of an old kind)\n"));
                    true
                }
            },
            Sheets::Ods(ods) => match ods.worksheet_range(&sheet.name) {
                Ok(range) => render_dense_range(&head, &range, &mut total_cells, &mut out),
                Err(_) => {
                    out.push_str(&format!("{head}]\n(not a sheet of cells: a chart, or a sheet of an old kind)\n"));
                    true
                }
            },
        };

        if !keep_going || out.len() > TEXT_BYTES || total_cells >= MAX_SHEET_CELLS {
            return Ok((sheets.len(), out.trim_end().to_string()));
        }
        out.push('\n');
    }
    Ok((sheets.len(), out.trim_end().to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::documents::tests::package;

    #[test]
    fn sparse_xlsx_with_a1_and_xfd1048576_streams_fast_without_dense_matrix_oom() {
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
                &format!(r#"<workbook {S}><sheets><sheet name="Sparse" sheetId="1" r:id="rId1"/></sheets></workbook>"#),
            ),
            (
                "xl/_rels/workbook.xml.rels",
                r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/></Relationships>"#,
            ),
            (
                "xl/worksheets/sheet1.xml",
                &format!(
                    r#"<worksheet {S}><dimension ref="A1:XFD1048576"/><sheetData><row r="1"><c r="A1" t="inlineStr"><is><t>ORIGIN</t></is></c></row><row r="1048576"><c r="XFD1048576" t="inlineStr"><is><t>FAR_CORNER</t></is></c></row></sheetData></worksheet>"#
                ),
            ),
        ]);

        let started = std::time::Instant::now();
        let (count, text) = sheets(&bytes).expect("sparse xlsx must succeed without OOM");
        let elapsed = started.elapsed();

        assert_eq!(count, 1);
        assert!(elapsed < std::time::Duration::from_millis(50), "sparse xlsx took {elapsed:?} (>= 50ms)");
        assert!(text.contains("[sheet 1 of 1: Sparse; rows 1-1048576, columns A-XFD]"), "{text}");
        assert!(text.contains("1\tORIGIN"), "{text}");
        assert!(text.contains("... [rows 2-1048575 empty] ..."), "{text}");
        assert!(text.contains("1048576\t") && text.contains("FAR_CORNER"), "{text}");
        assert!(text.len() < 512, "sparse output must stay compact: {} bytes", text.len());
    }
}
