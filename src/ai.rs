//! The AI assistant panel. It talks only to the company's own AI server,
//! set by its address (Settings: "AI: Set Company AI Server"); until one is
//! set the panel says "Coming soon" and Slate sends nothing anywhere. The
//! server is spoken to in the common chat-completions form (OpenAI style),
//! which Ollama, vLLM, LiteLLM and most in-house gateways offer.

use crate::app::{App, Input};
use crate::settings;
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::Networking::WinHttp::*;
use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_APP};

/// Posted with a `Box<Reply>` in lParam.
pub const WM_AI: u32 = WM_APP + 10;
pub const PANEL_W: f32 = 380.0;
/// Text of the file or selection sent along, at most.
const CONTEXT_MAX: usize = 24 * 1024;

pub struct Reply {
    pub req: u64,
    pub result: Result<String, String>,
}

#[derive(Default)]
pub struct AiPanel {
    pub open: bool,
    pub focus: bool,
    pub input: Input,
    /// (from the user, text)
    pub msgs: Vec<(bool, String)>,
    /// The request waiting for an answer.
    pub busy: Option<u64>,
    /// Send the selection (or the file) along with the question.
    pub context: bool,
    /// Pixels scrolled up from the newest message.
    pub scroll: f32,
}

// ---- a small JSON reader / writer: just what chat completions need ----

#[derive(Debug, PartialEq)]
pub enum Json {
    Null,
    Bool(bool),
    Num(f64),
    Str(String),
    Arr(Vec<Json>),
    Obj(Vec<(String, Json)>),
}

impl Json {
    pub fn get(&self, k: &str) -> Option<&Json> {
        match self {
            Json::Obj(v) => v.iter().find(|(a, _)| a == k).map(|(_, j)| j),
            _ => None,
        }
    }
    pub fn at(&self, i: usize) -> Option<&Json> {
        match self {
            Json::Arr(v) => v.get(i),
            _ => None,
        }
    }
    pub fn str(&self) -> Option<&str> {
        match self {
            Json::Str(s) => Some(s),
            _ => None,
        }
    }
}

pub fn parse_json(s: &str) -> Option<Json> {
    let b = s.as_bytes();
    let mut i = 0;
    let v = value(b, &mut i, 0)?;
    ws(b, &mut i);
    (i == b.len()).then_some(v)
}

fn ws(b: &[u8], i: &mut usize) {
    while *i < b.len() && matches!(b[*i], b' ' | b'\t' | b'\r' | b'\n') {
        *i += 1;
    }
}

fn value(b: &[u8], i: &mut usize, depth: usize) -> Option<Json> {
    if depth > 64 {
        return None;
    }
    ws(b, i);
    match *b.get(*i)? {
        b'{' => {
            *i += 1;
            let mut v = Vec::new();
            ws(b, i);
            if b.get(*i) == Some(&b'}') {
                *i += 1;
                return Some(Json::Obj(v));
            }
            loop {
                ws(b, i);
                let Json::Str(k) = string(b, i)? else { return None };
                ws(b, i);
                if b.get(*i) != Some(&b':') {
                    return None;
                }
                *i += 1;
                v.push((k, value(b, i, depth + 1)?));
                ws(b, i);
                match b.get(*i)? {
                    b',' => *i += 1,
                    b'}' => {
                        *i += 1;
                        return Some(Json::Obj(v));
                    }
                    _ => return None,
                }
            }
        }
        b'[' => {
            *i += 1;
            let mut v = Vec::new();
            ws(b, i);
            if b.get(*i) == Some(&b']') {
                *i += 1;
                return Some(Json::Arr(v));
            }
            loop {
                v.push(value(b, i, depth + 1)?);
                ws(b, i);
                match b.get(*i)? {
                    b',' => *i += 1,
                    b']' => {
                        *i += 1;
                        return Some(Json::Arr(v));
                    }
                    _ => return None,
                }
            }
        }
        b'"' => string(b, i),
        b't' if b[*i..].starts_with(b"true") => {
            *i += 4;
            Some(Json::Bool(true))
        }
        b'f' if b[*i..].starts_with(b"false") => {
            *i += 5;
            Some(Json::Bool(false))
        }
        b'n' if b[*i..].starts_with(b"null") => {
            *i += 4;
            Some(Json::Null)
        }
        _ => {
            let s = *i;
            while *i < b.len() && matches!(b[*i], b'-' | b'+' | b'.' | b'e' | b'E' | b'0'..=b'9') {
                *i += 1;
            }
            std::str::from_utf8(&b[s..*i]).ok()?.parse().ok().map(Json::Num)
        }
    }
}

