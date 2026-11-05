//! Direct2D / DirectWrite resources: render target, brushes per theme colour,
//! text formats and font metrics.

use crate::icons::{self, Icon};
use crate::sys::wide;
use std::cell::RefCell;
use std::collections::HashMap;
use windows::core::{Interface, Result, PCWSTR};
use windows::Foundation::Numerics::Matrix3x2;
use windows::Win32::Foundation::{BOOL, HWND};
use windows::Win32::Graphics::Direct2D::Common::{D2D1_COLOR_F, D2D_POINT_2F, D2D_RECT_F, D2D_SIZE_U};
use windows::Win32::Graphics::Direct2D::{
    D2D1CreateFactory, ID2D1Factory, ID2D1HwndRenderTarget, ID2D1PathGeometry, ID2D1RenderTarget, ID2D1SolidColorBrush,
    ID2D1StrokeStyle, D2D1_ANTIALIAS_MODE_ALIASED, D2D1_CAP_STYLE_ROUND, D2D1_DASH_STYLE_SOLID,
    D2D1_DRAW_TEXT_OPTIONS_CLIP, D2D1_FACTORY_TYPE_SINGLE_THREADED, D2D1_HWND_RENDER_TARGET_PROPERTIES,
    D2D1_LINE_JOIN_ROUND, D2D1_PRESENT_OPTIONS_NONE, D2D1_RENDER_TARGET_PROPERTIES, D2D1_ROUNDED_RECT,
    D2D1_STROKE_STYLE_PROPERTIES, D2D1_TEXT_ANTIALIAS_MODE_CLEARTYPE,
};
use windows::Win32::Graphics::DirectWrite::*;

#[derive(Clone, Copy, PartialEq, Eq)]
#[repr(usize)]
pub enum C {
    Bg,
    Chrome,
    Tabs,
    Fg,
    Dim,
    Faint,
    Accent,
    Sel,
    SelDim,
    CurLine,
    Border,
    Hover,
    Popup,
    Match,
    Danger,
    Kw,
    Ty,
    Str,
    Num,
    Com,
    Pre,
    Tag,
    Attr,
    Op,
    Head,
    OnAccent,
}
const NC: usize = C::OnAccent as usize + 1;

fn hex(v: u32) -> D2D1_COLOR_F {
    D2D1_COLOR_F {
        r: ((v >> 16) & 255) as f32 / 255.0,
        g: ((v >> 8) & 255) as f32 / 255.0,
        b: (v & 255) as f32 / 255.0,
        a: 1.0,
    }
}

const DARK: [u32; NC] = [
    0x15171C, 0x0E1014, 0x111318, 0xD8DCE3, 0x7B8494, 0x3A404C, 0x7AA2F7, 0x2B3A57, 0x232B3A, 0x1B1E25,
    0x262A33, 0x1F232B, 0x1C1F26, 0x3D3522, 0xF0717A, 0xC792EA, 0x7FDBCA, 0xC3E88D, 0xF78C6C, 0x5F6A7D,
    0xFFCB6B, 0xF07178, 0xFFCB6B, 0x89DDFF, 0x82AAFF, 0x0E1014,
];
const LIGHT: [u32; NC] = [
    0xFBFBFC, 0xEDEFF3, 0xF3F4F7, 0x24292F, 0x6E7781, 0xC8CDD4, 0x2F6FEB, 0xCCDDFB, 0xE3EAF5, 0xF1F4F8,
    0xD8DCE2, 0xE4E7EC, 0xFFFFFF, 0xFFE9A8, 0xCF222E, 0x8250DF, 0x0B6E99, 0x0A7F3F, 0xB35900, 0x8C959F,
    0x9A6700, 0xCF222E, 0x953800, 0x0550AE, 0x0550AE, 0xFFFFFF,
];

