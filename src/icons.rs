//! Slate's own icon set: stroked vector paths on a 16x16 grid, parsed once
//! into Direct2D geometries and drawn crisp at any DPI.

use windows::core::Result;
use windows::Win32::Graphics::Direct2D::Common::{
    D2D1_BEZIER_SEGMENT, D2D1_FIGURE_BEGIN_HOLLOW, D2D1_FIGURE_END_CLOSED, D2D1_FIGURE_END_OPEN, D2D_POINT_2F, D2D_SIZE_F,
};
use windows::Win32::Graphics::Direct2D::{
    ID2D1Factory, ID2D1PathGeometry, D2D1_ARC_SEGMENT, D2D1_ARC_SIZE_LARGE, D2D1_ARC_SIZE_SMALL,
    D2D1_SWEEP_DIRECTION_CLOCKWISE, D2D1_SWEEP_DIRECTION_COUNTER_CLOCKWISE,
};

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Icon {
    Menu,
    Minimize,
    Maximize,
    Restore,
    Close,
    Plus,
    ChevronUp,
    ChevronDown,
    ChevronRight,
    Search,
    /// The app mark: an S of pipe (top run, return bends, bottom run).
    Pipe,
    Update,
}

impl Icon {
    /// Path on a 16x16 grid. Absolute commands: M L H V A C Z.
    pub fn path(self) -> &'static str {
        match self {
            Icon::Menu => "M2.5 4.5 H13.5 M2.5 8 H13.5 M2.5 11.5 H13.5",
            Icon::Minimize => "M3.5 8 H12.5",
            Icon::Maximize => "M4 3.5 H12 A0.5 0.5 0 0 1 12.5 4 V12 A0.5 0.5 0 0 1 12 12.5 H4 A0.5 0.5 0 0 1 3.5 12 V4 A0.5 0.5 0 0 1 4 3.5 Z",
            Icon::Restore => "M5.5 3.5 H12.5 V10.5 M3.5 5.5 H10.5 V12.5 H3.5 Z",
            Icon::Close => "M4 4 L12 12 M12 4 L4 12",
            Icon::Plus => "M8 3 V13 M3 8 H13",
            Icon::ChevronUp => "M4 10 L8 6 L12 10",
            Icon::ChevronDown => "M4 6 L8 10 L12 6",
            Icon::ChevronRight => "M6 4 L10 8 L6 12",
            Icon::Search => "M10.5 6.75 A3.75 3.75 0 1 1 3 6.75 A3.75 3.75 0 1 1 10.5 6.75 M9.5 9.5 L13.5 13.5",
            Icon::Pipe => "M12.5 3.5 H7 A2.25 2.25 0 0 0 7 8 H9 A2.25 2.25 0 0 1 9 12.5 H3.5",
            Icon::Update => "M8 11 V3 M4.5 6.5 L8 3 L11.5 6.5 M3 13.5 H13",
        }
    }
}

fn nums(s: &str) -> Vec<f32> {
    s.split(|c: char| c.is_whitespace() || c == ',').filter(|t| !t.is_empty()).filter_map(|t| t.parse().ok()).collect()
}

/// Splits a path into (command, numbers) pairs.
pub fn tokens(path: &str) -> Vec<(char, Vec<f32>)> {
    let mut out = Vec::new();
    let mut cmd = None;
    let mut start = 0;
    for (i, c) in path.char_indices() {
        if c.is_ascii_alphabetic() {
            if let Some(k) = cmd {
                out.push((k, nums(&path[start..i])));
            }
            cmd = Some(c);
            start = i + 1;
        }
    }
    if let Some(k) = cmd {
        out.push((k, nums(&path[start..])));
    }
    out
}

pub fn build(d2d: &ID2D1Factory, icon: Icon) -> Result<ID2D1PathGeometry> {
    let p = |x: f32, y: f32| D2D_POINT_2F { x, y };
    unsafe {
        let geo = d2d.CreatePathGeometry()?;
        let sink = geo.Open()?;
        let mut open = false;
        let (mut cx, mut cy) = (0.0f32, 0.0f32);
        for (c, n) in tokens(icon.path()) {
            match c {
                'M' => {
                    if open {
                        sink.EndFigure(D2D1_FIGURE_END_OPEN);
                    }
                    (cx, cy) = (n[0], n[1]);
                    sink.BeginFigure(p(cx, cy), D2D1_FIGURE_BEGIN_HOLLOW);
                    open = true;
                }
                'L' => {
                    (cx, cy) = (n[0], n[1]);
                    sink.AddLine(p(cx, cy));
                }
                'H' => {
                    cx = n[0];
                    sink.AddLine(p(cx, cy));
                }
                'V' => {
                    cy = n[0];
                    sink.AddLine(p(cx, cy));
                }
                'A' => {
                    (cx, cy) = (n[5], n[6]);
                    sink.AddArc(&D2D1_ARC_SEGMENT {
                        point: p(cx, cy),
                        size: D2D_SIZE_F { width: n[0], height: n[1] },
                        rotationAngle: n[2],
                        sweepDirection: if n[4] != 0.0 { D2D1_SWEEP_DIRECTION_CLOCKWISE } else { D2D1_SWEEP_DIRECTION_COUNTER_CLOCKWISE },
                        arcSize: if n[3] != 0.0 { D2D1_ARC_SIZE_LARGE } else { D2D1_ARC_SIZE_SMALL },
                    });
                }
                'C' => {
                    (cx, cy) = (n[4], n[5]);
                    sink.AddBezier(&D2D1_BEZIER_SEGMENT { point1: p(n[0], n[1]), point2: p(n[2], n[3]), point3: p(cx, cy) });
                }
                'Z' => {
                    sink.EndFigure(D2D1_FIGURE_END_CLOSED);
                    open = false;
                }
                _ => {}
            }
        }
        if open {
            sink.EndFigure(D2D1_FIGURE_END_OPEN);
        }
        sink.Close()?;
        Ok(geo)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_icon_parses() {
        for icon in [
            Icon::Menu, Icon::Minimize, Icon::Maximize, Icon::Restore, Icon::Close, Icon::Plus, Icon::ChevronUp,
            Icon::ChevronDown, Icon::ChevronRight, Icon::Search, Icon::Pipe, Icon::Update,
        ] {
            for (c, n) in tokens(icon.path()) {
                let want = match c {
                    'M' | 'L' => 2,
                    'H' | 'V' => 1,
                    'A' => 7,
                    'C' => 6,
                    'Z' => 0,
                    _ => panic!("{icon:?}: unknown command {c}"),
                };
                assert_eq!(n.len(), want, "{icon:?} {c}");
                assert!(n.iter().all(|v| (0.0..=16.0).contains(v)), "{icon:?} off grid");
            }
        }
    }
}
