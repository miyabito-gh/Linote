//! 表示の設定（折り返し、空白・改行の表示、現在行、括弧、ガイド線、タブ、拡大）を
//! 編集欄に反映する。設定の値は [`ViewSettings`]（`settings.json` の `view`）。
//!
//! 書体と文字サイズは [`Editor::apply_theme`](super::Editor::apply_theme) が設定の値を
//! 使って付ける（スタイルの全消去と一緒に行うため）。書体を変えたら、設定を反映した後に
//! `apply_theme` を呼び直す。
//!
//! 画面に依存しない計算（右余白、インデントの取り出し）は純粋な関数にしてある。

use windows::Win32::Foundation::RECT;
use windows::Win32::UI::WindowsAndMessaging::GetClientRect;

use super::sci::*;
use super::{cstr, Editor};
use crate::appdata::settings::{ViewSettings, WrapMode, ZOOM_MAX, ZOOM_MIN};

/// 括弧の強調をする本文の大きさの上限（バイト）。対応する括弧の探索は文書の端まで
/// 走ることがあるので、大きい文書では行わない。
pub const BRACE_MATCH_MAX_BYTES: u64 = 2 * 1024 * 1024;

/// 桁指定の折り返しで、右余白に設定する幅（ピクセル）。本文の領域の幅が
/// `桁数 × 1 文字の幅` になるようにする。ウィンドウがそれより狭ければ 0。
/// 1 ピクセルの余裕を見て、ちょうどの桁数が次の行に折り返されないようにする。
pub fn right_margin_for_column(client_width: i32, text_start: i32, columns_px: i32) -> i32 {
    (client_width - text_start - columns_px - 1).max(0)
}

/// 行頭の空白（スペースとタブ）の部分。
pub fn leading_indent(line: &[u8]) -> &[u8] {
    let n = line
        .iter()
        .take_while(|&&b| b == b' ' || b == b'\t')
        .count();
    &line[..n]
}

/// 対応を調べる括弧か。
pub fn is_brace(b: u8) -> bool {
    matches!(b, b'(' | b')' | b'[' | b']' | b'{' | b'}')
}

impl Editor {
    /// 適用中の表示設定。
    pub fn view_settings(&self) -> ViewSettings {
        self.view.borrow().clone()
    }

    /// 表示設定を保持して編集欄に反映する。書体、文字サイズ、現在行の強調を入れる色は
    /// [`apply_theme`](Editor::apply_theme) で付くので、呼んだ後に `apply_theme` も呼ぶこと。
    pub fn apply_view(&self, view: &ViewSettings) {
        *self.view.borrow_mut() = view.clone();
        let wrap = match view.wrap {
            WrapMode::None => SC_WRAP_NONE,
            WrapMode::Word | WrapMode::Column => SC_WRAP_WORD,
            WrapMode::Char => SC_WRAP_CHAR,
        };
        self.send(SCI_SETWRAPMODE, wrap, 0);
        // 折り返した行は、元の行と同じ深さにそろえる。
        self.send(SCI_SETWRAPINDENTMODE, SC_WRAPINDENT_SAME, 0);
        self.send(
            SCI_SETVIEWWS,
            if view.show_whitespace {
                SCWS_VISIBLEALWAYS
            } else {
                SCWS_INVISIBLE
            },
            0,
        );
        self.send(SCI_SETVIEWEOL, view.show_eol as usize, 0);
        // 現在行の強調: 切るのはここで、入れるときの色は apply_theme が付ける。
        if !view.highlight_current_line {
            self.send(SCI_SETCARETLINEVISIBLE, 0, 0);
        }
        let edge = view.wrap == WrapMode::Column && view.edge_guide;
        self.send(SCI_SETEDGEMODE, if edge { EDGE_LINE } else { EDGE_NONE }, 0);
        self.send(SCI_SETEDGECOLUMN, view.wrap_column as usize, 0);
        self.apply_document_options();
        if self.zoom() != view.zoom {
            self.send(SCI_SETZOOM, view.zoom as usize, 0);
        }
        if !view.highlight_braces {
            self.clear_brace_highlight();
        }
        self.update_line_number_margin();
    }

    /// ドキュメントごとの設定（タブ幅、タブの代わりにスペース）を、表示中のドキュメントに付ける。
    pub(super) fn apply_document_options(&self) {
        let (width, spaces) = {
            let v = self.view.borrow();
            (v.tab_width, v.use_spaces)
        };
        self.send(SCI_SETTABWIDTH, width as usize, 0);
        // 0 ならインデント幅はタブ幅と同じ。
        self.send(SCI_SETINDENT, 0, 0);
        self.send(SCI_SETUSETABS, !spaces as usize, 0);
    }