/// Accent choices: (name, dark-theme colour, light-theme colour).
pub const ACCENTS: [(&str, u32, u32); 7] = [
    ("Slate blue", 0x7AA2F7, 0x2F6FEB),
    ("Teal", 0x5CCFE6, 0x0B7E8A),
    ("Violet", 0xB392F0, 0x7C3AED),
    ("Amber", 0xF5B062, 0xC2610C),
    ("Green", 0x7FD88F, 0x1F883D),
    ("Rose", 0xF38BA8, 0xD6336C),
    ("Graphite", 0xAEB8C8, 0x57606A),
];

fn blend(a: u32, b: u32, t: f32) -> u32 {
    let ch = |s: u32| -> u32 {
        let (x, y) = (((a >> s) & 255) as f32, ((b >> s) & 255) as f32);
        ((x * t + y * (1.0 - t)).round() as u32) << s
    };
    ch(16) | ch(8) | ch(0)
}

pub struct Gfx {
    pub d2d: ID2D1Factory,
    pub dw: IDWriteFactory,
    pub hrt: Option<ID2D1HwndRenderTarget>,
    pub rt: Option<ID2D1RenderTarget>,
    pub brushes: Vec<ID2D1SolidColorBrush>,
    /// Editor text, no wrapping.
    pub mono: Option<IDWriteTextFormat>,
    /// Editor text, wrapped to the view width.
    pub mono_wrap: Option<IDWriteTextFormat>,
    /// Gutter numbers, right aligned.
    pub mono_num: Option<IDWriteTextFormat>,
    pub ui: Option<IDWriteTextFormat>,
    pub ui_bold: Option<IDWriteTextFormat>,
    pub ui_small: Option<IDWriteTextFormat>,
    pub char_w: f32,
    pub line_h: f32,
    pub font_px: f32,
    pub dark: bool,
    pub accent: usize,
    stroke: Option<ID2D1StrokeStyle>,
    geos: RefCell<HashMap<Icon, ID2D1PathGeometry>>,
    /// Brushes for one-off colours (file badges), keyed by rgb and alpha.
    tints: RefCell<HashMap<(u32, u8), ID2D1SolidColorBrush>>,
}

