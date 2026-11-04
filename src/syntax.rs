//! Table-driven line lexer. Each line ends in a one-byte state (inside a
//! block comment, multi-line string, open tag...) so highlighting needs only
//! the state of the line above, never a rescan of the whole file.

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Tok {
    Text,
    Keyword,
    Type,
    Str,
    Num,
    Comment,
    Pre,
    Tag,
    Attr,
    Op,
    Heading,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Plain,
    Code,
    Markup,
    Ini,
    Markdown,
}

pub struct Lang {
    pub name: &'static str,
    pub exts: &'static [&'static str],
    pub line: &'static [&'static str],
    pub block: Option<(&'static str, &'static str)>,
    pub quotes: &'static [u8],
    /// Python-style `"""` / `'''` strings span lines.
    pub triple: bool,
    /// `#include`-style lines and `@attr`/`#[attr]` prefixes.
    pub pre: &'static [&'static str],
    pub keywords: &'static [&'static str],
    pub types: &'static [&'static str],
    pub nocase: bool,
    pub kind: Kind,
}

const ST_NORMAL: u8 = 0;
const ST_BLOCK: u8 = 1;
const ST_TRIPLE_DQ: u8 = 2;
const ST_TRIPLE_SQ: u8 = 3;
const ST_TEMPLATE: u8 = 4;
const ST_TAG: u8 = 5;
const ST_FENCE: u8 = 6;

