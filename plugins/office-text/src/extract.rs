//! Text of .docx, .xlsx and .pptx files: a zip of XML parts, read as a stream.
//!
//! Nothing is trusted: the sum of declared sizes, the size of each part, the
//! number of entries and the time are all bounded, and every read goes through
//! a counter that stops at the limit, whatever the zip headers claim. Only
//! the XML parts that hold text are opened, one at a time, and the XML is
//! parsed event by event, so no part is ever held in memory.

use std::cell::Cell;
use std::io::{BufReader, Cursor, Read};
use std::time::{Duration, Instant};

use quick_xml::events::Event;
use quick_xml::Reader;
use zip::ZipArchive;

#[derive(Debug, Clone)]
pub struct Limits {
    /// Size of the file itself.
    pub max_file: u64,
    /// Everything read out of the archive, all parts together.
    pub max_total: u64,
    /// One XML part.
    pub max_part: u64,
    pub max_entries: usize,
    /// PDF only: more pages than this and the file is skipped.
    pub max_pages: usize,
    /// Text returned (matches the content index's own cap of 1 MiB).
    pub max_text: usize,
    pub time: Duration,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_file: 50 * 1024 * 1024,
            max_total: 100 * 1024 * 1024,
            max_part: 20 * 1024 * 1024,
            max_entries: 5000,
            max_pages: 500,
            max_text: 1 << 20,
            time: Duration::from_secs(10),
        }
    }
}

/// Counts what is read and stops at the limits.
struct Guard<'a, R> {
    inner: R,
    total: &'a Cell<u64>,
    max_total: u64,
    part: u64,
    max_part: u64,
    deadline: Instant,
}

impl<R: Read> Read for Guard<'_, R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let fail = |m: &str| Err(std::io::Error::other(m.to_owned()));
        if Instant::now() > self.deadline {
            return fail("time limit exceeded");
        }
        let n = self.inner.read(buf)?;
        self.part += n as u64;
        self.total.set(self.total.get() + n as u64);
        if self.part > self.max_part {
            return fail("a part is over the per-part limit when uncompressed");
        }
        if self.total.get() > self.max_total {
            return fail("uncompressed size limit exceeded");
        }
        Ok(n)
    }
}

fn wanted(ext: &str, name: &str) -> bool {
    let xml = name.ends_with(".xml");
    match ext {
        "docx" => {
            xml && (name == "word/document.xml"
                || name == "word/footnotes.xml"
                || name == "word/endnotes.xml"
                || name.starts_with("word/header")
                || name.starts_with("word/footer"))
        }
        "xlsx" => xml && (name == "xl/sharedStrings.xml" || name.starts_with("xl/worksheets/")),
        "pptx" => xml && (name.starts_with("ppt/slides/") || name.starts_with("ppt/notesSlides/")),
        _ => false,
    }
}

/// Text of one Office file, or the reason it is skipped.
pub fn extract_office(bytes: &[u8], ext: &str, limits: &Limits) -> Result<String, String> {
    let deadline = Instant::now() + limits.time;
    if ext == "pdf" {
        return crate::pdf::extract_pdf(bytes, limits);
    }
    if !matches!(ext, "docx" | "xlsx" | "pptx") {
        return Err(format!("not an Office file type: .{ext}"));
    }
    if bytes.len() as u64 > limits.max_file {
        return Err("larger than 50 MB".into());
    }
    // An encrypted Office file is an OLE container, not a zip.
    let mut zip = ZipArchive::new(Cursor::new(bytes))
        .map_err(|_| "not a readable zip (encrypted or corrupt)".to_owned())?;
    if zip.len() > limits.max_entries {
        return Err("too many entries in the archive".into());
    }
    let mut names = Vec::new();
    let mut declared: u64 = 0;
    for i in 0..zip.len() {
        let f = zip
            .by_index_raw(i)
            .map_err(|_| "corrupt zip directory".to_owned())?;
        declared = declared.saturating_add(f.size());
        let Ok(name) = f.name() else { continue };
        if wanted(ext, &name) {
            if f.size() > limits.max_part {
                return Err("a part is over the per-part limit when uncompressed".into());
            }
            names.push(name.into_owned());
        }
    }
    if declared > limits.max_total {
        return Err("declared uncompressed size is over 100 MB (zip bomb)".into());
    }
    names.sort();
    let total = Cell::new(0u64);
    let mut out = String::new();
    for name in names {
        let Ok(part) = zip.by_name(&name) else {
            return Err(format!("could not open {name} (encrypted or corrupt)"));
        };
        let guard = Guard {
            inner: part,
            total: &total,
            max_total: limits.max_total,
            part: 0,
            max_part: limits.max_part,
            deadline,
        };
        read_part(guard, limits.max_text, &mut out).map_err(|e| format!("{name}: {e}"))?;
        if out.len() >= limits.max_text {
            break;
        }
    }
    let mut end = out.len().min(limits.max_text);
    while !out.is_char_boundary(end) {
        end -= 1;
    }
    out.truncate(end);
    Ok(out)
}

pub(crate) fn push_clean(out: &mut String, s: &str) {
    out.extend(
        s.chars()
            .map(|c| if c.is_control() && c != '\n' { ' ' } else { c }),
    );
}

/// Appends the text of the `t` elements of one part, with a line break after
/// each paragraph, shared string or row and a space after each cell.
fn read_part(src: impl Read, max_text: usize, out: &mut String) -> Result<(), String> {
    let mut reader = Reader::from_reader(BufReader::with_capacity(16 * 1024, src));
    let mut buf = Vec::new();
    let mut in_t = false;
    loop {
        buf.clear();
        let ev = reader
            .read_event_into(&mut buf)
            .map_err(|e| e.to_string())?;
        match ev {
            Event::Start(e) => {
                if e.local_name().as_ref() == "t" {
                    in_t = true;
                }
            }
            Event::Empty(e) => {
                if matches!(e.local_name().as_ref(), "tab" | "br") {
                    out.push(' ');
                }
            }
            Event::Text(t) if in_t => {
                push_clean(out, &t.xml10_content());
            }
            Event::CData(t) if in_t => {
                push_clean(out, &t.xml10_content());
            }
            Event::GeneralRef(r) if in_t => {
                if let Ok(Some(c)) = r.resolve_char_ref() {
                    out.push(c);
                } else if let Some(s) = quick_xml::escape::resolve_predefined_entity(&r) {
                    out.push_str(s);
                }
            }
            Event::End(e) => match e.local_name().as_ref() {
                "t" => in_t = false,
                "p" | "si" | "row" => out.push('\n'),
                "c" | "tc" => out.push(' '),
                _ => {}
            },
            Event::Eof => return Ok(()),
            _ => {}
        }
        if out.len() >= max_text {
            return Ok(());
        }
    }
}