fn string(b: &[u8], i: &mut usize) -> Option<Json> {
    if b.get(*i) != Some(&b'"') {
        return None;
    }
    *i += 1;
    let mut out: Vec<u8> = Vec::new();
    loop {
        let c = *b.get(*i)?;
        *i += 1;
        match c {
            b'"' => return String::from_utf8(out).ok().map(Json::Str),
            b'\\' => {
                let e = *b.get(*i)?;
                *i += 1;
                match e {
                    b'n' => out.push(b'\n'),
                    b't' => out.push(b'\t'),
                    b'r' => out.push(b'\r'),
                    b'b' => out.push(8),
                    b'f' => out.push(12),
                    b'u' => {
                        let hex = |i: &mut usize| -> Option<u32> {
                            let h = std::str::from_utf8(b.get(*i..*i + 4)?).ok()?;
                            *i += 4;
                            u32::from_str_radix(h, 16).ok()
                        };
                        let mut u = hex(i)?;
                        // A surrogate pair is two escapes.
                        if (0xD800..0xDC00).contains(&u) && b.get(*i) == Some(&b'\\') && b.get(*i + 1) == Some(&b'u') {
                            *i += 2;
                            let lo = hex(i)?;
                            u = 0x10000 + ((u - 0xD800) << 10) + (lo.wrapping_sub(0xDC00) & 0x3FF);
                        }
                        let ch = char::from_u32(u).unwrap_or('\u{FFFD}');
                        let mut tmp = [0u8; 4];
                        out.extend_from_slice(ch.encode_utf8(&mut tmp).as_bytes());
                    }
                    other => out.push(other),
                }
            }
            c => out.push(c),
        }
    }
}

pub fn json_str(s: &str) -> String {
    let mut o = String::with_capacity(s.len() + 2);
    o.push('"');
    for c in s.chars() {
        match c {
            '"' => o.push_str("\\\""),
            '\\' => o.push_str("\\\\"),
            '\n' => o.push_str("\\n"),
            '\r' => o.push_str("\\r"),
            '\t' => o.push_str("\\t"),
            c if (c as u32) < 0x20 => o.push_str(&format!("\\u{:04x}", c as u32)),
            c => o.push(c),
        }
    }
    o.push('"');
    o
}

/// The chat-completions request body.
pub fn request_body(model: &str, msgs: &[(&str, String)]) -> String {
    let list: Vec<String> = msgs.iter().map(|(role, text)| format!("{{\"role\":{},\"content\":{}}}", json_str(role), json_str(text))).collect();
    format!("{{\"model\":{},\"stream\":false,\"messages\":[{}]}}", json_str(model), list.join(","))
}

/// The answer text, or the server's error message.
pub fn reply_text(body: &str) -> Result<String, String> {
    let j = parse_json(body).ok_or("the AI server sent an answer Slate cannot read")?;
    if let Some(m) = j.get("choices").and_then(|c| c.at(0)).and_then(|c| c.get("message")).and_then(|m| m.get("content")).and_then(|c| c.str()) {
        return Ok(m.trim().to_string());
    }
    let e = j.get("error").map(|e| e.get("message").and_then(|m| m.str()).or(e.str()).unwrap_or("unknown error").to_string());
    Err(e.unwrap_or_else(|| "the AI server sent no answer".into()))
}