const C_KW: &[&str] = &[
    "auto", "break", "case", "catch", "class", "const", "constexpr", "continue", "default", "delete", "do", "else",
    "enum", "explicit", "extern", "false", "for", "friend", "goto", "if", "inline", "namespace", "new", "noexcept",
    "nullptr", "operator", "private", "protected", "public", "register", "return", "sizeof", "static", "struct",
    "switch", "template", "this", "throw", "true", "try", "typedef", "typename", "union", "using", "virtual",
    "volatile", "while", "override", "final", "static_cast", "dynamic_cast", "reinterpret_cast", "const_cast",
];
const C_TY: &[&str] = &[
    "bool", "char", "double", "float", "int", "long", "short", "signed", "unsigned", "void", "size_t", "wchar_t",
    "int8_t", "int16_t", "int32_t", "int64_t", "uint8_t", "uint16_t", "uint32_t", "uint64_t", "std", "string",
    "vector", "BOOL", "DWORD", "HANDLE", "HWND", "LPARAM", "WPARAM", "LRESULT", "UINT", "TCHAR", "LPCWSTR",
];
const CS_KW: &[&str] = &[
    "abstract", "as", "async", "await", "base", "break", "case", "catch", "checked", "class", "const", "continue",
    "default", "delegate", "do", "else", "enum", "event", "explicit", "extern", "false", "finally", "fixed", "for",
    "foreach", "get", "goto", "if", "implicit", "in", "interface", "internal", "is", "lock", "namespace", "new",
    "null", "operator", "out", "override", "params", "private", "protected", "public", "readonly", "record", "ref",
    "return", "sealed", "set", "sizeof", "static", "struct", "switch", "this", "throw", "true", "try", "typeof",
    "using", "var", "virtual", "void", "volatile", "while", "yield", "init", "where", "partial", "with",
];
const CS_TY: &[&str] = &[
    "bool", "byte", "char", "decimal", "double", "float", "int", "long", "object", "sbyte", "short", "string",
    "uint", "ulong", "ushort", "dynamic", "String", "List", "Dictionary", "Task", "IEnumerable",
];
const JAVA_KW: &[&str] = &[
    "abstract", "assert", "break", "case", "catch", "class", "const", "continue", "default", "do", "else", "enum",
    "extends", "false", "final", "finally", "for", "if", "implements", "import", "instanceof", "interface",
    "native", "new", "null", "package", "private", "protected", "public", "return", "static", "super", "switch",
    "synchronized", "this", "throw", "throws", "true", "try", "var", "void", "volatile", "while", "record",
    "fun", "val", "when", "object", "override", "data", "sealed", "open", "is", "in", "companion", "suspend",
];
const JAVA_TY: &[&str] = &[
    "boolean", "byte", "char", "double", "float", "int", "long", "short", "String", "Integer", "Long", "List",
    "Map", "Object", "Int", "Unit", "Any",
];
const JS_KW: &[&str] = &[
    "async", "await", "break", "case", "catch", "class", "const", "continue", "debugger", "default", "delete",
    "do", "else", "export", "extends", "false", "finally", "for", "from", "function", "if", "import", "in",
    "instanceof", "let", "new", "null", "of", "return", "static", "super", "switch", "this", "throw", "true",
    "try", "typeof", "undefined", "var", "void", "while", "with", "yield", "as", "interface", "type", "enum",
    "implements", "private", "public", "protected", "readonly", "declare", "namespace", "abstract", "keyof",
];
const JS_TY: &[&str] = &[
    "string", "number", "boolean", "any", "unknown", "never", "object", "Array", "Promise", "Map", "Set",
    "Record", "Date", "Error", "JSON", "Math", "console", "window", "document",
];
const RS_KW: &[&str] = &[
    "as", "async", "await", "break", "const", "continue", "crate", "dyn", "else", "enum", "extern", "false", "fn",
    "for", "if", "impl", "in", "let", "loop", "match", "mod", "move", "mut", "pub", "ref", "return", "self",
    "Self", "static", "struct", "super", "trait", "true", "type", "unsafe", "use", "where", "while",
];
const RS_TY: &[&str] = &[
    "bool", "char", "f32", "f64", "i8", "i16", "i32", "i64", "i128", "isize", "str", "u8", "u16", "u32", "u64",
    "u128", "usize", "String", "Vec", "Option", "Result", "Box", "Some", "None", "Ok", "Err", "HashMap", "Rc",
    "Arc",
];
const GO_KW: &[&str] = &[
    "break", "case", "chan", "const", "continue", "default", "defer", "else", "fallthrough", "for", "func", "go",
    "goto", "if", "import", "interface", "map", "package", "range", "return", "select", "struct", "switch",
    "type", "var", "true", "false", "nil",
];
const GO_TY: &[&str] = &[
    "bool", "byte", "error", "float32", "float64", "int", "int8", "int16", "int32", "int64", "rune", "string",
    "uint", "uint8", "uint16", "uint32", "uint64", "uintptr", "any",
];
const PY_KW: &[&str] = &[
    "and", "as", "assert", "async", "await", "break", "class", "continue", "def", "del", "elif", "else", "except",
    "False", "finally", "for", "from", "global", "if", "import", "in", "is", "lambda", "None", "nonlocal", "not",
    "or", "pass", "raise", "return", "True", "try", "while", "with", "yield", "match", "case", "self",
];
const PY_TY: &[&str] = &[
    "int", "float", "str", "bool", "list", "dict", "set", "tuple", "bytes", "object", "len", "print", "range",
    "open", "isinstance", "super", "enumerate", "zip", "map", "filter", "sorted", "min", "max", "sum",
];
const PS_KW: &[&str] = &[
    "begin", "break", "catch", "class", "continue", "data", "do", "dynamicparam", "else", "elseif", "end", "exit",
    "filter", "finally", "for", "foreach", "from", "function", "if", "in", "param", "process", "return", "switch",
    "throw", "trap", "try", "until", "using", "while", "-eq", "-ne", "-gt", "-lt", "-ge", "-le", "-like",
    "-notlike", "-match", "-notmatch", "-and", "-or", "-not", "$true", "$false", "$null",
];
const BAT_KW: &[&str] = &[
    "call", "cd", "chdir", "cls", "copy", "del", "dir", "do", "echo", "else", "endlocal", "errorlevel", "exist",
    "exit", "for", "goto", "if", "in", "md", "mkdir", "move", "not", "off", "on", "pause", "popd", "pushd", "ren",
    "rd", "rmdir", "set", "setlocal", "shift", "start", "type", "xcopy", "equ", "neq", "lss", "leq", "gtr", "geq",
    "defined", "enabledelayedexpansion",
];
const SH_KW: &[&str] = &[
    "if", "then", "else", "elif", "fi", "for", "while", "until", "do", "done", "case", "esac", "in", "function",
    "return", "exit", "export", "local", "readonly", "echo", "set", "unset", "shift", "source", "cd", "true",
    "false",
];
const SQL_KW: &[&str] = &[
    "add", "all", "alter", "and", "as", "asc", "begin", "between", "by", "case", "check", "column", "commit",
    "constraint", "create", "cross", "database", "declare", "default", "delete", "desc", "distinct", "drop",
    "else", "end", "exec", "exists", "foreign", "from", "full", "group", "having", "if", "in", "index", "inner",
    "insert", "into", "is", "join", "key", "left", "like", "limit", "not", "null", "on", "or", "order", "outer",
    "primary", "procedure", "references", "right", "rollback", "select", "set", "table", "then", "top",
    "transaction", "truncate", "union", "unique", "update", "values", "view", "when", "where", "with",
];
const SQL_TY: &[&str] = &[
    "bigint", "bit", "char", "date", "datetime", "decimal", "float", "int", "integer", "money", "nchar",
    "numeric", "nvarchar", "real", "smallint", "text", "time", "varchar", "boolean", "count", "sum", "avg", "min",
    "max",
];
const JSON_KW: &[&str] = &["true", "false", "null"];
const CSS_KW: &[&str] = &["important", "media", "import", "keyframes", "font-face", "supports", "root"];
const LUA_KW: &[&str] = &[
    "and", "break", "do", "else", "elseif", "end", "false", "for", "function", "goto", "if", "in", "local", "nil",
    "not", "or", "repeat", "return", "then", "true", "until", "while",
];
const PHP_KW: &[&str] = &[
    "abstract", "and", "array", "as", "break", "case", "catch", "class", "clone", "const", "continue", "declare",
    "default", "do", "echo", "else", "elseif", "empty", "endfor", "endforeach", "endif", "endwhile", "extends",
    "final", "finally", "fn", "for", "foreach", "function", "global", "if", "implements", "include", "instanceof",
    "interface", "isset", "list", "match", "namespace", "new", "null", "or", "print", "private", "protected",
    "public", "require", "require_once", "return", "static", "switch", "throw", "trait", "try", "unset", "use",
    "var", "while", "true", "false",
];
const VB_KW: &[&str] = &[
    "and", "as", "boolean", "byref", "byval", "call", "case", "class", "const", "dim", "do", "each", "else",
    "elseif", "end", "exit", "false", "for", "function", "if", "in", "integer", "is", "loop", "me", "mod", "new",
    "next", "not", "nothing", "object", "on", "option", "or", "private", "property", "public", "redim", "resume",
    "return", "select", "set", "string", "sub", "then", "to", "true", "until", "wend", "while", "with",
];

