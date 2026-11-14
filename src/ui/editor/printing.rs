//! 印刷のための Scintilla の操作（`SCI_FORMATRANGEFULL`、印刷の色・折り返し・行番号）。
//!
//! 印刷中は編集欄の設定を一時的に変える。[`PrintGuard`] が元に戻す（Drop）。
//! 描画先の DC（プリンターでもメモリ DC でもよい）と配置は呼び出し側が決める。

use windows::Win32::Graphics::Gdi::HDC;

use super::sci::*;
use super::Editor;
use crate::printing::{PageLayout, Rect};

/// 印刷の選択肢（設定のうち編集欄に関わるもの）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PrintParams {
    /// 色分けして印刷する（偽なら白黒）。
    pub color: bool,
    pub line_numbers: bool,
}

/// 印刷中の状態。作っている間、編集欄は印刷用の設定になる。
pub struct PrintGuard<'a> {
    editor: &'a Editor,
    color_mode: isize,
    wrap_mode: isize,
    margin_width: isize,
}

impl Editor {
    /// 印刷を始める（印刷の色、紙幅での折り返し、行番号の有無を設定する）。
    pub fn begin_print(&self, params: PrintParams) -> PrintGuard<'_> {
        let guard = PrintGuard {
            editor: self,
            color_mode: self.send(SCI_GETPRINTCOLOURMODE, 0, 0),
            wrap_mode: self.send(SCI_GETPRINTWRAPMODE, 0, 0),
            margin_width: self.send(SCI_GETMARGINWIDTHN, 0, 0),
        };
        let mode = if params.color {
            SC_PRINT_COLOURONWHITE
        } else {
            SC_PRINT_BLACKONWHITE
        };
        self.send(SCI_SETPRINTCOLOURMODE, mode, 0);
        // 折り返しは常に紙の幅で行う。拡大は画面の拡大率ではなく文字サイズどおり。
        self.send(SCI_SETPRINTWRAPMODE, SC_WRAP_WORD, 0);
        self.send(SCI_SETPRINTMAGNIFICATION, 0, 0);
        // 印刷では、幅が 0 でない行番号の余白だけが印刷される。
        if !params.line_numbers {
            self.send(SCI_SETMARGINWIDTHN, 0, 0);
        } else if guard.margin_width <= 0 {
            self.send(SCI_SETMARGINWIDTHN, 0, 1);
        }
        guard
    }
}

fn rect(r: &Rect) -> SciRectangle {
    SciRectangle {
        left: r.left,
        top: r.top,
        right: r.right,
        bottom: r.bottom,
    }
}

impl PrintGuard<'_> {
    /// `start..end` から 1 ページ分を組む。`draw` なら `hdc` に描く。次のページの先頭位置
    /// （範囲を使い切ったら `end`）を返す。
    pub fn format_page(
        &self,
        hdc: HDC,
        layout: &PageLayout,
        start: u64,
        end: u64,
        draw: bool,
    ) -> u64 {
        let mut range = RangeToFormatFull {
            hdc: hdc.0,
            hdc_target: hdc.0,
            rc: rect(&layout.body),
            rc_page: rect(&layout.page),
            cp_min: start as isize,
            cp_max: end as isize,
        };
        let next = self.editor.send(
            SCI_FORMATRANGEFULL,
            usize::from(draw),
            &mut range as *mut RangeToFormatFull as isize,
        );
        (next.max(0) as u64).min(end)
    }
}

impl Drop for PrintGuard<'_> {
    fn drop(&mut self) {
        // Scintilla が持っている印刷用のキャッシュを捨てる。
        self.editor.send(SCI_FORMATRANGEFULL, 0, 0);
        self.editor
            .send(SCI_SETPRINTCOLOURMODE, self.color_mode.max(0) as usize, 0);
        self.editor
            .send(SCI_SETPRINTWRAPMODE, self.wrap_mode.max(0) as usize, 0);
        self.editor.send(SCI_SETMARGINWIDTHN, 0, self.margin_width);
    }
}