/// Where requests go: the address as given if it names the endpoint,
/// otherwise `<address>/v1/chat/completions`.
pub fn endpoint(base: &str) -> String {
    let b = base.trim().trim_end_matches('/');
    if b.ends_with("/chat/completions") {
        b.to_string()
    } else if b.ends_with("/v1") {
        format!("{b}/chat/completions")
    } else {
        format!("{b}/v1/chat/completions")
    }
}

struct H(*mut std::ffi::c_void);
impl Drop for H {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe {
                let _ = WinHttpCloseHandle(self.0);
            }
        }
    }
}

/// POST a JSON body to http:// or https:// and return the response body.
pub fn post_json(url: &str, body: &str, key: &str) -> Result<String, String> {
    let (secure, rest) = match url.split_once("://") {
        Some(("https", r)) => (true, r),
        Some(("http", r)) => (false, r),
        _ => return Err("the AI server address must start with http:// or https://".into()),
    };
    let (hostport, path) = rest.split_once('/').map(|(h, p)| (h, format!("/{p}"))).unwrap_or((rest, "/".into()));
    let (host, port) = match hostport.rsplit_once(':') {
        Some((h, p)) if p.parse::<u16>().is_ok() => (h, p.parse::<u16>().unwrap()),
        _ => (hostport, if secure { 443 } else { 80 }),
    };
    let wh = crate::sys::wide(host);
    let wp = crate::sys::wide(&path);
    let agent = crate::sys::wide(&format!("Slate/{}", env!("CARGO_PKG_VERSION")));
    let mut headers = String::from("Content-Type: application/json\r\n");
    if !key.trim().is_empty() {
        headers += &format!("Authorization: Bearer {}\r\n", key.trim());
    }
    let wheaders: Vec<u16> = headers.encode_utf16().collect();
    unsafe {
        let s = H(WinHttpOpen(PCWSTR(agent.as_ptr()), WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY, PCWSTR::null(), PCWSTR::null(), 0));
        if s.0.is_null() {
            return Err("the network is not available".into());
        }
        // Answers can take a while to compose.
        let _ = WinHttpSetTimeouts(s.0, 10_000, 10_000, 30_000, 180_000);
        let c = H(WinHttpConnect(s.0, PCWSTR(wh.as_ptr()), port, 0));
        if c.0.is_null() {
            return Err("cannot reach the AI server".into());
        }
        let flags = if secure { WINHTTP_FLAG_SECURE } else { WINHTTP_OPEN_REQUEST_FLAGS(0) };
        let r = H(WinHttpOpenRequest(c.0, w!("POST"), PCWSTR(wp.as_ptr()), PCWSTR::null(), PCWSTR::null(), std::ptr::null(), flags));
        if r.0.is_null() {
            return Err("the request could not be made".into());
        }
        let data = body.as_bytes();
        WinHttpSendRequest(r.0, Some(&wheaders), Some(data.as_ptr() as *const _), data.len() as u32, data.len() as u32, 0)
            .map_err(|e| format!("cannot reach the AI server ({})", e.code().0 & 0xFFFF))?;
        WinHttpReceiveResponse(r.0, std::ptr::null_mut()).map_err(|_| "no answer from the AI server".to_string())?;
        let mut status = 0u32;
        let mut len = 4u32;
        let _ = WinHttpQueryHeaders(
            r.0,
            WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER,
            PCWSTR::null(),
            Some(&mut status as *mut u32 as *mut _),
            &mut len,
            std::ptr::null_mut(),
        );
        let mut out = Vec::new();
        let mut buf = vec![0u8; 64 * 1024];
        loop {
            let mut got = 0u32;
            if WinHttpReadData(r.0, buf.as_mut_ptr() as *mut _, buf.len() as u32, &mut got).is_err() || got == 0 {
                break;
            }
            out.extend_from_slice(&buf[..got as usize]);
            if out.len() > 8 << 20 {
                return Err("the answer is too long".into());
            }
        }
        let text = String::from_utf8_lossy(&out).into_owned();
        if status != 200 {
            // Servers usually explain themselves in an error object.
            return Err(match reply_text(&text) {
                Err(e) if e != "the AI server sent an answer Slate cannot read" => format!("the AI server answered {status}: {e}"),
                _ => format!("the AI server answered {status}"),
            });
        }
        Ok(text)
    }
}

