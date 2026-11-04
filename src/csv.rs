//! CSV / TSV grid model: delimiter sniffing, a quote-aware record index,
//! field parsing and quoting, column filters, sorting, and the clipboard
//! formats spreadsheets read (TSV text, HTML, Excel's XML Spreadsheet).

use crate::app::Input;
use crate::doc::Doc;
use std::borrow::Cow;
use std::cmp::Ordering;

pub const DELIMS: [(u8, &str); 4] = [(b',', "Comma"), (b'\t', "Tab"), (b';', "Semicolon"), (b'|', "Pipe")];

/// A quoted field longer than this that reaches a line end is taken to be
/// an unclosed quote: the record ends there instead of swallowing the file.
const MAX_QUOTED: usize = 1 << 20;

/// Clipboard copies above this many cells carry plain text only.
pub const RICH_CELLS: usize = 200_000;

pub fn delim_name(d: u8) -> &'static str {
    DELIMS.iter().find(|x| x.0 == d).map(|x| x.1).unwrap_or("Comma")
}

/// Extensions opened as a grid.
pub fn is_table_path(p: &std::path::Path) -> bool {
    let ext = p.extension().map(|e| e.to_string_lossy().to_ascii_lowercase()).unwrap_or_default();
    matches!(ext.as_str(), "csv" | "tsv" | "tab")
}

#[derive(Clone, Copy, PartialEq)]
enum St {
    Start,
    Plain,
    Quoted,
    QuoteEnd,
}

/// Start offset of every record followed by `text.len()` as an end
/// sentinel, and the largest field count. Only a quote that opens a field
/// starts quoting, so an inch mark (`6" PIPE`) stays plain text.
pub fn index(text: &[u8], d: u8) -> (Vec<u32>, usize) {
    let mut starts = vec![0u32];
    let mut st = St::Start;
    let (mut fields, mut maxf, mut qfrom) = (1usize, 1usize, 0usize);
    for (i, &c) in text.iter().enumerate() {
        let end = match st {
            St::Quoted => {
                if c == b'"' {
                    st = St::QuoteEnd;
                    false
                } else {
                    c == b'\n' && i - qfrom > MAX_QUOTED
                }
            }
            _ if c == b'\n' => true,
            _ if c == d => {
                st = St::Start;
                fields += 1;
                false
            }
            St::Start if c == b'"' => {
                st = St::Quoted;
                qfrom = i;
                false
            }
            St::QuoteEnd if c == b'"' => {
                st = St::Quoted;
                false
            }
            _ => {
                st = St::Plain;
                false
            }
        };
        if end {
            maxf = maxf.max(fields);
            fields = 1;
            st = St::Start;
            starts.push(i as u32 + 1);
        }
    }
    if *starts.last().unwrap() as usize == text.len() {
        if starts.len() > 1 {
            starts.pop();
        }
    } else {
        maxf = maxf.max(fields);
    }
    starts.push(text.len() as u32);
    (starts, maxf)
}

/// End of the raw field that starts at `a`.
fn span_end(rec: &[u8], d: u8, a: usize) -> usize {
    let mut st = St::Start;
    for (i, &c) in rec[a..].iter().enumerate() {
        match st {
            St::Start | St::Plain | St::QuoteEnd if c == d => return a + i,
            St::Start if c == b'"' => st = St::Quoted,
            St::Quoted if c == b'"' => st = St::QuoteEnd,
            St::QuoteEnd if c == b'"' => st = St::Quoted,
            St::Quoted => {}
            _ => st = St::Plain,
        }
    }
    rec.len()
}

/// Raw byte spans of the fields of one record (line end excluded).
pub fn spans(rec: &[u8], d: u8) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut a = 0;
    loop {
        let z = span_end(rec, d, a);
        out.push((a, z));
        if z >= rec.len() {
            return out;
        }
        a = z + 1;
    }
}

/// Raw field `k` of a record, without allocating.
fn raw_field(rec: &[u8], d: u8, k: usize) -> &[u8] {
    let mut a = 0;
    for _ in 0..k {
        let z = span_end(rec, d, a);
        if z >= rec.len() {
            return &[];
        }
        a = z + 1;
    }
    &rec[a..span_end(rec, d, a)]
}

