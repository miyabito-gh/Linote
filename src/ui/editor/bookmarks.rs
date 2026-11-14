//! 文書に属する Scintilla マーカー（タブ切替でも残り、保存はしない）。
use super::sci::*;
use super::Editor;
use crate::document::Selection;
use crate::ui::theme::{Theme, Token};

const BOOKMARK: usize = 0;
const BOOKMARK_MASK: isize = 1 << BOOKMARK;

impl Editor {
    pub(super) fn configure_bookmarks(&self) {
        self.send(SCI_MARKERDEFINE, BOOKMARK, SC_MARK_CIRCLE);
        // 行番号とは独立した小さな印の余白。行番号を消しても残す。
        self.send(SCI_SETMARGINTYPEN, 1, SC_MARGIN_SYMBOL);
        self.send(SCI_SETMARGINMASKN, 1, BOOKMARK_MASK);
        self.send(SCI_SETMARGINWIDTHN, 1, 12);
    }

    pub(super) fn apply_bookmark_theme(&self, theme: &Theme) {
        let color = theme.token(Token::Keyword).fore.colorref() as isize;
        self.send(SCI_MARKERSETFORE, BOOKMARK, color);
        self.send(SCI_MARKERSETBACK, BOOKMARK, color);
    }

    pub fn has_bookmark(&self, line: u64) -> bool {
        self.send(SCI_MARKERGET, line as usize, 0) & BOOKMARK_MASK != 0
    }

    /// 現在の行の印を切り替える。変更後に印があれば true。
    pub fn toggle_bookmark(&self) -> bool {
        let line = self.line_from_position(self.selection().caret) as usize;
        let marked = self.has_bookmark(line as u64);
        self.send(
            if marked {
                SCI_MARKERDELETE
            } else {
                SCI_MARKERADD
            },
            line,
            BOOKMARK as isize,
        );
        !marked
    }

    pub fn clear_bookmarks(&self) {
        self.send(SCI_MARKERDELETEALL, BOOKMARK, 0);
    }

    /// 現在行の次（前）の印へ移動する。端で折り返し、印がなければ false。
    pub fn goto_bookmark(&self, forward: bool) -> bool {
        let current = self.line_from_position(self.selection().caret);
        let line = if forward {
            let next = self.send(SCI_MARKERNEXT, (current + 1) as usize, BOOKMARK_MASK);
            if next >= 0 {
                next
            } else {
                self.send(SCI_MARKERNEXT, 0, BOOKMARK_MASK)
            }
        } else {
            let prev = if current > 0 {
                self.send(SCI_MARKERPREVIOUS, (current - 1) as usize, BOOKMARK_MASK)
            } else {
                -1
            };
            if prev >= 0 {
                prev
            } else {
                self.send(
                    SCI_MARKERPREVIOUS,
                    self.line_count().saturating_sub(1) as usize,
                    BOOKMARK_MASK,
                )
            }
        };
        if line < 0 {
            return false;
        }
        self.send(SCI_ENSUREVISIBLE, line as usize, 0);
        self.set_selection(Selection::caret_at(
            self.line_start(line as u64).unwrap_or(0),
        ));
        self.scroll_caret();
        true
    }
}