const NONE: &[&str] = &[];

pub static LANGS: &[Lang] = &[
    Lang { name: "Plain Text", exts: &["txt", "log"], line: NONE, block: None, quotes: b"", triple: false, pre: NONE, keywords: NONE, types: NONE, nocase: false, kind: Kind::Plain },
    Lang { name: "C / C++", exts: &["c", "h", "cpp", "hpp", "cc", "cxx", "hh", "hxx", "ino", "inl"], line: &["//"], block: Some(("/*", "*/")), quotes: b"\"'", triple: false, pre: &["#"], keywords: C_KW, types: C_TY, nocase: false, kind: Kind::Code },
    Lang { name: "C#", exts: &["cs", "csx"], line: &["//"], block: Some(("/*", "*/")), quotes: b"\"'", triple: false, pre: &["#", "["], keywords: CS_KW, types: CS_TY, nocase: false, kind: Kind::Code },
    Lang { name: "Java / Kotlin", exts: &["java", "kt", "kts", "gradle", "groovy"], line: &["//"], block: Some(("/*", "*/")), quotes: b"\"'", triple: false, pre: &["@"], keywords: JAVA_KW, types: JAVA_TY, nocase: false, kind: Kind::Code },
    Lang { name: "JavaScript / TypeScript", exts: &["js", "mjs", "cjs", "jsx", "ts", "tsx", "mts"], line: &["//"], block: Some(("/*", "*/")), quotes: b"\"'`", triple: false, pre: &["@"], keywords: JS_KW, types: JS_TY, nocase: false, kind: Kind::Code },
    Lang { name: "Rust", exts: &["rs"], line: &["//"], block: Some(("/*", "*/")), quotes: b"\"", triple: false, pre: &["#[", "#!["], keywords: RS_KW, types: RS_TY, nocase: false, kind: Kind::Code },
    Lang { name: "Go", exts: &["go"], line: &["//"], block: Some(("/*", "*/")), quotes: b"\"'`", triple: false, pre: NONE, keywords: GO_KW, types: GO_TY, nocase: false, kind: Kind::Code },
    Lang { name: "Python", exts: &["py", "pyw", "pyi"], line: &["#"], block: None, quotes: b"\"'", triple: true, pre: &["@"], keywords: PY_KW, types: PY_TY, nocase: false, kind: Kind::Code },
    Lang { name: "PowerShell", exts: &["ps1", "psm1", "psd1"], line: &["#"], block: Some(("<#", "#>")), quotes: b"\"'", triple: false, pre: &["["], keywords: PS_KW, types: NONE, nocase: true, kind: Kind::Code },
    Lang { name: "Batch", exts: &["bat", "cmd"], line: &["rem ", "::", "@rem "], block: None, quotes: b"\"", triple: false, pre: &[":"], keywords: BAT_KW, types: NONE, nocase: true, kind: Kind::Code },
    Lang { name: "Shell", exts: &["sh", "bash", "zsh"], line: &["#"], block: None, quotes: b"\"'", triple: false, pre: NONE, keywords: SH_KW, types: NONE, nocase: false, kind: Kind::Code },
    Lang { name: "SQL", exts: &["sql"], line: &["--"], block: Some(("/*", "*/")), quotes: b"'\"", triple: false, pre: NONE, keywords: SQL_KW, types: SQL_TY, nocase: true, kind: Kind::Code },
    Lang { name: "JSON", exts: &["json", "jsonc", "geojson"], line: &["//"], block: Some(("/*", "*/")), quotes: b"\"", triple: false, pre: NONE, keywords: JSON_KW, types: NONE, nocase: false, kind: Kind::Code },
    Lang { name: "XML / HTML", exts: &["xml", "html", "htm", "xhtml", "xaml", "svg", "config", "csproj", "vcxproj", "props", "targets", "xsd", "xsl", "xslt", "resx", "manifest", "plist", "vue"], line: NONE, block: Some(("<!--", "-->")), quotes: b"\"'", triple: false, pre: NONE, keywords: NONE, types: NONE, nocase: false, kind: Kind::Markup },
    Lang { name: "CSS", exts: &["css", "scss", "sass", "less"], line: &["//"], block: Some(("/*", "*/")), quotes: b"\"'", triple: false, pre: &["@", "."], keywords: CSS_KW, types: NONE, nocase: true, kind: Kind::Code },
    Lang { name: "INI / TOML", exts: &["ini", "cfg", "conf", "toml", "properties", "reg", "inf", "editorconfig", "gitconfig", "env"], line: NONE, block: None, quotes: b"\"'", triple: false, pre: NONE, keywords: NONE, types: NONE, nocase: false, kind: Kind::Ini },
    Lang { name: "YAML", exts: &["yml", "yaml"], line: &["#"], block: None, quotes: b"\"'", triple: false, pre: NONE, keywords: NONE, types: NONE, nocase: false, kind: Kind::Ini },
    Lang { name: "Markdown", exts: &["md", "markdown", "mdx"], line: NONE, block: None, quotes: b"", triple: false, pre: NONE, keywords: NONE, types: NONE, nocase: false, kind: Kind::Markdown },
    Lang { name: "Lua", exts: &["lua"], line: &["--"], block: Some(("--[[", "]]")), quotes: b"\"'", triple: false, pre: NONE, keywords: LUA_KW, types: NONE, nocase: false, kind: Kind::Code },
    Lang { name: "PHP", exts: &["php", "phtml"], line: &["//", "#"], block: Some(("/*", "*/")), quotes: b"\"'", triple: false, pre: NONE, keywords: PHP_KW, types: NONE, nocase: true, kind: Kind::Code },
    Lang { name: "Visual Basic", exts: &["vb", "vbs", "bas", "frm", "cls", "vba"], line: &["'", "rem "], block: None, quotes: b"\"", triple: false, pre: NONE, keywords: VB_KW, types: NONE, nocase: true, kind: Kind::Code },
];

