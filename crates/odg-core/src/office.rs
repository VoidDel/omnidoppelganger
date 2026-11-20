//! The words of Office documents: Word (.docx), PowerPoint (.pptx), and
//! spreadsheets (.xlsx, .xlsm, .xlsb, .xls, .ods). See [`crate::documents`],
//! which these are kinds of.
//!
//! A .docx or .pptx is a zip of XML parts; the words are read out of those,
//! in the order they are read in. Spreadsheets are read by `calamine`.

use std::collections::HashMap;
use std::io::{Cursor, Read};

use quick_xml::Reader;
use quick_xml::events::{BytesStart, Event};
use zip::ZipArchive;

use crate::documents::{DocError, TEXT_BYTES};

/// The most of one part of a document that is unzipped: far more than the
/// words of any real one, and an end to a file made to unzip without end.
const PART_BYTES: u64 = 200 * 1024 * 1024;

type Archive<'a> = ZipArchive<Cursor<&'a [u8]>>;

fn archive(bytes: &[u8]) -> Result<Archive<'_>, DocError> {
    ZipArchive::new(Cursor::new(bytes)).map_err(|_| DocError::Unreadable)
}

/// A part of the package as text, or `None` when there is no such part.
fn part(archive: &mut Archive, name: &str) -> Option<String> {
    let file = archive.by_name(name).ok()?;
    let mut bytes = Vec::new();
    file.take(PART_BYTES).read_to_end(&mut bytes).ok()?;
    Some(String::from_utf8_lossy(&bytes).into_owned())
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
        .and_then(|found| found.normalized_value(quick_xml::XmlVersion::Implicit1_0).ok().map(|value| value.into_owned()))
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

/// Go through an XML part step by step, with entities written as their
/// characters. What is inside `mc:Fallback` is skipped: it is the same thing
/// again, drawn the old way for readers that do not know the new.
fn walk(xml: &str, mut visit: impl FnMut(Step)) -> Result<(), DocError> {
    let mut reader = Reader::from_str(xml);
    let mut skipping = 0usize;
    // How many elements are open: a part that ends with some still open was cut short.
    let mut open = 0usize;
    loop {
        let event = reader.read_event().map_err(|_| DocError::Unreadable)?;
        match &event {
            Event::Start(_) => open += 1,
            Event::End(_) => open = open.saturating_sub(1),
            _ => {}
        }
        match event {
            Event::Start(element) => {
                if skipping > 0 || local(element.name().0) == "Fallback" {
                    skipping += 1;
                } else {
                    visit(Step::Open(&element));
                }
            }
            Event::End(element) => {
                if skipping > 0 {
                    skipping -= 1;
                } else {
                    visit(Step::Close(local(element.name().0)));
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
                    visit(Step::Text(character.encode_utf8(&mut [0; 4])));
                }
            }
            Event::Eof if open > 0 => return Err(DocError::Unreadable),
            Event::Eof => return Ok(()),
            _ => {}
        }
    }
}

/// How [`flow`] reads a part: Word's body, or the drawings of a slide.
struct Flow<'a> {
    /// Word's heading styles, by id, and the level of each: a paragraph in one is marked with as many `#`.
    headings: Option<&'a HashMap<String, usize>>,
    /// Keep empty paragraphs, which in Word part what is written; on a slide they are only room.
    blank_lines: bool,
}