const SYSTEM: &str = "You are the assistant built into Slate, a text editor used for piping design, drafting and QA work (PCF and IsoGen files, PDS reports, line lists and registers as CSV, Navisworks XML, logs). Answer briefly and plainly. Never invent dimensions, tags or specifications; say when something cannot be told from the text. When asked to rewrite or fix text, reply with only the new text, no explanation.";

impl App {
    pub fn ai_configured() -> bool {
        !settings::get().ai_url.trim().is_empty()
    }

    pub fn ai_toggle(&mut self) {
        self.ai.open = !self.ai.open;
        self.ai.focus = self.ai.open && Self::ai_configured();
        self.cache.clear();
        self.redraw();
    }

    /// Asks the server. `action` adds an instruction and sends the
    /// selection or file with it.
    pub fn ai_send(&mut self, action: Option<&str>) {
        if !Self::ai_configured() {
            return self.flash("The AI assistant is coming soon: it needs your company's AI server (AI: Set Company AI Server)");
        }
        if self.ai.busy.is_some() {
            return self.flash("Still waiting for the last answer");
        }
        let typed = self.ai.input.text.trim().to_string();
        let ask = match action {
            Some(a) => {
                if typed.is_empty() {
                    a.to_string()
                } else {
                    format!("{a}\n{typed}")
                }
            }
            None if typed.is_empty() => return,
            None => typed,
        };
        // What it is about: the selection, else the file (cut to a size).
        let mut content = ask.clone();
        if action.is_some() || self.ai.context {
            let b = self.cur_ref();
            let sel: Vec<u8> = b.sels.iter().filter(|s| !s.is_empty()).flat_map(|s| b.doc.text(s.lo(), s.hi())).collect();
            let (what, mut text) = if sel.is_empty() { (format!("The file {}", b.name()), b.doc.text(0, b.doc.len().min(CONTEXT_MAX))) } else { ("The selected text".to_string(), sel) };
            let cut = text.len() > CONTEXT_MAX;
            text.truncate(CONTEXT_MAX);
            let t = String::from_utf8_lossy(&text).into_owned();
            content = format!("{ask}\n\n{what}{}:\n```\n{t}\n```", if cut { " (beginning only)" } else { "" });
        }
        self.ai.msgs.push((true, ask));
        self.ai.input = Input::default();
        self.ai.scroll = 0.0;
        // The conversation so far (last few turns), then the new question.
        let mut msgs: Vec<(&str, String)> = vec![("system", SYSTEM.to_string())];
        let n = self.ai.msgs.len();
        for (user, text) in self.ai.msgs[n.saturating_sub(11)..n - 1].iter() {
            msgs.push((if *user { "user" } else { "assistant" }, text.clone()));
        }
        msgs.push(("user", content));
        let (url, model, key) = {
            let s = settings::get();
            (endpoint(&s.ai_url), if s.ai_model.trim().is_empty() { "default".to_string() } else { s.ai_model.trim().to_string() }, s.ai_key.clone())
        };
        let body = request_body(&model, &msgs);
        let req = crate::net::new_id();
        self.ai.busy = Some(req);
        let hwnd = self.hwnd.0 as isize;
        std::thread::spawn(move || {
            let result = post_json(&url, &body, &key).and_then(|t| reply_text(&t));
            let p = Box::into_raw(Box::new(Reply { req, result }));
            unsafe {
                if PostMessageW(HWND(hwnd as *mut _), WM_AI, WPARAM(0), LPARAM(p as isize)).is_err() {
                    drop(Box::from_raw(p));
                }
            }
        });
        self.redraw();
    }

    pub fn on_ai(&mut self, r: Reply) {
        if self.ai.busy != Some(r.req) {
            return;
        }
        self.ai.busy = None;
        match r.result {
            Ok(t) => self.ai.msgs.push((false, t)),
            Err(e) => self.ai.msgs.push((false, format!("\u{26A0} {e}"))),
        }
        self.ai.scroll = 0.0;
        self.redraw();
    }