pub fn lang_for_path(path: &str) -> usize {
    let lower = path.to_ascii_lowercase();
    let file = lower.rsplit(['\\', '/']).next().unwrap_or("");
    if file == "makefile" || file == "dockerfile" || file.ends_with(".mk") {
        return LANGS.iter().position(|l| l.name == "Shell").unwrap_or(0);
    }
    let Some(ext) = file.rsplit_once('.').map(|(_, e)| e) else {
        return 0;
    };
    LANGS.iter().position(|l| l.exts.contains(&ext)).unwrap_or(0)
}

fn starts_with_at(s: &[u8], i: usize, pat: &str, nocase: bool) -> bool {
    let p = pat.as_bytes();
    if i + p.len() > s.len() {
        return false;
    }
    if nocase {
        s[i..i + p.len()].eq_ignore_ascii_case(p)
    } else {
        &s[i..i + p.len()] == p
    }
}

fn find_from(s: &[u8], i: usize, pat: &str) -> Option<usize> {
    let p = pat.as_bytes();
    if p.is_empty() || s.len() < p.len() {
        return None;
    }
    (i..=s.len() - p.len()).find(|&k| &s[k..k + p.len()] == p)
}

fn is_word(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'_' || c >= 0x80
}

fn word_in(list: &[&str], w: &[u8], nocase: bool) -> bool {
    list.iter().any(|k| if nocase { k.as_bytes().eq_ignore_ascii_case(w) } else { k.as_bytes() == w })
}