/// The words of a part, a paragraph to a line: a heading marked with `#`,
/// an item of a list with `-`, and a table as rows of cells split by `|`.
/// The local names are the same in Word's body and on a slide.
fn flow(xml: &str, how: &Flow) -> Result<String, DocError> {
    let mut out = String::new();
    // The paragraphs open, the innermost last (a text box in Word holds paragraphs of its own): each with its mark and words.
    let mut paragraphs: Vec<(String, String)> = Vec::new();
    // The cells open, the innermost last, and the row being read in each table open.
    let mut cells: Vec<String> = Vec::new();
    let mut rows: Vec<Vec<String>> = Vec::new();
    let (mut in_run, mut in_text, mut in_number) = (false, false, false);

    /// A finished line: into the cell it is in, or onto the page.
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

    walk(xml, |step| match step {
        Step::Open(element) | Step::Leaf(element) => {
            let open = matches!(step, Step::Open(_));
            match local(element.name().0) {
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
                // A field that numbers the slide: its text is only the number.
                "fld" if open => in_number = attribute(element, "type", false).is_some_and(|kind| kind == "slidenum"),
                "tab" if in_run => paragraphs.last_mut().into_iter().for_each(|(_, words)| words.push('\t')),
                "br" | "cr" => paragraphs.last_mut().into_iter().for_each(|(_, words)| words.push('\n')),
                "tr" if open => rows.push(Vec::new()),
                "tc" if open => cells.push(String::new()),
                _ => {}
            }
        }
        Step::Close(name) => match name {
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
            // A blank line after a table on the page, so that it stands apart from what follows.
            "tbl" if cells.is_empty() => out.push('\n'),
            _ => {}
        },
        Step::Text(text) if in_text && !in_number => {
            if let Some((_, words)) = paragraphs.last_mut() {
                words.push_str(text);
            }
        }
        Step::Text(_) => {}
    })?;
    Ok(tidy(&out))
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

/// Word's heading styles, by id, and the level of each: from `styles.xml`,
/// where a style's id may be anything (`1` in a Chinese template) and its
/// name says what it is (`heading 1`).
fn heading_styles(styles: &str) -> HashMap<String, usize> {
    let mut levels = HashMap::new();
    let mut id: Option<String> = None;
    let _ = walk(styles, |step| match step {
        Step::Open(element) | Step::Leaf(element) if local(element.name().0) == "style" => id = attribute(element, "styleId", false),
        Step::Open(element) | Step::Leaf(element) if local(element.name().0) == "name" => {
            let name = attribute(element, "val", false).unwrap_or_default().to_lowercase();
            let level = if name == "title" { Some(1) } else { name.strip_prefix("heading").and_then(|level| level.trim().parse::<usize>().ok()) };
            if let (Some(id), Some(level)) = (&id, level.filter(|level| (1..=6).contains(level))) {
                levels.insert(id.clone(), level);
            }
        }
        Step::Close("style") => id = None,
        _ => {}
    });
    levels
}

/// The words of a Word document (.docx): its body, in order.
pub(crate) fn word(bytes: &[u8]) -> Result<String, DocError> {
    let mut archive = archive(bytes)?;
    let body = part(&mut archive, "word/document.xml").ok_or(DocError::Unreadable)?;
    let headings = part(&mut archive, "word/styles.xml").map(|styles| heading_styles(&styles)).unwrap_or_default();
    flow(&body, &Flow { headings: Some(&headings), blank_lines: true })
}

/// Where a link points from a part in `folder`: `../notesSlides/notesSlide1.xml` from `ppt/slides` is `ppt/notesSlides/notesSlide1.xml`.
fn resolve(folder: &str, target: &str) -> String {
    let mut path: Vec<&str> = if target.starts_with('/') { Vec::new() } else { folder.split('/').filter(|step| !step.is_empty()).collect() };
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
    let Some(rels) = part(archive, &format!("{folder}/_rels/{file}.rels")) else {
        return HashMap::new();
    };
    let mut found = HashMap::new();
    let _ = walk(&rels, |step| {
        if let Step::Open(element) | Step::Leaf(element) = step
            && local(element.name().0) == "Relationship"
            && attribute(element, "TargetMode", false).is_none_or(|mode| mode != "External")
            && let (Some(id), Some(kind), Some(target)) = (attribute(element, "Id", false), attribute(element, "Type", false), attribute(element, "Target", false))
        {
            let kind = kind.rsplit('/').next().unwrap_or_default().to_string();
            found.insert(id, (kind, resolve(folder, &target)));
        }
    });
    found
}

/// The words of a presentation (.pptx), slide by slide in the order they are
/// shown, each with its speaker's notes; and how many slides there are.
pub(crate) fn slides(bytes: &[u8]) -> Result<(usize, String), DocError> {
    let mut archive = archive(bytes)?;
    let presentation = part(&mut archive, "ppt/presentation.xml").ok_or(DocError::Unreadable)?;
    let linked = links(&mut archive, "ppt/presentation.xml");
    let mut order: Vec<String> = Vec::new();
    walk(&presentation, |step| {
        if let Step::Open(element) | Step::Leaf(element) = step
            && local(element.name().0) == "sldId"
            && let Some((kind, path)) = attribute(element, "id", true).and_then(|id| linked.get(&id))
            && kind == "slide"
        {
            order.push(path.clone());
        }
    })?;
    // A presentation that does not list its slides: by the numbers in their names.
    if order.is_empty() {
        let mut numbered: Vec<(u32, String)> = archive
            .file_names()
            .filter_map(|name| {
                let name = name.ok()?;
                Some((name.strip_prefix("ppt/slides/slide")?.strip_suffix(".xml")?.parse().ok()?, name.to_string()))
            })
            .collect();
        numbered.sort();
        order = numbered.into_iter().map(|(_, name)| name).collect();
    }
    let drawing = Flow { headings: None, blank_lines: false };
    let mut out = String::new();
    for (at, path) in order.iter().enumerate() {
        let Some(slide) = part(&mut archive, path) else { continue };
        let mut hidden = false;
        let _ = walk(&slide, |step| {
            if let Step::Open(element) = step
                && local(element.name().0) == "sld"
            {
                hidden = attribute(element, "show", false).is_some_and(|show| show == "0");
            }
        });
        out.push_str(&format!("[slide {} of {}{}]\n", at + 1, order.len(), if hidden { ", hidden" } else { "" }));
        let words = flow(&slide, &drawing)?;
        out.push_str(if words.is_empty() { "(no text on this slide)" } else { &words });
        out.push('\n');
        let notes = links(&mut archive, path).into_values().find(|(kind, _)| kind == "notesSlide").and_then(|(_, notes)| part(&mut archive, &notes));
        if let Some(notes) = notes.map(|notes| flow(&notes, &drawing)).transpose()?.filter(|notes| !notes.is_empty()) {
            out.push_str(&format!("Notes: {notes}\n"));
        }
        out.push('\n');
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

/// A cell's value as text on one line: a whole number without its `.0`, a date as a date.
fn cell(value: &calamine::Data) -> String {
    use calamine::Data;
    let text = match value {
        Data::Empty => String::new(),
        Data::Float(number) if number.fract() == 0.0 && number.abs() < 1e15 => format!("{}", *number as i64),
        Data::DateTime(when) if when.is_datetime() => match when.as_datetime() {
            // A time without a day falls on Excel's day 0.
            Some(at) if chrono::Datelike::year(&at) < 1900 => at.format("%H:%M:%S").to_string(),
            Some(at) if at.time() == chrono::NaiveTime::MIN => at.format("%Y-%m-%d").to_string(),
            Some(at) => at.format("%Y-%m-%d %H:%M:%S").to_string(),
            None => value.to_string(),
        },
        other => other.to_string(),
    };
    text.replace(['\t', '\n', '\r'], " ")
}

/// The values of a spreadsheet's cells, sheet by sheet, each under a line
/// that names it and the rows and columns it fills, a row to a line that
/// starts with its number; and how many sheets there are.
pub(crate) fn sheets(bytes: &[u8]) -> Result<(usize, String), DocError> {
    use calamine::{Reader, SheetVisible};
    let mut book = calamine::open_workbook_auto_from_rs(Cursor::new(bytes)).map_err(|error| if error.to_string().to_lowercase().contains("password") { DocError::Locked } else { DocError::Unreadable })?;
    let sheets = book.sheets_metadata().to_vec();
    let mut out = String::new();
    for (at, sheet) in sheets.iter().enumerate() {
        let hidden = if matches!(sheet.visible, SheetVisible::Visible) { "" } else { ", hidden" };
        let head = format!("[sheet {} of {}: {}{hidden}", at + 1, sheets.len(), sheet.name);
        match book.worksheet_range(&sheet.name) {
            Ok(range) => match (range.start(), range.end()) {
                (Some((top, left)), Some((bottom, right))) if !range.is_empty() => {
                    out.push_str(&format!("{head}; rows {}-{}, columns {}-{}]\n", top + 1, bottom + 1, column(left), column(right)));
                    for (offset, row) in range.rows().enumerate() {
                        let cells: Vec<String> = row.iter().map(cell).collect();
                        let used = cells.iter().rposition(|cell| !cell.is_empty()).map_or(0, |last| last + 1);
                        if used > 0 {
                            out.push_str(&format!("{}\t{}\n", top as usize + 1 + offset, cells[..used].join("\t")));
                        }
                        // The rest would be cut in any case: stop reading a sheet of millions of rows.
                        if out.len() > TEXT_BYTES {
                            return Ok((sheets.len(), out));
                        }
                    }
                }
                _ => out.push_str(&format!("{head}]\n(empty)\n")),
            },
            Err(_) => out.push_str(&format!("{head}]\n(not a sheet of cells: a chart, or a sheet of an old kind)\n")),
        }
        out.push('\n');
    }
    Ok((sheets.len(), out.trim_end().to_string()))
}
