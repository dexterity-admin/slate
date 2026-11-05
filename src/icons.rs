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
    // Toolbar.
    New,
    Open,
    Save,
    SaveAll,
    CloseFile,
    Cut,
    Copy,
    Paste,
    Undo,
    Redo,
    Replace,
    FindFiles,
    ZoomIn,
    ZoomOut,
    Wrap,
    Pilcrow,
    Split,
    Compare,
    Grid,
    Record,
    Stop,
    Play,
    Follow,
    People,
    Spark,
}

#[cfg(test)]
pub const ALL: &[Icon] = &[
    Icon::Menu, Icon::Minimize, Icon::Maximize, Icon::Restore, Icon::Close, Icon::Plus, Icon::ChevronUp,
    Icon::ChevronDown, Icon::ChevronRight, Icon::Search, Icon::Pipe, Icon::Update, Icon::New, Icon::Open,
    Icon::Save, Icon::SaveAll, Icon::CloseFile, Icon::Cut, Icon::Copy, Icon::Paste, Icon::Undo, Icon::Redo,
    Icon::Replace, Icon::FindFiles, Icon::ZoomIn, Icon::ZoomOut, Icon::Wrap, Icon::Pilcrow, Icon::Split,
    Icon::Compare, Icon::Grid, Icon::Record, Icon::Stop, Icon::Play, Icon::Follow, Icon::People, Icon::Spark,
];

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
            Icon::New => "M9.5 2.5 H4 V13.5 H12.5 V5.5 Z M9.5 2.5 V5.5 H12.5",
            Icon::Open => "M2.5 12.5 V3.5 H6 L7.5 5 H12.5 V7",
            Icon::Save => "M2.5 2.5 H11 L13.5 5 V13.5 H2.5 Z M5 2.5 V5.5 H10 V2.5",
            Icon::SaveAll => "M4.5 2.5 V1.5 H11.5 L14.5 4.5 V11.5 H13.5 M1.5 4.5 H8.5 L11.5 7.5 V14.5 H1.5 Z M4 4.5 V7 H8 V4.5",
            Icon::CloseFile => "M2.5 13.5 V5 A1.5 1.5 0 0 1 4 3.5 H12 A1.5 1.5 0 0 1 13.5 5 V13.5",
            Icon::Cut => "M7 11.5 A2 2 0 1 1 3 11.5 A2 2 0 1 1 7 11.5 M13 11.5 A2 2 0 1 1 9 11.5 A2 2 0 1 1 13 11.5 M6.2 10 L11.5 2.5 M9.8 10 L4.5 2.5",
            Icon::Copy => "M6 5.5 H13 V13.5 H6 Z M10 5.5 V2.5 H3 V10.5 H6",
            Icon::Paste => "M5.5 3.5 H3.5 V14 H12.5 V3.5 H10.5",
            Icon::Undo => "M5.5 9 L2.5 6 L5.5 3 M2.5 6 H9.5 A3.5 3.5 0 0 1 9.5 13 H6",
            Icon::Redo => "M10.5 9 L13.5 6 L10.5 3 M13.5 6 H6.5 A3.5 3.5 0 0 0 6.5 13 H10",
            Icon::Replace => "M9.25 6 A3.25 3.25 0 1 1 2.75 6 A3.25 3.25 0 1 1 9.25 6 M8.3 8.3 L9.5 9.5",
            Icon::FindFiles => "M2.5 12.5 V3.5 H6 L7.5 5 H13.5 V6.5 M2.5 12.5 H6.5",
            Icon::ZoomIn | Icon::ZoomOut => "M10.5 6.75 A3.75 3.75 0 1 1 3 6.75 A3.75 3.75 0 1 1 10.5 6.75 M9.5 9.5 L13.5 13.5",
            Icon::Wrap => "M2.5 4 H13.5 M2.5 8 H11 A2 2 0 0 1 11 12 H8 M2.5 12 H5",
            Icon::Pilcrow => "M9.5 13.5 V2.5 M12 13.5 V2.5 M13.5 2.5 H7.5 A3 3 0 0 0 7.5 8.5 H9.5",
            Icon::Split => "M2.5 3.5 H13.5 V12.5 H2.5 Z M8 3.5 V12.5",
            Icon::Compare => "M2.5 2.5 H7 V13.5 H2.5 Z M9 2.5 H13.5 V13.5 H9 Z",
            Icon::Grid => "M2.5 3.5 H13.5 V12.5 H2.5 Z M2.5 6.5 H13.5 M2.5 9.5 H13.5 M6.5 6.5 V12.5 M10 6.5 V12.5",
            Icon::Record => "M12.5 8 A4.5 4.5 0 1 1 3.5 8 A4.5 4.5 0 1 1 12.5 8",
            Icon::Stop => "M4.5 4.5 H11.5 V11.5 H4.5 Z",
            Icon::Play => "M5 3 L13 8 L5 13 Z",
            Icon::Follow => "M2.5 3 H12.5 M2.5 6 H12.5 M2.5 9 H7.5 M2.5 12 H6",
            Icon::People => "M8 5.5 A2 2 0 1 1 4 5.5 A2 2 0 1 1 8 5.5 M2 13 A4 4 0 0 1 10 13",
            Icon::Spark => "M7 2 C7.5 5.5 9 7 12.5 7.5 C9 8 7.5 9.5 7 13 C6.5 9.5 5 8 1.5 7.5 C5 7 6.5 5.5 7 2 Z",
        }
    }

    /// A second stroke drawn in the accent colour: the part of the icon that
    /// says what it does (the plus on New, the label on Save, the arrow on
    /// Follow), so the toolbar reads at a glance without turning into a
    /// jumble of colours.
    pub fn accent_path(self) -> Option<&'static str> {
        Some(match self {
            Icon::New => "M8.25 7.5 V11.5 M6.25 9.5 H10.25",
            Icon::Open => "M2.5 12.5 L4.5 7.5 H14.5 L12.5 12.5 Z",
            Icon::Save => "M5 13.5 V9.5 H11 V13.5",
            Icon::SaveAll => "M4 14.5 V11.5 H9 V14.5",
            Icon::CloseFile => "M6 7 L10 11 M10 7 L6 11",
            Icon::Paste => "M6 2 H10 V5 H6 Z M6 8.5 H10 M6 11 H10",
            Icon::Replace => "M10.5 14 L14 10.5 L12.5 9 L9 12.5 V14 Z",
            Icon::FindFiles => "M13.5 10.5 A2.5 2.5 0 1 1 8.5 10.5 A2.5 2.5 0 1 1 13.5 10.5 M12.8 12.3 L14.5 14",
            Icon::ZoomIn => "M6.75 4.75 V8.75 M4.75 6.75 H8.75",
            Icon::ZoomOut => "M4.75 6.75 H8.75",
            Icon::Wrap => "M9.5 10.5 L8 12 L9.5 13.5",
            Icon::Compare => "M4 6 H5.5 M10.5 6 H12 M10.5 9 H12 M10.5 11 H12",
            Icon::Grid => "M2.5 3.5 H13.5 V6.5 H2.5 Z",
            Icon::Record => "M9.5 8 A1.5 1.5 0 1 1 6.5 8 A1.5 1.5 0 1 1 9.5 8",
            Icon::Follow => "M11 7 V13.5 M8.5 11 L11 13.5 L13.5 11",
            Icon::People => "M12.5 6 A1.75 1.75 0 1 1 9 6 A1.75 1.75 0 1 1 12.5 6 M11.5 9.6 A3.5 3.5 0 0 1 14.5 13",
            Icon::Spark => "M12.5 1.5 V4.5 M11 3 H14 M13 11 V14 M11.5 12.5 H14.5",
            _ => return None,
        })
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

pub fn build(d2d: &ID2D1Factory, path: &str) -> Result<ID2D1PathGeometry> {
    let p = |x: f32, y: f32| D2D_POINT_2F { x, y };
    unsafe {
        let geo = d2d.CreatePathGeometry()?;
        let sink = geo.Open()?;
        let mut open = false;
        let (mut cx, mut cy) = (0.0f32, 0.0f32);
        for (c, n) in tokens(path) {
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
        for &icon in ALL {
            for (c, n) in tokens(icon.path()).into_iter().chain(icon.accent_path().map(tokens).unwrap_or_default()) {
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
