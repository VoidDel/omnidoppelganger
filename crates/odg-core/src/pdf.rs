//! The words of a PDF, page by page: a datasheet, a paper, a manual. See
//! [`crate::documents`], which this is one kind of.
//!
//! Drawings, pictures, and where things stand on the page are not kept. A PDF
//! of scanned pages has no words to take, and is refused as such.

use pdf_extract::{Document, PlainTextOutput, output_doc_page};

use crate::documents::{DocError, TEXT_BYTES};

/// Whether these bytes are a PDF: its header is to open the file, though readers look for it in the first kilobyte.
pub fn is_pdf(bytes: &[u8]) -> bool {
    bytes[..bytes.len().min(1024)]
        .windows(5)
        .any(|window| window == b"%PDF-")
}

/// The words of a PDF, each page's under a line that numbers it, and how many
/// pages there are. A page whose words cannot be read is said to be so, and
/// the others are still read.
pub(crate) fn read(bytes: &[u8]) -> Result<(usize, String), DocError> {
    let mut document = Document::load_mem(bytes).map_err(|_| DocError::Unreadable)?;
    // Many PDFs are locked against changes only, with an empty password to open them.
    if document.is_encrypted() && document.decrypt("").is_err() {
        return Err(DocError::Locked);
    }
    let numbers: Vec<u32> = document.get_pages().keys().copied().collect();
    let mut text = String::new();
    let mut worded = false;
    for (at, number) in numbers.iter().enumerate() {
        let mut page = String::new();
        // The reader is new to some PDFs it is given, and may panic on a page: that page cannot be read, the rest still can.
        let done = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let mut output = PlainTextOutput::new(&mut page);
            output_doc_page(&document, &mut output, *number)
        }));
        text.push_str(&format!("[page {} of {}]\n", at + 1, numbers.len()));
        match done {
            Ok(Ok(())) => {
                let page = tidy(&page);
                worded |= !page.is_empty();
                text.push_str(if page.is_empty() {
                    "(no text on this page)"
                } else {
                    &page
                });
            }
            _ => text.push_str("(the text of this page could not be read)"),
        }
        text.push_str("\n\n");
        if text.len() > TEXT_BYTES {
            break;
        }
    }
    if !worded {
        return Err(DocError::NoText);
    }
    Ok((numbers.len(), text.trim_end().to_string()))
}

/// The words of a page as lines: no spaces at their ends, no more than one
/// empty line in a row, and each character as it is written in running text.
fn tidy(page: &str) -> String {
    let page: String = page.chars().map(plain).collect();
    let mut lines: Vec<&str> = Vec::new();
    for line in page.lines().map(str::trim_end) {
        if !line.trim().is_empty() || lines.last().is_some_and(|last| !last.is_empty()) {
            lines.push(if line.trim().is_empty() { "" } else { line });
        }
    }
    lines.join("\n").trim().to_string()
}

/// Radicals of the CJK Radicals Supplement drawn the same as a character in
/// running text, and that character: a Chinese font often maps its glyph for
/// 页 or 风 to the radical, and the text then says 第二⻚ for 第二页, which
/// reads and searches wrong. Radicals drawn only as part of a character (氵, 灬) stay.
const RADICALS: &[(char, char)] = &[
    ('\u{2EB0}', '\u{7E9F}'), // 纟
    ('\u{2EC4}', '\u{897F}'), // 西
    ('\u{2EC5}', '\u{89C1}'), // 见
    ('\u{2EC6}', '\u{89D2}'), // 角
    ('\u{2EC8}', '\u{8BA0}'), // 讠
    ('\u{2EC9}', '\u{8D1D}'), // 贝
    ('\u{2ECB}', '\u{8F66}'), // 车
    ('\u{2ED0}', '\u{9485}'), // 钅
    ('\u{2ED1}', '\u{9577}'), // 長
    ('\u{2ED3}', '\u{957F}'), // 长
    ('\u{2ED4}', '\u{95E8}'), // 门
    ('\u{2ED8}', '\u{9752}'), // 青
    ('\u{2ED9}', '\u{97E6}'), // 韦
    ('\u{2EDA}', '\u{9875}'), // 页
    ('\u{2EDB}', '\u{98CE}'), // 风
    ('\u{2EDC}', '\u{98DE}'), // 飞
    ('\u{2EDD}', '\u{98DF}'), // 食
    ('\u{2EE0}', '\u{9963}'), // 饣
    ('\u{2EE1}', '\u{9996}'), // 首
    ('\u{2EE2}', '\u{9A6C}'), // 马
    ('\u{2EE3}', '\u{9AA8}'), // 骨
    ('\u{2EE4}', '\u{9B3C}'), // 鬼
    ('\u{2EE5}', '\u{9C7C}'), // 鱼
    ('\u{2EE6}', '\u{9E1F}'), // 鸟
    ('\u{2EE7}', '\u{5364}'), // 卤
    ('\u{2EE8}', '\u{9EA6}'), // 麦
    ('\u{2EE9}', '\u{9EC4}'), // 黄
    ('\u{2EEA}', '\u{9EFE}'), // 黾
    ('\u{2EEC}', '\u{9F50}'), // 齐
    ('\u{2EEE}', '\u{9F7F}'), // 齿
    ('\u{2EF0}', '\u{9F99}'), // 龙
];