/// Field bytes with quotes removed and `""` unescaped; borrowed unless the
/// field was quoted.
fn unquote(raw: &[u8]) -> Cow<'_, [u8]> {
    if raw.first() != Some(&b'"') {
        return Cow::Borrowed(raw);
    }
    let mut out = Vec::with_capacity(raw.len());
    let mut i = 1;
    while i < raw.len() {
        let c = raw[i];
        if c == b'"' {
            if raw.get(i + 1) == Some(&b'"') {
                out.push(b'"');
                i += 2;
                continue;
            }
            // Text after a closing quote is kept as written.
            out.extend_from_slice(&raw[i + 1..]);
            break;
        }
        out.push(c);
        i += 1;
    }
    Cow::Owned(out)
}

/// The value of a raw field: quotes removed and `""` unescaped.
pub fn value(raw: &[u8]) -> String {
    String::from_utf8_lossy(&unquote(raw)).into_owned()
}

pub fn fields(rec: &[u8], d: u8) -> Vec<String> {
    spans(rec, d).into_iter().map(|(a, z)| value(&rec[a..z])).collect()
}

/// A value written as a field, quoted the way Excel quotes it.
pub fn quote(v: &str, d: u8) -> Vec<u8> {
    let b = v.as_bytes();
    if !b.iter().any(|&c| c == d || c == b'"' || c == b'\n' || c == b'\r') {
        return b.to_vec();
    }
    let mut out = Vec::with_capacity(b.len() + 2);
    out.push(b'"');
    for &c in b {
        if c == b'"' {
            out.push(b'"');
        }
        out.push(c);
    }
    out.push(b'"');
    out
}

/// The delimiter whose field count is most consistent over the first records.
pub fn sniff(sample: &[u8]) -> u8 {
    let cut = sample.iter().rposition(|&c| c == b'\n').map(|p| p + 1).unwrap_or(sample.len());
    let sample = &sample[..cut];
    let mut best = (0usize, b',');
    for &(d, _) in &DELIMS {
        let (starts, _) = index(sample, d);
        let n = (starts.len() - 1).min(50);
        let counts: Vec<usize> = (0..n).map(|k| spans(trim_eol(&sample[starts[k] as usize..starts[k + 1] as usize]), d).len()).collect();
        let (mut mode, mut seen) = (0, 0);
        for &c in &counts {
            let k = counts.iter().filter(|&&x| x == c).count();
            if c > 1 && (k > seen || (k == seen && c > mode)) {
                mode = c;
                seen = k;
            }
        }
        let score = seen * 1000 + mode;
        if score > best.0 {
            best = (score, d);
        }
    }
    best.1
}

fn trim_eol(r: &[u8]) -> &[u8] {
    let r = r.strip_suffix(b"\n").unwrap_or(r);
    r.strip_suffix(b"\r").unwrap_or(r)
}

/// Every record of `text` split into values.
pub fn parse_rows(text: &[u8], d: u8) -> Vec<Vec<String>> {
    let (starts, _) = index(text, d);
    if text.is_empty() {
        return Vec::new();
    }
    (0..starts.len() - 1).map(|i| fields(trim_eol(&text[starts[i] as usize..starts[i + 1] as usize]), d)).collect()
}

// ---- values ----------------------------------------------------------------------

/// A number that reads the same once a spreadsheet stores it: no leading
/// zeros, no trailing fractional zeros, at most 15 digits.
pub fn is_plain_number(v: &str) -> bool {
    let s = v.strip_prefix('-').unwrap_or(v);
    let (int, frac) = match s.split_once('.') {
        Some((i, f)) => (i, Some(f)),
        None => (s, None),
    };
    let digits = |t: &str| !t.is_empty() && t.bytes().all(|c| c.is_ascii_digit());
    digits(int)
        && (int == "0" || !int.starts_with('0'))
        && frac.map_or(true, |f| digits(f) && !f.ends_with('0'))
        && int.len() + frac.map_or(0, |f| f.len()) <= 15
}

/// Numeric value for sorting, filters and right alignment.
pub fn num(v: &str) -> Option<f64> {
    let t = v.trim();
    let first = t.bytes().next()?;
    if !(first.is_ascii_digit() || matches!(first, b'-' | b'+' | b'.')) {
        return None;
    }
    t.parse().ok()
}