impl Gfx {
    pub fn new() -> Result<Self> {
        unsafe {
            Ok(Gfx {
                d2d: D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)?,
                dw: DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)?,
                hrt: None,
                rt: None,
                brushes: Vec::new(),
                mono: None,
                mono_wrap: None,
                mono_num: None,
                ui: None,
                ui_bold: None,
                ui_small: None,
                char_w: 8.0,
                line_h: 18.0,
                font_px: 14.0,
                dark: true,
                accent: 0,
                stroke: None,
                geos: RefCell::new(HashMap::new()),
                tints: RefCell::new(HashMap::new()),
            })
        }
    }

    pub fn create_target(&mut self, hwnd: HWND, w: u32, h: u32, dpi: f32) -> Result<()> {
        unsafe {
            let props = D2D1_RENDER_TARGET_PROPERTIES::default();
            let hp = D2D1_HWND_RENDER_TARGET_PROPERTIES {
                hwnd,
                pixelSize: D2D_SIZE_U { width: w.max(1), height: h.max(1) },
                presentOptions: D2D1_PRESENT_OPTIONS_NONE,
            };
            let hrt = self.d2d.CreateHwndRenderTarget(&props, &hp)?;
            let rt: ID2D1RenderTarget = hrt.cast()?;
            rt.SetDpi(dpi, dpi);
            rt.SetTextAntialiasMode(D2D1_TEXT_ANTIALIAS_MODE_CLEARTYPE);
            self.hrt = Some(hrt);
            self.rt = Some(rt);
        }
        self.make_brushes()
    }

    pub fn drop_target(&mut self) {
        self.brushes.clear();
        self.tints.borrow_mut().clear();
        self.rt = None;
        self.hrt = None;
    }

    /// The theme palette with the chosen accent and the selection colours
    /// derived from it.
    fn palette(&self) -> [u32; NC] {
        let mut pal = if self.dark { DARK } else { LIGHT };
        let (_, d, l) = ACCENTS[self.accent.min(ACCENTS.len() - 1)];
        let acc = if self.dark { d } else { l };
        let bg = pal[C::Bg as usize];
        pal[C::Accent as usize] = acc;
        pal[C::Sel as usize] = blend(acc, bg, if self.dark { 0.30 } else { 0.24 });
        pal[C::SelDim as usize] = blend(acc, bg, if self.dark { 0.16 } else { 0.12 });
        pal
    }

    pub fn make_brushes(&mut self) -> Result<()> {
        let Some(rt) = &self.rt else { return Ok(()) };
        let pal = self.palette();
        self.brushes.clear();
        self.tints.borrow_mut().clear();
        for &c in pal.iter() {
            self.brushes.push(unsafe { rt.CreateSolidColorBrush(&hex(c), None)? });
        }
        if self.stroke.is_none() {
            let props = D2D1_STROKE_STYLE_PROPERTIES {
                startCap: D2D1_CAP_STYLE_ROUND,
                endCap: D2D1_CAP_STYLE_ROUND,
                dashCap: D2D1_CAP_STYLE_ROUND,
                lineJoin: D2D1_LINE_JOIN_ROUND,
                miterLimit: 1.0,
                dashStyle: D2D1_DASH_STYLE_SOLID,
                dashOffset: 0.0,
            };
            self.stroke = unsafe { self.d2d.CreateStrokeStyle(&props, None).ok() };
        }
        Ok(())
    }

    pub fn b(&self, c: C) -> &ID2D1SolidColorBrush {
        &self.brushes[c as usize]
    }

    pub fn color(&self, c: C) -> D2D1_COLOR_F {
        hex(self.palette()[c as usize])
    }

    /// A brush for an arbitrary colour (cached).
    pub fn tint(&self, rgb: u32, alpha: f32) -> ID2D1SolidColorBrush {
        let key = (rgb, (alpha * 255.0) as u8);
        if let Some(b) = self.tints.borrow().get(&key) {
            return b.clone();
        }
        let mut c = hex(rgb);
        c.a = alpha;
        let b = unsafe { self.rt().CreateSolidColorBrush(&c, None).unwrap() };
        self.tints.borrow_mut().insert(key, b.clone());
        b
    }

    /// Fills with a theme colour at `alpha` (hover fades).
    pub fn fill_alpha(&self, x: f32, y: f32, w: f32, h: f32, r: f32, c: C, alpha: f32) {
        if alpha <= 0.01 {
            return;
        }
        let b = self.b(c);
        unsafe {
            b.SetOpacity(alpha.min(1.0));
            let rr = D2D1_ROUNDED_RECT { rect: rect(x, y, w, h), radiusX: r, radiusY: r };
            self.rt().FillRoundedRectangle(&rr, b);
            b.SetOpacity(1.0);
        }
    }

    /// Draws `icon` centred in the box, `size` DIPs across, `width` DIPs stroke.
    pub fn draw_icon(&self, icon: Icon, x: f32, y: f32, w: f32, h: f32, c: C, size: f32, width: f32) {
        self.draw_icon_brush(icon, x, y, w, h, self.b(c), size, width);
    }

    pub fn draw_icon_brush(&self, icon: Icon, x: f32, y: f32, w: f32, h: f32, brush: &ID2D1SolidColorBrush, size: f32, width: f32) {
        let geo = {
            let mut g = self.geos.borrow_mut();
            if !g.contains_key(&icon) {
                let Ok(geo) = icons::build(&self.d2d, icon) else { return };
                g.insert(icon, geo);
            }
            g[&icon].clone()
        };
        let k = size / 16.0;
        let ox = x + (w - size) / 2.0;
        let oy = y + (h - size) / 2.0;
        unsafe {
            let rt = self.rt();
            let mut old = Matrix3x2::default();
            rt.GetTransform(&mut old);
            rt.SetTransform(&Matrix3x2 { M11: k, M12: 0.0, M21: 0.0, M22: k, M31: old.M31 + ox, M32: old.M32 + oy });
            rt.DrawGeometry(&geo, brush, width / k, self.stroke.as_ref());
            rt.SetTransform(&old);
        }
    }

    fn family_exists(&self, name: &str) -> bool {
        unsafe {
            let mut coll = None;
            if self.dw.GetSystemFontCollection(&mut coll, false).is_err() {
                return false;
            }
            let Some(coll) = coll else { return false };
            let n = wide(name);
            let mut idx = 0u32;
            let mut exists = BOOL(0);
            coll.FindFamilyName(PCWSTR(n.as_ptr()), &mut idx, &mut exists).is_ok() && exists.as_bool()
        }
    }

    fn format(&self, family: &str, size: f32, weight: DWRITE_FONT_WEIGHT) -> Result<IDWriteTextFormat> {
        let f = wide(family);
        unsafe {
            self.dw.CreateTextFormat(
                PCWSTR(f.as_ptr()),
                None,
                weight,
                DWRITE_FONT_STYLE_NORMAL,
                DWRITE_FONT_STRETCH_NORMAL,
                size,
                windows::core::w!("en-us"),
            )
        }
    }

    /// (Re)creates fonts for the editor font/size and measures a cell.
    pub fn make_fonts(&mut self, family: &str, size: f32, tab: usize) -> Result<()> {
        let fam = [family, "Cascadia Mono", "Consolas", "Courier New"]
            .into_iter()
            .find(|f| self.family_exists(f))
            .unwrap_or("Consolas")
            .to_string();
        self.font_px = size;
        let mono = self.format(&fam, size, DWRITE_FONT_WEIGHT_NORMAL)?;
        unsafe {
            let probe: Vec<u16> = "M".repeat(100).encode_utf16().collect();
            let lay = self.dw.CreateTextLayout(&probe, &mono, 10000.0, 1000.0)?;
            let mut m = DWRITE_TEXT_METRICS::default();
            lay.GetMetrics(&mut m)?;
            self.char_w = m.widthIncludingTrailingWhitespace / 100.0;
            let mut lm = [DWRITE_LINE_METRICS::default(); 1];
            let mut cnt = 0;
            let _ = lay.GetLineMetrics(Some(&mut lm), &mut cnt);
            let natural = lm[0].height.max(size);
            self.line_h = (natural * 1.32).round();
            let baseline = lm[0].baseline + ((self.line_h - natural) / 2.0).floor();
            for f in [&mono] {
                f.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP)?;
            }
            let wrap = self.format(&fam, size, DWRITE_FONT_WEIGHT_NORMAL)?;
            wrap.SetWordWrapping(DWRITE_WORD_WRAPPING_WRAP)?;
            let num = self.format(&fam, size * 0.9, DWRITE_FONT_WEIGHT_NORMAL)?;
            num.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_TRAILING)?;
            num.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP)?;
            num.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER)?;
            for f in [&mono, &wrap] {
                f.SetLineSpacing(DWRITE_LINE_SPACING_METHOD_UNIFORM, self.line_h, baseline)?;
                f.SetIncrementalTabStop(self.char_w * tab as f32)?;
            }
            self.mono = Some(mono);
            self.mono_wrap = Some(wrap);
            self.mono_num = Some(num);
        }
        if self.ui.is_none() {
            let ui = self.format("Segoe UI", 12.5, DWRITE_FONT_WEIGHT_NORMAL)?;
            let bold = self.format("Segoe UI", 12.5, DWRITE_FONT_WEIGHT_SEMI_BOLD)?;
            let small = self.format("Segoe UI", 11.5, DWRITE_FONT_WEIGHT_NORMAL)?;
            unsafe {
                for f in [&ui, &bold, &small] {
                    f.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP)?;
                    f.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER)?;
                }
                let sign = self.dw.CreateEllipsisTrimmingSign(&ui)?;
                let trim = DWRITE_TRIMMING { granularity: DWRITE_TRIMMING_GRANULARITY_CHARACTER, delimiter: 0, delimiterCount: 0 };
                ui.SetTrimming(&trim, &sign)?;
                let sign = self.dw.CreateEllipsisTrimmingSign(&small)?;
                small.SetTrimming(&trim, &sign)?;
            }
            self.ui = Some(ui);
            self.ui_bold = Some(bold);
            self.ui_small = Some(small);
        }
        Ok(())
    }

    // ---- drawing helpers (all coordinates in DIPs) ---------------------------

    pub fn rt(&self) -> &ID2D1RenderTarget {
        self.rt.as_ref().unwrap()
    }

    pub fn fill(&self, x: f32, y: f32, w: f32, h: f32, c: C) {
        unsafe { self.rt().FillRectangle(&rect(x, y, w, h), self.b(c)) }
    }

    /// Fills with an arbitrary colour at `alpha`.
    pub fn fill_tint(&self, x: f32, y: f32, w: f32, h: f32, rgb: u32, alpha: f32) {
        unsafe { self.rt().FillRectangle(&rect(x, y, w, h), &self.tint(rgb, alpha)) }
    }

    pub fn line_tint(&self, x0: f32, y0: f32, x1: f32, y1: f32, rgb: u32, alpha: f32, w: f32) {
        unsafe { self.rt().DrawLine(D2D_POINT_2F { x: x0, y: y0 }, D2D_POINT_2F { x: x1, y: y1 }, &self.tint(rgb, alpha), w, None) }
    }

    pub fn fill_round(&self, x: f32, y: f32, w: f32, h: f32, r: f32, c: C) {
        let rr = D2D1_ROUNDED_RECT { rect: rect(x, y, w, h), radiusX: r, radiusY: r };
        unsafe { self.rt().FillRoundedRectangle(&rr, self.b(c)) }
    }

    pub fn stroke_round(&self, x: f32, y: f32, w: f32, h: f32, r: f32, c: C, width: f32) {
        let rr = D2D1_ROUNDED_RECT { rect: rect(x + 0.5, y + 0.5, w - 1.0, h - 1.0), radiusX: r, radiusY: r };
        unsafe { self.rt().DrawRoundedRectangle(&rr, self.b(c), width, None) }
    }

    pub fn line(&self, x0: f32, y0: f32, x1: f32, y1: f32, c: C, w: f32) {
        unsafe { self.rt().DrawLine(D2D_POINT_2F { x: x0, y: y0 }, D2D_POINT_2F { x: x1, y: y1 }, self.b(c), w, None) }
    }

    pub fn text(&self, s: &str, f: &IDWriteTextFormat, x: f32, y: f32, w: f32, h: f32, c: C) {
        let v: Vec<u16> = s.encode_utf16().collect();
        unsafe {
            self.rt().DrawText(&v, f, &rect(x, y, w, h), self.b(c), D2D1_DRAW_TEXT_OPTIONS_CLIP, DWRITE_MEASURING_MODE_NATURAL);
        }
    }

    pub fn measure(&self, s: &str, f: &IDWriteTextFormat) -> f32 {
        let v: Vec<u16> = s.encode_utf16().collect();
        unsafe {
            let Ok(l) = self.dw.CreateTextLayout(&v, f, 4000.0, 100.0) else { return 0.0 };
            let mut m = DWRITE_TEXT_METRICS::default();
            let _ = l.GetMetrics(&mut m);
            m.widthIncludingTrailingWhitespace
        }
    }

    pub fn clip(&self, x: f32, y: f32, w: f32, h: f32) {
        unsafe { self.rt().PushAxisAlignedClip(&rect(x, y, w, h), D2D1_ANTIALIAS_MODE_ALIASED) }
    }

    pub fn unclip(&self) {
        unsafe { self.rt().PopAxisAlignedClip() }
    }
}

pub fn rect(x: f32, y: f32, w: f32, h: f32) -> D2D_RECT_F {
    D2D_RECT_F { left: x, top: y, right: x + w, bottom: y + h }
}