/// Lexes one line. Appends `(start, end, tok)` byte spans to `out` when given
/// and returns the state for the next line.
pub fn lex(lang: &Lang, s: &[u8], state: u8, mut out: Option<&mut Vec<(u32, u32, Tok)>>) -> u8 {
    let mut push = |a: usize, b: usize, t: Tok| {
        if let Some(o) = out.as_deref_mut() {
            if b > a && t != Tok::Text {
                o.push((a as u32, b as u32, t));
            }
        }
    };
    match lang.kind {
        Kind::Plain => return 0,
        Kind::Ini => {
            let t = s.iter().position(|c| !c.is_ascii_whitespace()).unwrap_or(s.len());
            if t < s.len() {
                match s[t] {
                    b';' | b'#' => push(t, s.len(), Tok::Comment),
                    b'[' => push(t, s.len(), Tok::Heading),
                    _ => {
                        if let Some(eq) = s.iter().position(|&c| c == b'=' || c == b':') {
                            push(t, eq, Tok::Attr);
                            push(eq, eq + 1, Tok::Op);
                            let v = &s[eq + 1..];
                            let vt = v.iter().position(|c| !c.is_ascii_whitespace()).unwrap_or(v.len());
                            if vt < v.len() && (v[vt] == b'"' || v[vt] == b'\'' || v[vt].is_ascii_digit()) {
                                let tok = if v[vt].is_ascii_digit() { Tok::Num } else { Tok::Str };
                                push(eq + 1 + vt, s.len(), tok);
                            }
                        }
                    }
                }
            }
            return 0;
        }
        Kind::Markdown => {
            let t = s.iter().position(|c| *c != b' ').unwrap_or(s.len());
            if starts_with_at(s, t, "```", false) || starts_with_at(s, t, "~~~", false) {
                push(0, s.len(), Tok::Pre);
                return if state == ST_FENCE { 0 } else { ST_FENCE };
            }
            if state == ST_FENCE {
                push(0, s.len(), Tok::Str);
                return ST_FENCE;
            }
            if t < s.len() && s[t] == b'#' {
                push(0, s.len(), Tok::Heading);
                return 0;
            }
            if t < s.len() && s[t] == b'>' {
                push(0, s.len(), Tok::Comment);
                return 0;
            }
            if t + 1 < s.len() && (s[t] == b'-' || s[t] == b'*' || s[t] == b'+') && s[t + 1] == b' ' {
                push(t, t + 1, Tok::Keyword);
            }
            let mut i = 0;
            while i < s.len() {
                if s[i] == b'`' {
                    let e = s[i + 1..].iter().position(|&c| c == b'`').map(|k| i + k + 2).unwrap_or(s.len());
                    push(i, e, Tok::Str);
                    i = e;
                } else if starts_with_at(s, i, "**", false) || starts_with_at(s, i, "__", false) {
                    let m = &s[i..i + 2];
                    let e = (i + 2..s.len().saturating_sub(1)).find(|&k| &s[k..k + 2] == m).map(|k| k + 2).unwrap_or(s.len());
                    push(i, e, Tok::Keyword);
                    i = e;
                } else if s[i] == b'[' {
                    let e = s[i..].iter().position(|&c| c == b')').map(|k| i + k + 1).unwrap_or(i + 1);
                    push(i, e, Tok::Type);
                    i = e;
                } else {
                    i += 1;
                }
            }
            return 0;
        }
        Kind::Markup => return lex_markup(s, state, &mut push),
        Kind::Code => {}
    }

    let mut i = 0;
    let mut st = state;
    // Continue a construct opened on an earlier line.
    match st {
        ST_BLOCK => {
            let (_, close) = lang.block.unwrap_or(("", "*/"));
            match find_from(s, 0, close) {
                Some(k) => {
                    push(0, k + close.len(), Tok::Comment);
                    i = k + close.len();
                    st = ST_NORMAL;
                }
                None => {
                    push(0, s.len(), Tok::Comment);
                    return ST_BLOCK;
                }
            }
        }
        ST_TRIPLE_DQ | ST_TRIPLE_SQ => {
            let close = if st == ST_TRIPLE_DQ { "\"\"\"" } else { "'''" };
            match find_from(s, 0, close) {
                Some(k) => {
                    push(0, k + 3, Tok::Str);
                    i = k + 3;
                    st = ST_NORMAL;
                }
                None => {
                    push(0, s.len(), Tok::Str);
                    return st;
                }
            }
        }
        ST_TEMPLATE => match s.iter().position(|&c| c == b'`') {
            Some(k) => {
                push(0, k + 1, Tok::Str);
                i = k + 1;
                st = ST_NORMAL;
            }
            None => {
                push(0, s.len(), Tok::Str);
                return ST_TEMPLATE;
            }
        },
        _ => {}
    }

    let first = s.iter().position(|c| !c.is_ascii_whitespace()).unwrap_or(s.len());
    while i < s.len() {
        let c = s[i];
        if let Some((open, _)) = lang.block {
            if starts_with_at(s, i, open, false) {
                let close = lang.block.unwrap().1;
                match find_from(s, i + open.len(), close) {
                    Some(k) => {
                        push(i, k + close.len(), Tok::Comment);
                        i = k + close.len();
                        continue;
                    }
                    None => {
                        push(i, s.len(), Tok::Comment);
                        return ST_BLOCK;
                    }
                }
            }
        }
        // Word-like prefixes ("rem ") only count at the start of the line.
        let is_comment = lang.line.iter().any(|p| {
            if p.ends_with(' ') {
                i == first && (starts_with_at(s, i, p, true) || s[i..].eq_ignore_ascii_case(p.trim_end().as_bytes()))
            } else {
                starts_with_at(s, i, p, false)
            }
        });
        if is_comment {
            push(i, s.len(), Tok::Comment);
            return ST_NORMAL;
        }
        if lang.triple && (starts_with_at(s, i, "\"\"\"", false) || starts_with_at(s, i, "'''", false)) {
            let q = if c == b'"' { "\"\"\"" } else { "'''" };
            match find_from(s, i + 3, q) {
                Some(k) => {
                    push(i, k + 3, Tok::Str);
                    i = k + 3;
                    continue;
                }
                None => {
                    push(i, s.len(), Tok::Str);
                    return if c == b'"' { ST_TRIPLE_DQ } else { ST_TRIPLE_SQ };
                }
            }
        }
        if lang.quotes.contains(&c) {
            // Rust lifetimes ('a) are not character literals.
            if c == b'\'' && lang.name == "Rust" && i + 2 < s.len() && s[i + 2] != b'\'' && !(s[i + 1] == b'\\') {
                i += 1;
                continue;
            }
            let mut k = i + 1;
            while k < s.len() && s[k] != c {
                if s[k] == b'\\' && lang.name != "Batch" && lang.name != "SQL" && lang.name != "Visual Basic" {
                    k += 1;
                }
                k += 1;
            }
            if c == b'`' && k >= s.len() {
                push(i, s.len(), Tok::Str);
                return ST_TEMPLATE;
            }
            let e = (k + 1).min(s.len());
            push(i, e, Tok::Str);
            i = e;
            continue;
        }
        if let Some(p) = lang.pre.iter().find(|p| starts_with_at(s, i, p, false)) {
            // "#" and ":" (C preprocessor, batch labels) own the whole line but
            // only at its start; "[..]" runs to the bracket; others take a word.
            let e = match *p {
                "#" | ":" | "#![" if i == first => s.len(),
                "#" | ":" | "#![" => i,
                "[" | "#[" => s[i..].iter().position(|&c| c == b']').map(|k| i + k + 1).unwrap_or(s.len()),
                _ => {
                    let mut k = i + p.len();
                    while k < s.len() && (is_word(s[k]) || s[k] == b'-' || s[k] == b'.') {
                        k += 1;
                    }
                    k
                }
            };
            if e > i + p.len() {
                push(i, e, Tok::Pre);
                i = e;
                continue;
            }
        }
        if c.is_ascii_digit() && (i == 0 || !is_word(s[i - 1])) {
            let mut k = i + 1;
            while k < s.len() && (is_word(s[k]) || s[k] == b'.') {
                k += 1;
            }
            push(i, k, Tok::Num);
            i = k;
            continue;
        }
        if is_word(c) || c == b'$' || (c == b'-' && lang.nocase && i + 1 < s.len() && s[i + 1].is_ascii_alphabetic()) {
            let mut k = i + 1;
            while k < s.len() && (is_word(s[k]) || (lang.name == "CSS" && s[k] == b'-')) {
                k += 1;
            }
            let w = &s[i..k];
            if word_in(lang.keywords, w, lang.nocase) {
                push(i, k, Tok::Keyword);
            } else if word_in(lang.types, w, lang.nocase) || (c.is_ascii_uppercase() && lang.kind == Kind::Code && !lang.nocase && k - i > 1 && w.iter().any(|b| b.is_ascii_lowercase())) {
                push(i, k, Tok::Type);
            } else if c == b'$' {
                push(i, k, Tok::Attr);
            }
            i = k;
            continue;
        }
        if b"+-*/%=<>!&|^~?:".contains(&c) {
            push(i, i + 1, Tok::Op);
        }
        i += 1;
    }
    st
}