fn num_bytes(v: &[u8]) -> Option<f64> {
    num(std::str::from_utf8(v).ok()?)
}

/// Sort key of one cell. Blank cells sort last, numbers before text, text
/// ignores case; keys borrow the document text.
enum Key<'a> {
    Num(f64),
    Text(Cow<'a, [u8]>),
    Blank,
}

impl<'a> Key<'a> {
    fn of(v: Cow<'a, [u8]>) -> Key<'a> {
        if v.trim_ascii().is_empty() {
            Key::Blank
        } else if let Some(n) = num_bytes(&v) {
            Key::Num(n)
        } else {
            Key::Text(v)
        }
    }

    fn cmp(&self, o: &Key) -> Ordering {
        match (self, o) {
            (Key::Num(a), Key::Num(b)) => a.partial_cmp(b).unwrap_or(Ordering::Equal),
            (Key::Text(a), Key::Text(b)) => a.iter().map(u8::to_ascii_lowercase).cmp(b.iter().map(u8::to_ascii_lowercase)),
            (Key::Num(_), _) | (Key::Text(_), Key::Blank) => Ordering::Less,
            (Key::Blank, Key::Blank) => Ordering::Equal,
            _ => Ordering::Greater,
        }
    }
}

/// One column filter. Plain text matches anywhere (any case); `=x` the
/// whole value; `!x` excludes; `>`, `<`, `>=`, `<=` compare numbers.
enum Test {
    Has(String),
    Not(String),
    Eq(String),
    Cmp(&'static str, f64),
}

fn compile(f: &str) -> Option<Test> {
    let f = f.trim();
    if f.is_empty() {
        return None;
    }
    for op in [">=", "<=", ">", "<"] {
        if let Some(n) = f.strip_prefix(op).and_then(|r| r.trim().parse::<f64>().ok()) {
            return Some(Test::Cmp(op, n));
        }
    }
    Some(if let Some(r) = f.strip_prefix('!') {
        Test::Not(r.trim().to_lowercase())
    } else if let Some(r) = f.strip_prefix('=') {
        Test::Eq(r.trim().to_lowercase())
    } else {
        Test::Has(f.to_lowercase())
    })
}

/// Case-insensitive substring test on bytes; `needle` is lowercase. ASCII
/// needles need no allocation; others fold the cell's case first.
fn contains_ci(hay: &[u8], needle: &str) -> bool {
    let n = needle.as_bytes();
    if n.is_empty() {
        return true;
    }
    if !needle.is_ascii() {
        return String::from_utf8_lossy(hay).to_lowercase().contains(needle);
    }
    hay.len() >= n.len() && hay.windows(n.len()).any(|w| w.iter().zip(n).all(|(a, b)| a.to_ascii_lowercase() == *b))
}

fn passes(t: &Test, v: &[u8]) -> bool {
    match t {
        Test::Has(s) => contains_ci(v, s),
        Test::Not(s) => s.is_empty() || !contains_ci(v, s),
        Test::Eq(s) => {
            let v = v.trim_ascii();
            if s.is_ascii() {
                v.eq_ignore_ascii_case(s.as_bytes())
            } else {
                String::from_utf8_lossy(v).to_lowercase() == *s
            }
        }
        Test::Cmp(op, n) => num_bytes(v).map_or(false, |x| match *op {
            ">=" => x >= *n,
            "<=" => x <= *n,
            ">" => x > *n,
            _ => x < *n,
        }),
    }
}

/// Appends one value to tab-separated text, quoted when Excel needs it.
fn push_tsv(out: &mut String, v: &str) {
    if v.contains(['\t', '\n', '\r']) || v.starts_with('"') {
        out.push('"');
        out.push_str(&v.replace('"', "\"\""));
        out.push('"');
    } else {
        out.push_str(v);
    }
}

// ---- clipboard formats -----------------------------------------------------------

/// Tab-separated text with CRLF rows; Excel splits it into columns.
pub fn to_tsv(rows: &[Vec<String>]) -> String {
    let mut out = String::new();
    for r in rows {
        for (k, v) in r.iter().enumerate() {
            if k > 0 {
                out.push('\t');
            }
            push_tsv(&mut out, v);
        }
        out.push_str("\r\n");
    }
    out
}

fn esc_xml(s: &str, out: &mut String) {
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\r' => {}
            '\n' => out.push_str("&#10;"),
            _ => out.push(c),
        }
    }
}

