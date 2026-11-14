//! ステータスバーの独自描画（行・列、文字コード、改行形式、読み取り専用の表示、短い状態表示）。
//! 文字コードと改行の表示は [`crate::text::TextFormat::label`]／[`crate::text::Eol::label`]。
//!
//! 左に短い状態表示（「保存しました」など）、右に項目を並べる:
//! `[読み取り専用] [外部で変更] [AI の変更]  行 12, 列 5 (3 文字選択)  言語  UTF-8  CRLF`。
//! 項目のクリックは [`StatusSegment`] を返すだけで、操作（文字コードの変更など）は
//! `ui::app` のフック経由で統合側（I1）が行う。
//!
//! 担当: タスク B。

use windows::Win32::Foundation::RECT;
use windows::Win32::Graphics::Gdi::HDC;

use crate::text::TextFormat;
use crate::ui::theme::{Rgb, Theme};
use crate::ui::window::{BufferedPaint, Canvas, UiFont};

/// 状態表示の重さ（色を変える）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MessageKind {
    Info,
    Warning,
}

/// 右側の項目。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum StatusSegment {
    ReadOnly,
    ExternalChange,
    AiEdits,
    Position,
    Language,
    Encoding,
    Eol,
    Message,
}

/// 表示内容。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct StatusInfo {
    /// 行（1 始まり）。
    pub line: u64,
    /// 列（1 始まり。文字数）。
    pub column: u64,
    /// 選択している文字数（選択がなければ 0）。
    pub selected_chars: u64,
    pub language: String,
    /// 文字コード（[`TextFormat::label`]）。
    pub encoding: String,
    /// 改行形式（[`crate::text::Eol::label`]）。
    pub eol: String,
    pub read_only: bool,
    pub external_change: bool,
    pub ai_edits: bool,
}

impl StatusInfo {
    pub fn with_format(mut self, format: &TextFormat) -> Self {
        self.encoding = format.label();
        self.eol = format.eol.label().to_owned();
        self
    }

    /// 右側の項目の表示文字列（左から順）。
    pub fn segments(&self) -> Vec<(StatusSegment, String)> {
        let mut out = Vec::new();
        if self.read_only {
            out.push((StatusSegment::ReadOnly, "読み取り専用".to_owned()));
        }
        if self.external_change {
            out.push((StatusSegment::ExternalChange, "外部で変更".to_owned()));
        }
        if self.ai_edits {
            out.push((StatusSegment::AiEdits, "AI の変更あり".to_owned()));
        }
        let mut pos = format!("行 {}, 列 {}", self.line, self.column);
        if self.selected_chars > 0 {
            pos.push_str(&format!(" ({} 文字選択)", self.selected_chars));
        }
        out.push((StatusSegment::Position, pos));
        if !self.language.is_empty() {
            out.push((StatusSegment::Language, self.language.clone()));
        }
        if !self.encoding.is_empty() {
            out.push((StatusSegment::Encoding, self.encoding.clone()));
        }
        if !self.eol.is_empty() {
            out.push((StatusSegment::Eol, self.eol.clone()));
        }
        out
    }
}

/// 右から詰めて並べる。`widths` は各項目の文字の幅。戻り値は各項目の (左端, 幅)。
/// 左側に `min_left` 以上の余白が残らない項目は、左のものから省く（`None`）。
pub fn layout_segments(
    widths: &[i32],
    right: i32,
    pad: i32,
    min_left: i32,
) -> Vec<Option<(i32, i32)>> {
    let mut out = vec![None; widths.len()];
    let mut x = right;
    // 右端（改行形式）から順に置き、入らなくなったらそこで止める。
    for i in (0..widths.len()).rev() {
        let w = widths[i] + pad * 2;
        if x - w < min_left {
            break;
        }
        x -= w;
        out[i] = Some((x, w));
    }
    out
}

/// ステータスバーの状態。
#[derive(Debug, Default)]
pub struct StatusBar {
    info: StatusInfo,
    message: Option<(String, MessageKind)>,
    rect: RECT,
    dpi: u32,
    /// 最後に描いた項目の位置（クリックの判定用。クライアント座標）。
    hit: Vec<(StatusSegment, RECT)>,
}

impl StatusBar {
    pub fn new() -> StatusBar {
        StatusBar {
            dpi: 96,
            ..Default::default()
        }
    }

    pub fn info(&self) -> &StatusInfo {
        &self.info
    }

    /// 変わったら `true`。
    pub fn set_info(&mut self, info: StatusInfo) -> bool {
        let changed = info != self.info;
        self.info = info;
        changed
    }

    pub fn message(&self) -> Option<&str> {
        self.message.as_ref().map(|(m, _)| m.as_str())
    }