fn lex_markup(s: &[u8], state: u8, push: &mut impl FnMut(usize, usize, Tok)) -> u8 {
    let mut i = 0;
    let mut st = state;
    while i < s.len() {
        match st {
            ST_BLOCK => match find_from(s, i, "-->") {
                Some(k) => {
                    push(i, k + 3, Tok::Comment);
                    i = k + 3;
                    st = ST_NORMAL;
                }
                None => {
                    push(i, s.len(), Tok::Comment);
                    return ST_BLOCK;
                }
            },
            ST_TAG => {
                let c = s[i];
                if c == b'>' || starts_with_at(s, i, "/>", false) || starts_with_at(s, i, "?>", false) {
                    let n = if c == b'>' { 1 } else { 2 };
                    push(i, i + n, Tok::Tag);
                    i += n;
                    st = ST_NORMAL;
                } else if c == b'"' || c == b'\'' {
                    let e = s[i + 1..].iter().position(|&q| q == c).map(|k| i + k + 2).unwrap_or(s.len());
                    push(i, e, Tok::Str);
                    i = e;
                } else if is_word(c) {
                    let mut k = i + 1;
                    while k < s.len() && (is_word(s[k]) || s[k] == b'-' || s[k] == b':' || s[k] == b'.') {
                        k += 1;
                    }
                    push(i, k, Tok::Attr);
                    i = k;
                } else {
                    i += 1;
                }
            }
            _ => {
                if starts_with_at(s, i, "<!--", false) {
                    st = ST_BLOCK;
                    push(i, i + 4, Tok::Comment);
                    i += 4;
                } else if s[i] == b'<' {
                    let mut k = i + 1;
                    if k < s.len() && (s[k] == b'/' || s[k] == b'?' || s[k] == b'!') {
                        k += 1;
                    }
                    while k < s.len() && (is_word(s[k]) || s[k] == b'-' || s[k] == b':' || s[k] == b'.') {
                        k += 1;
                    }
                    push(i, k, Tok::Tag);
                    i = k;
                    st = ST_TAG;
                } else if s[i] == b'&' {
                    let e = s[i..].iter().position(|&c| c == b';').map(|k| i + k + 1).unwrap_or(i + 1);
                    push(i, e, Tok::Num);
                    i = e;
                } else {
                    i += 1;
                }
            }
        }
    }
    st
}