/// Excel's own "XML Spreadsheet" clipboard format. Every value that is not
/// a plain number goes in as text, so `0012`, `1-2` and `6"` paste exactly
/// as they are in the file instead of becoming 12, a date, or a formula.
pub fn to_xml_spreadsheet(rows: &[Vec<String>]) -> Option<String> {
    if rows.iter().map(|r| r.len()).sum::<usize>() > RICH_CELLS {
        return None;
    }
    let mut out = String::from(
        "<?xml version=\"1.0\"?>\r\n<Workbook xmlns=\"urn:schemas-microsoft-com:office:spreadsheet\" xmlns:ss=\"urn:schemas-microsoft-com:office:spreadsheet\"><Worksheet ss:Name=\"Sheet1\"><Table>",
    );
    for r in rows {
        out.push_str("<Row>");
        for v in r {
            if v.is_empty() {
                out.push_str("<Cell/>");
            } else if is_plain_number(v) {
                out.push_str("<Cell><Data ss:Type=\"Number\">");
                out.push_str(v);
                out.push_str("</Data></Cell>");
            } else {
                out.push_str("<Cell><Data ss:Type=\"String\">");
                esc_xml(v, &mut out);
                out.push_str("</Data></Cell>");
            }
        }
        out.push_str("</Row>");
    }
    out.push_str("</Table></Worksheet></Workbook>");
    Some(out)
}

/// A CF_HTML table, read by Google Sheets, LibreOffice and browsers. Text
/// cells carry Excel's text number format for the same reason as above.
pub fn to_html(rows: &[Vec<String>]) -> Option<String> {
    if rows.iter().map(|r| r.len()).sum::<usize>() > RICH_CELLS {
        return None;
    }
    let mut frag = String::from("<table>");
    for r in rows {
        frag.push_str("<tr>");
        for v in r {
            if v.is_empty() || is_plain_number(v) {
                frag.push_str("<td>");
            } else {
                frag.push_str("<td style='mso-number-format:\"\\@\"'>");
            }
            let mut e = String::new();
            esc_xml(v, &mut e);
            frag.push_str(&e.replace("&#10;", "<br style='mso-data-placement:same-cell'>"));
            frag.push_str("</td>");
        }
        frag.push_str("</tr>");
    }
    frag.push_str("</table>");
    let pre = "<html><head><meta charset=\"utf-8\"></head><body><!--StartFragment-->";
    let post = "<!--EndFragment--></body></html>";
    let head_len = "Version:0.9\r\nStartHTML:0000000000\r\nEndHTML:0000000000\r\nStartFragment:0000000000\r\nEndFragment:0000000000\r\n".len();
    let sf = head_len + pre.len();
    let ef = sf + frag.len();
    let end = ef + post.len();
    Some(format!(
        "Version:0.9\r\nStartHTML:{head_len:010}\r\nEndHTML:{end:010}\r\nStartFragment:{sf:010}\r\nEndFragment:{ef:010}\r\n{pre}{frag}{post}"
    ))
}

// ---- the grid ------------------------------------------------------------------------

pub struct Grid {
    pub delim: u8,
    /// Buffer generation the index was built from.
    pub gen: u64,
    /// Record start offsets plus an end sentinel.
    pub starts: Vec<u32>,
    pub ncols: usize,
    pub header: bool,
    pub widths: Vec<f32>,
    pub filters: Vec<Input>,
    pub sort: Option<(usize, bool)>,
    /// Records shown, in display order (header excluded).
    pub order: Vec<u32>,
    /// First shown row at the top of the view, and horizontal scroll.
    pub top: usize,
    pub x: f32,
    /// Selection as (shown row, column): `cur` moves, `anchor` stays.
    pub cur: (usize, usize),
    pub anchor: (usize, usize),
    /// The selection was made from column headers (copies include them).
    pub whole_cols: bool,
    /// Cell being edited: (shown row, column, text).
    pub edit: Option<(usize, usize, Input)>,
    /// Column whose filter box has the keyboard.
    pub filter_focus: Option<usize>,
}