    /// 桁指定の折り返しのとき、右余白を本文の領域が指定の桁数ぶんの幅になるように
    /// 合わせる（ウィンドウの大きさ、余白、書体、拡大が変わったら呼ぶ）。
    /// 桁指定でなければ右余白を 0 にする。
    pub fn update_wrap_margin(&self) {
        let (wrap, columns) = {
            let v = self.view.borrow();
            (v.wrap, v.wrap_column)
        };
        let margin = if wrap == WrapMode::Column {
            let mut r = RECT::default();
            // SAFETY: 自分のウィンドウのクライアント領域を読む。
            unsafe {
                let _ = GetClientRect(self.hwnd, &mut r);
            }
            let sample = cstr(&"0".repeat(columns as usize));
            let columns_px = self.send(SCI_TEXTWIDTH, STYLE_DEFAULT, sample.as_ptr() as isize);
            let text_start = self.send(SCI_GETMARGINLEFT, 0, 0)
                + self.send(SCI_GETMARGINWIDTHN, 0, 0)
                + self.send(SCI_GETMARGINWIDTHN, 1, 0);
            if r.right - r.left <= 0 {
                0
            } else {
                right_margin_for_column(r.right - r.left, text_start as i32, columns_px as i32)
                    as isize
            }
        } else {
            0
        };
        if self.send(SCI_GETMARGINRIGHT, 0, 0) != margin {
            self.send(SCI_SETMARGINRIGHT, 0, margin);
        }
    }

    /// 拡大率（`SCI_GETZOOM`）。
    pub fn zoom(&self) -> i32 {
        self.send(SCI_GETZOOM, 0, 0) as i32
    }

    /// 拡大率を設定する（範囲内に切り詰める）。
    pub fn set_zoom(&self, zoom: i32) {
        self.send(SCI_SETZOOM, zoom.clamp(ZOOM_MIN, ZOOM_MAX) as usize, 0);
    }

    /// 改行の直後に、前の行の行頭の空白を入力する（自動インデント）。
    /// カーソルが改行の直後の行頭にあるときだけ働く。入力した空白のバイト数を返す。
    pub fn auto_indent_after_newline(&self) -> usize {
        if self.is_read_only() || self.send(SCI_GETSELECTIONS, 0, 0) != 1 {
            return 0;
        }
        let caret = self.selection().caret;
        let line = self.line_from_position(caret);
        if line == 0 || self.line_start(line) != Some(caret) {
            return 0;
        }
        let prev = line - 1;
        let Some(prev_start) = self.line_start(prev) else {
            return 0;
        };
        let indent_end = self
            .send(SCI_GETLINEINDENTPOSITION, prev as usize, 0)
            .max(0) as u64;
        let text = self.text_range(prev_start, indent_end.min(self.line_end(prev)));
        let indent = leading_indent(&text);
        if indent.is_empty() {
            return 0;
        }
        self.send(SCI_ADDTEXT, indent.len(), indent.as_ptr() as isize);
        indent.len()
    }

    /// 改行の入力が自動インデントの対象か（改行コードの種類に応じて、入力された文字が
    /// 改行の最後の文字か）。
    pub fn is_newline_char(&self, ch: u32) -> bool {
        match ch {
            0x0A => true,
            // CR だけを改行にする文書では、CR が最後の文字。
            0x0D => self.send(SCI_GETEOLMODE, 0, 0) as usize == SC_EOL_CR,
            _ => false,
        }
    }

    /// カーソルの前後の括弧とその対応を強調する（対応がなければ赤系の強調）。
    pub fn update_brace_highlight(&self) {
        if !self.view.borrow().highlight_braces || self.length() > BRACE_MATCH_MAX_BYTES {
            self.clear_brace_highlight();
            return;
        }
        let caret = self.selection().caret;
        let candidates = [caret.checked_sub(1), Some(caret)];
        for pos in candidates.into_iter().flatten() {
            let b = self.send(SCI_GETCHARAT, pos as usize, 0) as u8;
            if !is_brace(b) {
                continue;
            }
            let other = self.send(SCI_BRACEMATCH, pos as usize, 0);
            if other >= 0 {
                self.send(SCI_BRACEHIGHLIGHT, pos as usize, other);
            } else {
                self.send(SCI_BRACEBADLIGHT, pos as usize, 0);
            }
            return;
        }
        self.clear_brace_highlight();
    }

    fn clear_brace_highlight(&self) {
        self.send(SCI_BRACEHIGHLIGHT, usize::MAX, -1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn right_margin_leaves_exactly_the_columns() {
        // 幅 1000、左 50、80 桁 = 640px → 本文の領域は 640px + 1px の余裕。
        let m = right_margin_for_column(1000, 50, 640);
        assert_eq!(m, 309);
        assert_eq!(1000 - 50 - m, 641);
        // ウィンドウが桁幅より狭ければ余白なし（ウィンドウ幅で折り返す）。
        assert_eq!(right_margin_for_column(500, 50, 640), 0);
    }

    #[test]
    fn leading_indent_takes_spaces_and_tabs_only() {
        assert_eq!(leading_indent(b"  \tfoo"), b"  \t");
        assert_eq!(leading_indent(b"foo"), b"");
        assert_eq!(leading_indent(b"   "), b"   ");
        assert_eq!(leading_indent("\u{3000}x".as_bytes()), b"");
    }

    #[test]
    fn braces() {
        assert!(is_brace(b'(') && is_brace(b'}') && !is_brace(b'<') && !is_brace(b'a'));
    }
}
