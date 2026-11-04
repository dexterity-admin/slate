//! Direct2D / DirectWrite resources: render target, brushes per theme colour,
//! text formats and font metrics.

use crate::sys::wide;
use windows::core::{Interface, Result, PCWSTR};
use windows::Win32::Foundation::{BOOL, HWND};
use windows::Win32::Graphics::Direct2D::Common::{D2D1_COLOR_F, D2D_POINT_2F, D2D_RECT_F, D2D_SIZE_U};
use windows::Win32::Graphics::Direct2D::{
    D2D1CreateFactory, ID2D1Factory, ID2D1HwndRenderTarget, ID2D1RenderTarget, ID2D1SolidColorBrush,
    D2D1_ANTIALIAS_MODE_ALIASED, D2D1_DRAW_TEXT_OPTIONS_CLIP, D2D1_FACTORY_TYPE_SINGLE_THREADED,
    D2D1_HWND_RENDER_TARGET_PROPERTIES, D2D1_PRESENT_OPTIONS_NONE, D2D1_RENDER_TARGET_PROPERTIES,
    D2D1_ROUNDED_RECT, D2D1_TEXT_ANTIALIAS_MODE_CLEARTYPE,
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
    pub icons: Option<IDWriteTextFormat>,
    pub icons_small: Option<IDWriteTextFormat>,
    pub char_w: f32,
    pub line_h: f32,
    pub font_px: f32,
    pub dark: bool,
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
                icons: None,
                icons_small: None,
                char_w: 8.0,
                line_h: 18.0,
                font_px: 14.0,
                dark: true,
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
        self.rt = None;
        self.hrt = None;
    }

    pub fn make_brushes(&mut self) -> Result<()> {
        let Some(rt) = &self.rt else { return Ok(()) };
        let pal = if self.dark { &DARK } else { &LIGHT };
        self.brushes.clear();
        for &c in pal.iter() {
            self.brushes.push(unsafe { rt.CreateSolidColorBrush(&hex(c), None)? });
        }
        Ok(())
    }

    pub fn b(&self, c: C) -> &ID2D1SolidColorBrush {
        &self.brushes[c as usize]
    }

    pub fn color(&self, c: C) -> D2D1_COLOR_F {
        hex(if self.dark { DARK[c as usize] } else { LIGHT[c as usize] })
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
            let icon_font = if self.family_exists("Segoe Fluent Icons") { "Segoe Fluent Icons" } else { "Segoe MDL2 Assets" };
            let icons = self.format(icon_font, 10.0, DWRITE_FONT_WEIGHT_NORMAL)?;
            let icons_small = self.format(icon_font, 8.0, DWRITE_FONT_WEIGHT_NORMAL)?;
            unsafe {
                for f in [&ui, &bold, &small, &icons, &icons_small] {
                    f.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP)?;
                    f.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER)?;
                }
                for f in [&icons, &icons_small] {
                    f.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_CENTER)?;
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
            self.icons = Some(icons);
            self.icons_small = Some(icons_small);
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

    pub fn icon(&self, glyph: char, x: f32, y: f32, w: f32, h: f32, c: C, small: bool) {
        let f = if small { self.icons_small.as_ref() } else { self.icons.as_ref() };
        self.text(&glyph.to_string(), f.unwrap(), x, y, w, h, c);
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