impl Grid {
    pub fn new(doc: &mut Doc, gen: u64, delim: u8) -> Grid {
        let mut g = Grid {
            delim,
            gen,
            starts: vec![0, 0],
            ncols: 1,
            header: true,
            widths: Vec::new(),
            filters: Vec::new(),
            sort: None,
            order: Vec::new(),
            top: 0,
            x: 0.0,
            cur: (0, 0),
            anchor: (0, 0),
            whole_cols: false,
            edit: None,
            filter_focus: None,
        };
        g.reindex(doc, gen);
        g.autosize(doc);
        g
    }

    /// Rebuilds the record index after the text changed; filters and sort stay.
    pub fn reindex(&mut self, doc: &mut Doc, gen: u64) {
        let (starts, n) = index(doc.gap.contiguous(), self.delim);
        self.starts = starts;
        self.ncols = n.max(1);
        self.gen = gen;
        self.widths.resize(self.ncols, 96.0);
        self.filters.resize(self.ncols, Input::default());
        self.refilter(doc);
    }

    pub fn records(&self) -> usize {
        self.starts.len() - 1
    }

    pub fn first_data(&self) -> usize {
        (self.header && self.records() > 1) as usize
    }

    /// Record `i` without its line end.
    pub fn record(&self, doc: &Doc, i: usize) -> Vec<u8> {
        let mut r = doc.text(self.starts[i] as usize, self.starts[i + 1] as usize);
        let n = trim_eol(&r).len();
        r.truncate(n);
        r
    }

    pub fn row(&self, doc: &Doc, i: usize) -> Vec<String> {
        fields(&self.record(doc, i), self.delim)
    }

    pub fn header_names(&self, doc: &Doc) -> Vec<String> {
        let mut h = if self.first_data() == 1 { self.row(doc, 0) } else { Vec::new() };
        h.resize(self.ncols, String::new());
        for (k, v) in h.iter_mut().enumerate() {
            if v.is_empty() {
                *v = col_letters(k);
            }
        }
        h
    }

    /// Recomputes the shown rows from the filters and sort order. Reads
    /// field bytes in place: a million rows filter in well under a second.
    pub fn refilter(&mut self, doc: &mut Doc) {
        let tests: Vec<(usize, Test)> = self.filters.iter().enumerate().filter_map(|(k, f)| compile(&f.text).map(|t| (k, t))).collect();
        let first = self.first_data();
        let d = self.delim;
        let text = doc.gap.contiguous();
        let rec = |i: usize| trim_eol(&text[self.starts[i] as usize..self.starts[i + 1] as usize]);
        let mut order: Vec<u32> = Vec::with_capacity(self.records() - first);
        let mut keys: Vec<Key> = Vec::new();
        for i in first..self.records() {
            if !tests.is_empty() {
                let r = rec(i);
                if !tests.iter().all(|(k, t)| passes(t, &unquote(raw_field(r, d, *k)))) {
                    continue;
                }
            }
            if let Some((c, _)) = self.sort {
                keys.push(Key::of(unquote(raw_field(rec(i), d, c))));
            }
            order.push(i as u32);
        }
        if let Some((_, asc)) = self.sort {
            let mut idx: Vec<u32> = (0..order.len() as u32).collect();
            idx.sort_by(|&a, &b| {
                let (ka, kb) = (&keys[a as usize], &keys[b as usize]);
                let o = ka.cmp(kb);
                // Blank cells stay last either way.
                if asc || matches!(ka, Key::Blank) || matches!(kb, Key::Blank) {
                    o
                } else {
                    o.reverse()
                }
            });
            order = idx.into_iter().map(|k| order[k as usize]).collect();
        }
        self.order = order;
        let last = self.order.len().saturating_sub(1);
        self.cur.0 = self.cur.0.min(last);
        self.anchor.0 = self.anchor.0.min(last);
        self.cur.1 = self.cur.1.min(self.ncols - 1);
        self.anchor.1 = self.anchor.1.min(self.ncols - 1);
        self.top = self.top.min(last);
    }

    pub fn filtered(&self) -> bool {
        self.filters.iter().any(|f| !f.text.trim().is_empty())
    }