#[cfg(test)]
mod tests {
    use super::*;

    fn toks(lang: &str, s: &str, st: u8) -> (Vec<(u32, u32, Tok)>, u8) {
        let l = LANGS.iter().find(|l| l.name == lang).unwrap();
        let mut v = Vec::new();
        let st = lex(l, s.as_bytes(), st, Some(&mut v));
        (v, st)
    }

    #[test]
    fn rust_line() {
        let (v, st) = toks("Rust", "fn main() { let x = \"hi\"; // c", 0);
        assert_eq!(st, 0);
        assert_eq!(v[0], (0, 2, Tok::Keyword));
        assert!(v.contains(&(20, 24, Tok::Str)));
        assert_eq!(*v.last().unwrap(), (26, 30, Tok::Comment));
    }

    #[test]
    fn block_comment_spans_lines() {
        let (_, st) = toks("C / C++", "int a; /* open", 0);
        assert_eq!(st, ST_BLOCK);
        let (v, st) = toks("C / C++", "still */ int b;", st);
        assert_eq!(st, 0);
        assert_eq!(v[0], (0, 8, Tok::Comment));
    }

    #[test]
    fn python_triple_and_batch_rem() {
        let (_, st) = toks("Python", "x = \"\"\"doc", 0);
        assert_eq!(st, ST_TRIPLE_DQ);
        let (v, _) = toks("Batch", "REM hello", 0);
        assert_eq!(v, vec![(0, 9, Tok::Comment)]);
        let (v, _) = toks("Batch", "echo remark", 0);
        assert!(!v.iter().any(|t| t.2 == Tok::Comment));
    }

    #[test]
    fn markup_tag_across_lines() {
        let (_, st) = toks("XML / HTML", "<a href=\"x\"", 0);
        assert_eq!(st, ST_TAG);
        let (v, st) = toks("XML / HTML", " id='y'>text", st);
        assert_eq!(st, 0);
        assert!(v.contains(&(1, 3, Tok::Attr)));
    }

    #[test]
    fn ext_lookup() {
        assert_eq!(LANGS[lang_for_path("C:\\a\\b.PS1")].name, "PowerShell");
        assert_eq!(LANGS[lang_for_path("x.unknown")].name, "Plain Text");
    }
}