    pub fn set_message(&mut self, message: Option<(String, MessageKind)>) {
        self.message = message;
    }

    pub fn set_rect(&mut self, rect: RECT, dpi: u32) {
        self.rect = rect;
        self.dpi = dpi;
    }

    pub fn rect(&self) -> RECT {
        self.rect
    }

    /// クリックの位置にある項目。
    pub fn hit_test(&self, x: i32, y: i32) -> Option<StatusSegment> {
        if !crate::ui::window::contains(&self.rect, x, y) {
            return None;
        }
        self.hit
            .iter()
            .find(|(_, r)| crate::ui::window::contains(r, x, y))
            .map(|(s, _)| *s)
    }

    pub fn paint(&mut self, target: HDC, theme: &Theme, font: &UiFont) {
        let Some(buf) = BufferedPaint::new(target, self.rect) else {
            return;
        };
        let c = buf.canvas(self.dpi);
        let ui = &theme.ui;
        let local = buf.local_rect();
        c.fill(&local, ui.status_bg);
        c.set_font(font);
        let pad = c.px(8);
        let segments = self.info.segments();
        let widths: Vec<i32> = segments.iter().map(|(_, s)| c.measure(s)).collect();
        let min_left = c.px(120).min(local.right / 3);
        let positions = layout_segments(&widths, local.right - c.px(4), pad, min_left);
        self.hit.clear();
        let mut leftmost = local.right;
        for ((segment, text), pos) in segments.iter().zip(&positions) {
            let Some((x, w)) = *pos else { continue };
            leftmost = leftmost.min(x);
            let r = RECT {
                left: x,
                right: x + w,
                ..local
            };
            let color = match segment {
                StatusSegment::ReadOnly | StatusSegment::ExternalChange => ui.status_warn,
                StatusSegment::AiEdits => ui.ai_mark,
                StatusSegment::Position => ui.status_fg,
                _ => ui.status_dim,
            };
            draw_centered(&c, &r, text, color);
            self.hit.push((
                *segment,
                RECT {
                    left: r.left + self.rect.left,
                    right: r.right + self.rect.left,
                    top: self.rect.top,
                    bottom: self.rect.bottom,
                },
            ));
        }
        if let Some((message, kind)) = &self.message {
            let color = match kind {
                MessageKind::Info => ui.status_fg,
                MessageKind::Warning => ui.status_warn,
            };
            let r = RECT {
                left: local.left + c.px(10),
                right: leftmost - pad,
                ..local
            };
            c.text(&r, message, color, false);
            self.hit.push((
                StatusSegment::Message,
                RECT {
                    left: self.rect.left + r.left,
                    right: self.rect.left + r.right.max(r.left),
                    top: self.rect.top,
                    bottom: self.rect.bottom,
                },
            ));
        }
    }
}

fn draw_centered(c: &Canvas, r: &RECT, text: &str, color: Rgb) {
    let w = c.measure(text);
    let left = r.left + ((r.right - r.left) - w) / 2;
    c.text(
        &RECT {
            left,
            right: r.right,
            ..*r
        },
        text,
        color,
        false,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::text::{Encoding, Eol};

    #[test]
    fn segments_follow_the_state() {
        let info = StatusInfo {
            line: 12,
            column: 5,
            selected_chars: 3,
            language: "Rust".into(),
            ..Default::default()
        }
        .with_format(&TextFormat {
            encoding: Encoding::Utf8,
            bom: true,
            eol: Eol::Lf,
        });
        let segs = info.segments();
        let names: Vec<_> = segs.iter().map(|(_, s)| s.as_str()).collect();
        assert_eq!(
            names,
            ["行 12, 列 5 (3 文字選択)", "Rust", "UTF-8 (BOM)", "LF"]
        );

        let ro = StatusInfo {
            line: 1,
            column: 1,
            read_only: true,
            external_change: true,
            ai_edits: true,
            ..Default::default()
        };
        let kinds: Vec<_> = ro.segments().into_iter().map(|(k, _)| k).collect();
        assert_eq!(
            kinds,
            [
                StatusSegment::ReadOnly,
                StatusSegment::ExternalChange,
                StatusSegment::AiEdits,
                StatusSegment::Position
            ]
        );
    }

    #[test]
    fn segments_are_packed_from_the_right_and_dropped_when_narrow() {
        let pos = layout_segments(&[50, 30, 20], 500, 5, 100);
        assert_eq!(pos, vec![Some((370, 60)), Some((430, 40)), Some((470, 30))]);
        // 狭いと左の項目から省く。
        let pos = layout_segments(&[50, 30, 20], 200, 5, 100);
        assert_eq!(pos, vec![None, Some((130, 40)), Some((170, 30))]);
    }
}