    /// Column widths estimated from the header and the first records.
    pub fn autosize(&mut self, doc: &Doc) {
        let mut chars = vec![3usize; self.ncols];
        for (k, h) in self.header_names(doc).iter().enumerate() {
            chars[k] = chars[k].max(h.chars().count() + 3);
        }
        for i in self.first_data()..self.records().min(300) {
            for (k, v) in self.row(doc, i).iter().enumerate().take(self.ncols) {
                chars[k] = chars[k].max(v.chars().count());
            }
        }
        self.widths = chars.iter().map(|&n| (n as f32 * 7.2 + 22.0).clamp(56.0, 360.0)).collect();
    }

    /// Selected block: (first row, last row, first col, last col).
    pub fn block(&self) -> (usize, usize, usize, usize) {
        (self.cur.0.min(self.anchor.0), self.cur.0.max(self.anchor.0), self.cur.1.min(self.anchor.1), self.cur.1.max(self.anchor.1))
    }

    pub fn is_selected(&self, r: usize, c: usize) -> bool {
        let (r0, r1, c0, c1) = self.block();
        r >= r0 && r <= r1 && c >= c0 && c <= c1
    }

    /// Values of the selected block, with the header row on top when whole
    /// columns are selected or `headers` asks for it.
    pub fn selection_values(&self, doc: &Doc, headers: bool) -> Vec<Vec<String>> {
        let (r0, r1, c0, c1) = self.block();
        let pick = |row: Vec<String>| (c0..=c1).map(|k| row.get(k).cloned().unwrap_or_default()).collect::<Vec<_>>();
        let mut out = Vec::new();
        if (headers || self.whole_cols) && self.first_data() == 1 {
            out.push(pick(self.row(doc, 0)));
        }
        if self.order.is_empty() {
            return out;
        }
        for r in r0..=r1.min(self.order.len() - 1) {
            out.push(pick(self.row(doc, self.order[r] as usize)));
        }
        out
    }

    /// The selected block as tab-separated text, built straight from the
    /// document; for copies too large for the rich formats.
    pub fn selection_tsv(&self, doc: &mut Doc, headers: bool) -> String {
        let (r0, r1, c0, c1) = self.block();
        let d = self.delim;
        let text = doc.gap.contiguous();
        let mut out = String::new();
        let mut put = |i: usize| {
            let rec = trim_eol(&text[self.starts[i] as usize..self.starts[i + 1] as usize]);
            let mut a = 0;
            for c in 0..=c1 {
                let z = if a > rec.len() { a } else { span_end(rec, d, a) };
                if c > c0 {
                    out.push('\t');
                }
                if c >= c0 && a < z {
                    push_tsv(&mut out, &String::from_utf8_lossy(&unquote(&rec[a..z])));
                }
                a = z + 1;
            }
            out.push_str("\r\n");
        };
        if (headers || self.whole_cols) && self.first_data() == 1 {
            put(0);
        }
        if !self.order.is_empty() {
            for r in r0..=r1.min(self.order.len() - 1) {
                put(self.order[r] as usize);
            }
        }
        out
    }

    /// Cells in the selected block, counting a header row when copied.
    pub fn selection_cells(&self, headers: bool) -> usize {
        let (r0, r1, c0, c1) = self.block();
        let rows = if self.order.is_empty() { 0 } else { r1.min(self.order.len() - 1) + 1 - r0 };
        (rows + ((headers || self.whole_cols) && self.first_data() == 1) as usize) * (c1 - c0 + 1)
    }

    /// Byte changes that set cells `(record, col) = value`; untouched fields
    /// keep their exact bytes. Records are rewritten whole, one change each.
    pub fn cell_changes(&self, doc: &Doc, cells: &[(usize, usize, String)]) -> Vec<(usize, usize, Vec<u8>)> {
        let mut by_rec: Vec<(usize, Vec<(usize, &str)>)> = Vec::new();
        for (r, c, v) in cells {
            match by_rec.iter_mut().find(|x| x.0 == *r) {
                Some(x) => x.1.push((*c, v)),
                None => by_rec.push((*r, vec![(*c, v)])),
            }
        }
        by_rec.sort_by_key(|x| x.0);
        let d = self.delim;
        let mut out = Vec::new();
        for (r, sets) in by_rec {
            let rec = self.record(doc, r);
            let sp = spans(&rec, d);
            let width = sp.len().max(sets.iter().map(|s| s.0 + 1).max().unwrap_or(0));
            let mut new = Vec::with_capacity(rec.len() + 16);
            for k in 0..width {
                if k > 0 {
                    new.push(d);
                }
                match sets.iter().rev().find(|s| s.0 == k) {
                    Some((_, v)) => new.extend_from_slice(&quote(v, d)),
                    None => {
                        if let Some(&(a, z)) = sp.get(k) {
                            new.extend_from_slice(&rec[a..z]);
                        }
                    }
                }
            }
            let a = self.starts[r] as usize;
            out.push((a, a + rec.len(), new));
        }
        out
    }