/// A character as running text writes it: a Kangxi radical (⼆ for 二) as
/// the character it stands for, by its compatibility mapping, as are the two
/// supplementary radicals that have one; a radical of [`RADICALS`] as its
/// character; anything else as it is.
fn plain(character: char) -> char {
    use unicode_normalization::UnicodeNormalization;
    if ('\u{2F00}'..='\u{2FD5}').contains(&character) || matches!(character, '\u{2E9F}' | '\u{2EF3}') {
        return std::iter::once(character).nfkc().next().unwrap_or(character);
    }
    RADICALS
        .iter()
        .find(|(radical, _)| *radical == character)
        .map_or(character, |(_, written)| *written)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A PDF of these pages, each a line or more of words in Helvetica, or none when a page is empty.
    pub(crate) fn sample(pages: &[&[&str]]) -> Vec<u8> {
        let mut objects: Vec<String> = Vec::new();
        let font = 3 + 2 * pages.len();
        let kids: Vec<String> = (0..pages.len()).map(|at| format!("{} 0 R", 3 + 2 * at)).collect();
        objects.push("<< /Type /Catalog /Pages 2 0 R >>".into());
        objects.push(format!(
            "<< /Type /Pages /Kids [{}] /Count {} >>",
            kids.join(" "),
            pages.len()
        ));
        for (at, lines) in pages.iter().enumerate() {
            objects.push(format!("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << /Font << /F1 {font} 0 R >> >> /Contents {} 0 R >>", 4 + 2 * at));
            let words: String = lines
                .iter()
                .enumerate()
                .map(|(row, line)| format!("BT /F1 12 Tf 72 {} Td ({line}) Tj ET\n", 760 - 16 * row as i64))
                .collect();
            objects.push(format!("<< /Length {} >>\nstream\n{words}endstream", words.len()));
        }
        objects.push("<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".into());
        let mut pdf = b"%PDF-1.4\n".to_vec();
        let mut offsets = Vec::new();
        for (at, object) in objects.iter().enumerate() {
            offsets.push(pdf.len());
            pdf.extend_from_slice(format!("{} 0 obj\n{object}\nendobj\n", at + 1).as_bytes());
        }
        let xref = pdf.len();
        pdf.extend_from_slice(format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1).as_bytes());
        for offset in offsets {
            pdf.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
        }
        pdf.extend_from_slice(
            format!(
                "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
                objects.len() + 1
            )
            .as_bytes(),
        );
        pdf
    }

    #[test]
    fn takes_the_words_of_each_page_under_its_number() {
        let pdf = sample(&[&["Pin 1 is VIN", "Pin 2 is GND"], &[], &["Absolute maximum 42 V"]]);
        assert!(is_pdf(&pdf));
        let (pages, text) = read(&pdf).unwrap();
        assert_eq!(pages, 3);
        let at = |words: &str| text.find(words).unwrap_or_else(|| panic!("{words} not in {text}"));
        assert!(at("[page 1 of 3]") < at("Pin 1 is VIN") && at("Pin 1 is VIN") < at("Pin 2 is GND"));
        assert!(at("Pin 2 is GND") < at("[page 2 of 3]") && at("[page 2 of 3]") < at("(no text on this page)"));
        assert!(at("[page 3 of 3]") < at("Absolute maximum 42 V"));
    }

    #[test]
    fn writes_radicals_a_font_put_for_characters_as_the_characters() {
        // 第⼆⻚ as a font gave it: a Kangxi radical and a supplementary one; then 氵, which is only ever part of a character.
        assert_eq!(
            tidy("第\u{2F06}\u{2EDA}：\u{2EDB}扇 \u{2F00}\u{2EA1}  \n\n\n\nΩ（R22）"),
            "第二页：风扇 一\u{2EA1}\n\nΩ（R22）"
        );
    }

    #[test]
    fn says_why_a_pdf_has_no_words_to_give() {
        assert_eq!(read(&sample(&[&[], &[]])), Err(DocError::NoText));
        assert_eq!(read(b"%PDF-1.7\nnot really"), Err(DocError::Unreadable));
        let mut cut = sample(&[&["words"]]);
        cut.truncate(cut.len() / 3);
        assert!(read(&cut).is_err());
        assert!(!is_pdf(b"plain words"));
    }
}