    /// The newest answer, if it is not an error.
    fn ai_last(&self) -> Option<String> {
        self.ai.msgs.iter().rev().find(|m| !m.0).map(|m| m.1.clone()).filter(|t| !t.starts_with('\u{26A0}'))
    }

    /// Puts the newest answer into the editor, replacing the selection.
    pub fn ai_insert(&mut self) {
        let Some(t) = self.ai_last() else { return self.flash("No answer to insert yet") };
        if self.cur_ref().read_only() {
            let m = self.read_only_reason();
            return self.flash(&m);
        }
        // Answers often come wrapped in a code fence: insert what is inside.
        let t = strip_fence(&t);
        let mut text: Vec<u8> = t.replace("\r\n", "\n").into_bytes();
        let b = self.cur();
        // Whole lines replaced keep their line break.
        let p = b.primary();
        if !p.is_empty() && b.doc.byte(p.hi() - 1) == b'\n' && text.last() != Some(&b'\n') {
            text.push(b'\n');
        }
        crate::edit::insert(b, &text, crate::buffer::EditKind::Other);
        self.edited();
        self.reveal_caret();
        self.redraw();
    }

    pub fn ai_copy(&mut self) {
        let Some(t) = self.ai_last() else { return };
        crate::sys::set_clipboard(self.hwnd, strip_fence(&t).as_bytes(), false);
        self.flash("Answer copied");
    }

    pub fn ai_clear(&mut self) {
        self.ai.msgs.clear();
        self.ai.busy = None;
        self.ai.scroll = 0.0;
        self.redraw();
    }
}

/// The text inside a single ``` fence, or the text as it is.
pub fn strip_fence(t: &str) -> String {
    let s = t.trim();
    if let Some(rest) = s.strip_prefix("```") {
        if let Some(body) = rest.strip_suffix("```") {
            // Drop a language tag on the first line.
            return match body.split_once('\n') {
                Some((first, more)) if !first.contains(' ') => more.trim_end_matches('\n').to_string(),
                _ => body.trim().to_string(),
            };
        }
    }
    s.to_string()
}


// ---- the panel on screen ----

/// Parts of the panel a click can land on.
pub const P_INPUT: u8 = 1;
pub const P_SEND: u8 = 2;
pub const P_INSERT: u8 = 3;
pub const P_COPY: u8 = 4;
pub const P_EXPLAIN: u8 = 5;
pub const P_SUMMARY: u8 = 6;
pub const P_REWRITE: u8 = 7;
pub const P_CONTEXT: u8 = 8;
pub const P_CLOSE: u8 = 9;
pub const P_SETUP: u8 = 10;
pub const P_CLEAR: u8 = 11;
pub const P_PANEL: u8 = 0;

impl App {
    /// The panel's rectangle, when open.
    pub fn ai_rect(&self) -> Option<crate::ui::Rect> {
        (self.ai.open && self.w > PANEL_W + 300.0).then(|| crate::ui::r(self.w - PANEL_W, self.top(), PANEL_W, (self.h - self.top() - crate::app::STATUS_H).max(0.0)))
    }