    /// Moves record offsets after edits made by `cell_changes`, so a cell
    /// edit costs no rescan of the file.
    pub fn shift_after(&mut self, changes: &[(usize, usize, Vec<u8>)], gen: u64, doc_len: usize) {
        let mut d: i64 = 0;
        let mut k = 0;
        for s in self.starts.iter_mut() {
            while k < changes.len() && changes[k].0 < *s as usize {
                d += changes[k].2.len() as i64 - (changes[k].1 - changes[k].0) as i64;
                k += 1;
            }
            *s = (*s as i64 + d) as u32;
        }
        *self.starts.last_mut().unwrap() = doc_len as u32;
        self.gen = gen;
    }
}

/// Spreadsheet column name: A, B, ... Z, AA, AB, ...
pub fn col_letters(mut k: usize) -> String {
    let mut s = Vec::new();
    loop {
        s.push(b'A' + (k % 26) as u8);
        if k < 26 {
            break;
        }
        k = k / 26 - 1;
    }
    s.reverse();
    String::from_utf8(s).unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn index_handles_quotes_newlines_and_inch_marks() {
        let t = b"a,b\n\"x\ny\",2\n6\" PIPE,3\n\"q\"\"t\",4";
        let (s, n) = index(t, b',');
        assert_eq!(n, 2);
        assert_eq!(s.len() - 1, 4);
        let rec = |i: usize| trim_eol(&t[s[i] as usize..s[i + 1] as usize]).to_vec();
        assert_eq!(fields(&rec(1), b','), vec!["x\ny", "2"]);
        assert_eq!(fields(&rec(2), b','), vec!["6\" PIPE", "3"]);
        assert_eq!(fields(&rec(3), b','), vec!["q\"t", "4"]);
        let (s, _) = index(b"a\nb\n", b',');
        assert_eq!(s, vec![0, 2, 4]);
        let (s, _) = index(b"", b',');
        assert_eq!(s, vec![0, 0]);
    }

    #[test]
    fn sniffs_delimiters() {
        assert_eq!(sniff(b"a,b,c\n1,2,3\n"), b',');
        assert_eq!(sniff(b"a;b;c\n1,5;2;3\n"), b';');
        assert_eq!(sniff(b"a\tb\n1\t2\n"), b'\t');
        assert_eq!(sniff(b"LINE|SIZE|SPEC\n1001|6\"|A1\n"), b'|');
    }

    #[test]
    fn quoting_round_trips() {
        for v in ["plain", "a,b", "6\" pipe", "two\nlines", ""] {
            let q = quote(v, b',');
            assert_eq!(value(&q), v);
            assert_eq!(spans(&q, b',').len(), 1);
        }
    }

    #[test]
    fn plain_numbers_and_filters() {
        assert!(is_plain_number("12") && is_plain_number("-0.5") && is_plain_number("0"));
        assert!(!is_plain_number("0012") && !is_plain_number("1.50") && !is_plain_number("1-2") && !is_plain_number("1234567890123456"));
        let t = |f: &str, v: &str| passes(&compile(f).unwrap(), v.as_bytes());
        assert!(t("cs", "A106 CS") && !t("!cs", "A106 CS") && t("=a1", " A1 ") && !t("=a1", "A10"));
        assert!(t(">=6", "6") && t("<2.5", "2") && !t(">6", "abc"));
        assert!(compile("  ").is_none());
    }

    #[test]
    fn excel_formats_keep_text() {
        let rows = vec![vec!["0012".to_string(), "1-2".into(), "42".into(), "a\tb".into()]];
        assert_eq!(to_tsv(&rows), "0012\t1-2\t42\t\"a\tb\"\r\n");
        let x = to_xml_spreadsheet(&rows).unwrap();
        assert!(x.contains("<Data ss:Type=\"String\">0012</Data>") && x.contains("<Data ss:Type=\"Number\">42</Data>"));
        let h = to_html(&rows).unwrap();
        let num = |k: &str| h[h.find(k).unwrap() + k.len()..][..10].parse::<usize>().unwrap();
        assert!(h[num("StartFragment:")..num("EndFragment:")].starts_with("<table>"));
        assert!(h[num("StartFragment:")..num("EndFragment:")].ends_with("</table>"));
        assert_eq!(num("EndHTML:"), h.len());
    }

    /// Timing on a large generated table; runs when SLATE_HUGE is set.
    #[test]
    fn huge_table_timings() {
        if std::env::var_os("SLATE_HUGE").is_none() {
            return;
        }
        let mut t = Vec::with_capacity(110 << 20);
        t.extend_from_slice(b"LINE NO,SIZE,SPEC,FROM,TO,TEMP C,PRESS BARG,ISO NO,REMARKS\n");
        for i in 0..1_000_000u32 {
            let spec = ["A1A", "B2", "C1", "D4"][(i % 4) as usize];
            t.extend_from_slice(format!("{}\"-P-{i:07}-{spec},{}\",{spec},P-{},E-{},{},{}.5,{:04},\"note, row {i}\"\n", i % 24 + 1, i % 24 + 1, i % 977, i % 613, i % 400, i % 90, i % 9999).as_bytes());
        }
        let mb = t.len() as f64 / 1048576.0;
        let mut doc = Doc::load(t);
        let t0 = std::time::Instant::now();
        let mut g = Grid::new(&mut doc, 1, b',');
        let open = t0.elapsed();
        g.filters[2].set("a1a");
        let t0 = std::time::Instant::now();
        g.refilter(&mut doc);
        let filter = t0.elapsed();
        g.sort = Some((5, false));
        let t0 = std::time::Instant::now();
        g.refilter(&mut doc);
        let sort = t0.elapsed();
        g.anchor = (0, 0);
        g.cur = (g.order.len() - 1, g.ncols - 1);
        let t0 = std::time::Instant::now();
        let tsv = g.selection_tsv(&mut doc, true);
        let copy = t0.elapsed();
        eprintln!(
            "{mb:.0} MB, {} records: grid open {open:?}, filter {filter:?} ({} rows), filter+sort {sort:?}, copy {copy:?} ({} MB TSV)",
            g.records(),
            g.order.len(),
            tsv.len() >> 20
        );
        assert_eq!(g.order.len(), 250_000);
        assert!(tsv.starts_with("LINE NO\tSIZE"));
        // Descending temperature, blanks last: the first row is the hottest.
        assert_eq!(g.row(&doc, g.order[0] as usize)[5], "396");
    }

    #[test]
    fn grid_edits_shift_offsets_and_sort() {
        let mut doc = Doc::load(b"name,size\nb,10\na,9\nc,\n".to_vec());
        let mut g = Grid::new(&mut doc, 1, b',');
        assert_eq!(g.order, vec![1, 2, 3]);
        g.sort = Some((1, true));
        g.refilter(&mut doc);
        assert_eq!(g.order, vec![2, 1, 3]);
        g.sort = Some((1, false));
        g.refilter(&mut doc);
        assert_eq!(g.order, vec![1, 2, 3]);
        g.filters[0].set("!b");
        g.refilter(&mut doc);
        assert_eq!(g.order, vec![2, 3]);
        let ch = g.cell_changes(&doc, &[(1, 0, "x,y".into())]);
        for (a, z, t) in ch.iter().rev() {
            doc.remove_at(*a, z - a);
            doc.insert_at(*a, t);
        }
        g.shift_after(&ch, 2, doc.len());
        assert_eq!(doc.text(0, doc.len()), b"name,size\n\"x,y\",10\na,9\nc,\n");
        assert_eq!(g.row(&doc, 2), vec!["a", "9"]);
        let fresh = index(doc.gap.contiguous(), b',').0;
        assert_eq!(g.starts, fresh);
        // Setting a column past the end pads the record.
        let ch = g.cell_changes(&doc, &[(3, 3, "z".into())]);
        assert_eq!(ch[0].2, b"c,,,z");
        assert_eq!(col_letters(0) + &col_letters(27), "AAB");
    }
}
