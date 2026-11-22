//! 文書が 0 件のときに本文の場所へ描く案内（V1）。
//!
//! 「新規」「開く」と、最近使ったファイル数件をクリックできるリンクとして、
//! テーマの色で描く。編集欄（Scintilla）は隠れているので、メインウィンドウが描く。

use windows::Win32::Foundation::{POINT, RECT};
use windows::Win32::Graphics::Gdi::{GetDC, ReleaseDC, ScreenToClient, HDC};
use windows::Win32::UI::WindowsAndMessaging::GetCursorPos;

use crate::command::CommandId;
use crate::ui::window::{self, Canvas};

use super::fileops::shorten_path;
use super::App;

/// 案内に出す最近使ったファイルの最大件数。
pub(super) const EMPTY_RECENT_MAX: usize = 5;

/// 案内の 1 行。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct EmptyLine {
    pub text: String,
    pub action: Option<EmptyAction>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum EmptyAction {
    New,
    Open,
    Recent(usize),
}

/// 案内の行を作る（描画に依存しない）。`new_key`・`open_key` はキー割り当ての表記
/// （割り当てがなければ `None`）。
pub(super) fn empty_lines(
    new_key: Option<&str>,
    open_key: Option<&str>,
    recent: &[String],
) -> Vec<EmptyLine> {
    let mut lines = [
        ("新規", new_key, EmptyAction::New),
        ("開く", open_key, EmptyAction::Open),
    ]
    .into_iter()
    .map(|(label, key, action)| EmptyLine {
        text: key.map_or_else(|| label.to_owned(), |key| format!("{label}  ({key})")),
        action: Some(action),
    })
    .collect::<Vec<_>>();
    if !recent.is_empty() {
        lines.push(EmptyLine {
            text: String::new(),
            action: None,
        });
        lines.push(EmptyLine {
            text: "最近使ったファイル".to_owned(),
            action: None,
        });
        for (i, p) in recent.iter().take(EMPTY_RECENT_MAX).enumerate() {
            lines.push(EmptyLine {
                text: shorten_path(p, 70),
                action: Some(EmptyAction::Recent(i + 1)),
            });
        }
    }
    lines
}

impl App {
    fn chord_text(&self, command: &'static str) -> Option<String> {
        self.keymap
            .chord_for(CommandId::new(command))
            .map(|c| c.to_string())
    }

    /// 文書が 0 件のときの案内を描く（`WM_PAINT` の中で）。リンクの当たり判定も更新する。
    pub(super) fn paint_empty_view(&mut self, hdc: HDC) {
        let c = Canvas { hdc, dpi: self.dpi };
        let area = self.layout.editor;
        c.fill(&area, self.theme.editor.bg);
        c.set_font(&self.font);
        let lines = empty_lines(
            self.chord_text("file.new").as_deref(),
            self.chord_text("file.open").as_deref(),
            &self.settings.recent_files,
        );
        let line_h = c.px(26);
        let total = line_h * lines.len() as i32;
        let width = (c.px(560))
            .min(area.right - area.left - c.px(32))
            .max(c.px(120));
        let left = area.left + ((area.right - area.left - width) / 2).max(c.px(16));
        let mut top = area.top + ((area.bottom - area.top - total) / 3).max(c.px(16));
        self.empty_links.clear();
        for line in &lines {
            let r = RECT {
                left,
                top,
                right: left + width,
                bottom: top + line_h,
            };
            let color = if line.action.is_some() {
                self.theme.ui.accent
            } else {
                self.theme.ui.status_dim
            };
            c.text(&r, &line.text, color, false);
            if let Some(action) = line.action {
                let right = r.right.min(r.left + c.measure(&line.text) + c.px(8));
                self.empty_links.push((RECT { right, ..r }, action));
            }
            top += line_h;
        }
    }

    /// 自己診断用: 案内を（画面の DC に）描いて、当たり判定を作る。
    pub(crate) fn paint_for_test(&mut self) {
        // SAFETY: ウィンドウの DC を借りて返す。
        unsafe {
            let hdc = GetDC(Some(self.hwnd));
            self.paint_empty_view(hdc);
            ReleaseDC(Some(self.hwnd), hdc);
        }
    }

    /// 案内のリンクの上の座標なら、対応する操作。
    pub(super) fn empty_link_at(&self, x: i32, y: i32) -> Option<EmptyAction> {
        if self.active.is_some() {
            return None;
        }
        self.empty_links
            .iter()
            .find(|(r, _)| window::contains(r, x, y))
            .map(|(_, n)| *n)
    }

    /// マウスカーソルが案内のリンクの上にあるか。
    pub(super) fn cursor_over_empty_link(&self) -> bool {
        let mut p = POINT::default();
        // SAFETY: 出力先は POINT 1 つ。
        unsafe {
            let _ = GetCursorPos(&mut p);
            let _ = ScreenToClient(self.hwnd, &mut p);
        }
        self.empty_link_at(p.x, p.y).is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lines_show_keys_and_numbered_recent_files() {
        let lines = empty_lines(Some("Ctrl+N"), Some("Ctrl+O"), &[]);
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].text, "新規  (Ctrl+N)");
        assert_eq!(lines[1].text, "開く  (Ctrl+O)");
        let recent: Vec<String> = (1..=8).map(|i| format!("C:\\work\\{i}.txt")).collect();
        let lines = empty_lines(Some("Ctrl+N"), Some("Ctrl+O"), &recent);
        let links: Vec<_> = lines
            .iter()
            .filter_map(|l| match l.action {
                Some(EmptyAction::Recent(index)) => Some(index),
                _ => None,
            })
            .collect();
        assert_eq!(links, vec![1, 2, 3, 4, 5]);
        assert_eq!(lines.last().unwrap().text, "C:\\work\\5.txt");
        // 割り当てがなくてもクリックできる。
        let lines = empty_lines(None, None, &[]);
        assert_eq!(lines[0].text, "新規");
        assert_eq!(lines[0].action, Some(EmptyAction::New));
    }
}