    /// The clickable parts of the panel.
    pub fn ai_layout(&self) -> Vec<(u8, crate::ui::Rect)> {
        use crate::ui::r;
        let Some(p) = self.ai_rect() else { return Vec::new() };
        let mut out = vec![(P_CLOSE, r(p.x + p.w - 38.0, p.y + 6.0, 30.0, 28.0))];
        if !Self::ai_configured() {
            out.push((P_SETUP, r(p.x + 40.0, p.y + p.h * 0.42 + 120.0, p.w - 80.0, 32.0)));
            return out;
        }
        out.push((P_CLEAR, r(p.x + p.w - 74.0, p.y + 6.0, 34.0, 28.0)));
        // Quick actions under the header.
        let chips = [(P_EXPLAIN, "Explain"), (P_SUMMARY, "Summarise"), (P_REWRITE, "Fix / rewrite")];
        let mut x = p.x + 12.0;
        for (id, label) in chips {
            let w = self.text_w(label, true) + 22.0;
            out.push((id, r(x, p.y + 44.0, w, 26.0)));
            x += w + 6.0;
        }
        let bottom = p.y + p.h;
        out.push((P_INPUT, r(p.x + 12.0, bottom - 48.0, p.w - 24.0, 36.0)));
        out.push((P_SEND, r(p.x + p.w - 50.0, bottom - 44.0, 34.0, 28.0)));
        out.push((P_CONTEXT, r(p.x + 12.0, bottom - 78.0, 200.0, 24.0)));
        out.push((P_INSERT, r(p.x + p.w - 150.0, bottom - 78.0, 74.0, 24.0)));
        out.push((P_COPY, r(p.x + p.w - 70.0, bottom - 78.0, 58.0, 24.0)));
        out
    }

    pub fn ai_hit(&self, x: f32, y: f32) -> Option<u8> {
        let p = self.ai_rect()?;
        if !p.has(x, y) {
            return None;
        }
        Some(self.ai_layout().into_iter().find(|(_, rc)| rc.has(x, y)).map(|(id, _)| id).unwrap_or(P_PANEL))
    }

    pub fn ai_click(&mut self, part: u8) {
        self.ai.focus = Self::ai_configured() && matches!(part, P_INPUT | P_SEND | P_PANEL | P_CONTEXT);
        match part {
            P_CLOSE => self.ai_toggle(),
            P_SETUP => self.exec(crate::app::Cmd::AiServer),
            P_SEND => self.ai_send(None),
            P_EXPLAIN => self.ai_send(Some("Explain this.")),
            P_SUMMARY => self.ai_send(Some("Summarise this in a few lines.")),
            P_REWRITE => self.ai_send(Some("Fix any mistakes in this and return the corrected text only.")),
            P_INSERT => self.ai_insert(),
            P_COPY => self.ai_copy(),
            P_CLEAR => self.ai_clear(),
            P_CONTEXT => self.ai.context = !self.ai.context,
            _ => {}
        }
        self.redraw();
    }

    /// Keys while the panel's input has the focus; true when used.
    pub fn ai_key(&mut self, vk: u32, ctrl: bool, shift: bool) -> bool {
        if !self.ai.focus || !self.ai.open {
            return false;
        }
        match vk {
            0x0D => self.ai_send(None),
            0x1B => self.ai.focus = false,
            _ => {
                let hwnd = self.hwnd;
                if !self.ai.input.key(vk, ctrl, shift, hwnd) {
                    // Shortcuts (Ctrl+S, Ctrl+Tab...) still reach the editor.
                    return ctrl && matches!(vk, 0x41 | 0x43 | 0x56 | 0x58 | 0x5A);
                }
            }
        }
        self.redraw();
        true
    }

    pub fn ai_char(&mut self, ch: char) -> bool {
        if !self.ai.focus || !self.ai.open {
            return false;
        }
        self.ai.input.insert(&ch.to_string());
        self.redraw();
        true
    }

    pub fn ai_wheel(&mut self, x: f32, y: f32, delta: i32) -> bool {
        if self.ai_hit(x, y).is_none() {
            return false;
        }
        self.ai.scroll = (self.ai.scroll + delta as f32 / 120.0 * 48.0).max(0.0);
        self.redraw();
        true
    }

    pub fn paint_ai(&self) {
        use crate::gfx::C;
        use crate::icons::Icon;
        let Some(p) = self.ai_rect() else { return };
        let g = &self.gfx;
        g.fill(p.x, p.y, p.w, p.h, C::Chrome);
        g.fill(p.x, p.y, 1.0, p.h, C::Border);
        let ui = g.ui.as_ref().unwrap();
        let small = g.ui_small.as_ref().unwrap();
        let bold = g.ui_bold.as_ref().unwrap();
        // Header.
        self.draw_logo_alpha(p.x + 14.0, p.y + 10.0, 20.0, 1.0);
        g.text("AI Assistant", bold, p.x + 42.0, p.y + 6.0, 200.0, 28.0, C::Fg);
        let layout = self.ai_layout();
        let find = |id: u8| layout.iter().find(|(k, _)| *k == id).map(|(_, rc)| rc);
        let hot = |id: u8| self.hover == crate::app::Hit::Ai(id);
        if let Some(rc) = find(P_CLOSE) {
            if hot(P_CLOSE) {
                g.fill_round(rc.x, rc.y, rc.w, rc.h, 5.0, C::Hover);
            }
            g.draw_icon(Icon::Close, rc.x, rc.y, rc.w, rc.h, C::Dim, 10.0, 1.3);
        }
        g.fill(p.x, p.y + 40.0, p.w, 1.0, C::Border);
        if !Self::ai_configured() {
            // Coming soon: what it will be, and that nothing is sent meanwhile.
            let cy = p.y + p.h * 0.42;
            self.draw_logo_alpha(p.x + (p.w - 64.0) / 2.0, cy - 96.0, 64.0, 0.9);
            unsafe {
                let _ = bold.SetTextAlignment(windows::Win32::Graphics::DirectWrite::DWRITE_TEXT_ALIGNMENT_CENTER);
                let _ = small.SetTextAlignment(windows::Win32::Graphics::DirectWrite::DWRITE_TEXT_ALIGNMENT_CENTER);
            }
            g.text("Coming soon", bold, p.x, cy - 20.0, p.w, 24.0, C::Fg);
            g.text_wrapped(
                "The assistant will answer questions about your files, explain or summarise a selection and fix text \u{2013} using your company's own AI server. Until that server is set up, nothing is sent anywhere.",
                small,
                p.x + 32.0,
                cy + 10.0,
                p.w - 64.0,
                C::Dim,
            );
            unsafe {
                let _ = bold.SetTextAlignment(Default::default());
                let _ = small.SetTextAlignment(Default::default());
            }
            if let Some(rc) = find(P_SETUP) {
                g.fill_round(rc.x, rc.y, rc.w, rc.h, 6.0, if hot(P_SETUP) { C::Hover } else { C::Bg });
                g.stroke_round(rc.x, rc.y, rc.w, rc.h, 6.0, C::Border, 1.0);
                let label = "Set up the company AI server\u{2026}";
                let w = g.measure(label, small);
                g.text(label, small, rc.x + (rc.w - w) / 2.0, rc.y, rc.w, rc.h, C::Fg);
            }
            return;
        }
        if let Some(rc) = find(P_CLEAR) {
            if hot(P_CLEAR) {
                g.fill_round(rc.x, rc.y, rc.w, rc.h, 5.0, C::Hover);
            }
            g.text("Clear", small, rc.x + 2.0, rc.y, rc.w, rc.h, C::Dim);
        }
        for (id, label) in [(P_EXPLAIN, "Explain"), (P_SUMMARY, "Summarise"), (P_REWRITE, "Fix / rewrite")] {
            if let Some(rc) = find(id) {
                g.fill_round(rc.x, rc.y, rc.w, rc.h, 13.0, if hot(id) { C::Hover } else { C::Bg });
                g.stroke_round(rc.x, rc.y, rc.w, rc.h, 13.0, C::Border, 1.0);
                g.text(label, small, rc.x + 11.0, rc.y, rc.w, rc.h, C::Fg);
            }
        }
        // Messages, newest at the bottom.
        let top = p.y + 78.0;
        let bottom = p.y + p.h - 86.0;
        g.clip(p.x, top, p.w, (bottom - top).max(0.0));
        let maxw = p.w - 60.0;
        let mut y = bottom + self.ai.scroll;
        let mut items: Vec<(bool, String)> = self.ai.msgs.clone();
        if self.ai.busy.is_some() {
            items.push((false, "Thinking\u{2026}".into()));
        }
        if items.is_empty() {
            g.text_wrapped("Ask about the file or the selection, or pick a quick action above. Tick \u{201C}Include selection / file\u{201D} to send it along with your question.", small, p.x + 24.0, top + 16.0, p.w - 48.0, C::Dim);
        }
        for (user, text) in items.iter().rev() {
            let (w, h) = g.wrapped_size(text, ui, maxw);
            let bh = h + 16.0;
            y -= bh + 10.0;
            if y + bh < top {
                break;
            }
            let bw = (w + 24.0).min(p.w - 36.0);
            let bx = if *user { p.x + p.w - 14.0 - bw } else { p.x + 14.0 };
            g.fill_round(bx, y, bw, bh, 10.0, if *user { C::SelDim } else { C::Bg });
            if !*user {
                g.stroke_round(bx, y, bw, bh, 10.0, C::Border, 1.0);
            }
            g.text_wrapped(text, ui, bx + 12.0, y + 8.0, maxw, if text.starts_with('\u{26A0}') { C::Danger } else { C::Fg });
        }
        g.unclip();
        // Context toggle, answer actions, input.
        if let Some(rc) = find(P_CONTEXT) {
            let on = self.ai.context;
            g.stroke_round(rc.x, rc.y + 5.0, 14.0, 14.0, 3.0, if on { C::Accent } else { C::Dim }, 1.2);
            if on {
                g.fill_round(rc.x + 3.0, rc.y + 8.0, 8.0, 8.0, 2.0, C::Accent);
            }
            g.text("Include selection / file", small, rc.x + 22.0, rc.y, rc.w, rc.h, C::Dim);
        }
        let has_answer = self.ai.msgs.iter().any(|m| !m.0 && !m.1.starts_with('\u{26A0}'));
        for (id, label) in [(P_INSERT, "Insert"), (P_COPY, "Copy")] {
            if let Some(rc) = find(id) {
                if hot(id) && has_answer {
                    g.fill_round(rc.x, rc.y, rc.w, rc.h, 5.0, C::Hover);
                }
                g.text(label, small, rc.x + 8.0, rc.y, rc.w, rc.h, if has_answer { C::Fg } else { C::Faint });
            }
        }
        if let Some(rc) = find(P_INPUT) {
            self.draw_input(&self.ai.input, rc, self.ai.focus, if self.ai.busy.is_some() { "Waiting for the answer\u{2026}" } else { "Ask the assistant\u{2026}" }, 40.0);
        }
        if let Some(rc) = find(P_SEND) {
            g.fill_round(rc.x, rc.y, rc.w, rc.h, 6.0, if self.ai.input.text.trim().is_empty() { C::SelDim } else { C::Accent });
            g.draw_icon(Icon::ChevronRight, rc.x, rc.y, rc.w, rc.h, C::OnAccent, 12.0, 1.6);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_round_trip_and_replies() {
        let body = request_body("llama3", &[("system", "be brief".into()), ("user", "6\" PIPE\n\u{e9}\t\"x\"".into())]);
        let j = parse_json(&body).unwrap();
        assert_eq!(j.get("messages").and_then(|m| m.at(1)).and_then(|m| m.get("content")).and_then(|c| c.str()), Some("6\" PIPE\n\u{e9}\t\"x\""));
        let ok = r#"{"id":"x","choices":[{"index":0,"message":{"role":"assistant","content":" Hello 😀\n"},"finish_reason":"stop"}],"usage":{"total_tokens":12}}"#;
        assert_eq!(reply_text(ok), Ok("Hello \u{1F600}".into()));
        assert_eq!(reply_text(r#"{"error":{"message":"model not found"}}"#), Err("model not found".into()));
        assert!(reply_text("<html>").is_err());
        assert_eq!(endpoint("http://ai.office:11434"), "http://ai.office:11434/v1/chat/completions");
        assert_eq!(endpoint("http://ai/v1/"), "http://ai/v1/chat/completions");
        assert_eq!(endpoint("https://gw/api/chat/completions"), "https://gw/api/chat/completions");
        assert_eq!(strip_fence("```text\nPIPE\nELBOW\n```"), "PIPE\nELBOW");
        assert_eq!(strip_fence("plain"), "plain");
    }
}
