//! 巨大ファイルのビューの純粋な計算（スクロール、桁の割り当て、選択、行番号の入力）。
//!
//! 標準ライブラリだけに依存する（Linux でもテストできるよう、`tests/bigview_logic.rs` が
//! このファイルを直接取り込む）。位置はすべてファイル先頭からのバイト数。

/// スクロールバーの範囲（0..=SCROLL_RANGE）。ファイルのバイト位置を線形に写す。
pub const SCROLL_RANGE: i32 = 100_000;
/// タブの桁幅。
pub const TAB_COLS: u32 = 4;
/// 行ジャンプで受け付ける行番号の最大桁数。
pub const MAX_GOTO_DIGITS: usize = 18;

/// 範囲を (小, 大) に整える。
pub fn normalize(a: u64, b: u64) -> (u64, u64) {
    if a <= b {
        (a, b)
    } else {
        (b, a)
    }
}

/// バイト位置からスクロールバーの位置へ。
pub fn scroll_pos(top: u64, len: u64) -> i32 {
    if len == 0 {
        return 0;
    }
    ((top.min(len) as u128 * SCROLL_RANGE as u128) / len as u128) as i32
}

/// スクロールバーの位置からバイト位置へ（行頭への丸めは呼び出し側）。
pub fn offset_from_scroll(pos: i32, len: u64) -> u64 {
    let pos = pos.clamp(0, SCROLL_RANGE) as u128;
    ((pos * len as u128) / SCROLL_RANGE as u128) as u64
}

/// つまみの大きさ（表示中のバイト数に比例。最低 1）。
pub fn scroll_page(visible_bytes: u64, len: u64) -> u32 {
    if len == 0 {
        return SCROLL_RANGE as u32 + 1;
    }
    let page = (visible_bytes.min(len) as u128 * SCROLL_RANGE as u128) / len as u128;
    (page as u32).clamp(1, SCROLL_RANGE as u32)
}

/// 文字の桁幅（全角・絵文字は 2、結合文字は 0）。タブは別扱い。
pub fn char_cols(c: char) -> u32 {
    let u = c as u32;
    match u {
        0x00..=0x1f | 0x7f => 1, // 制御文字は 1 桁の記号として描く
        0x300..=0x36f | 0x200b..=0x200f | 0xfe00..=0xfe0f => 0,
        0x1100..=0x115f
        | 0x2e80..=0x303e
        | 0x3041..=0x33ff
        | 0x3400..=0x4dbf
        | 0x4e00..=0x9fff
        | 0xa000..=0xa4cf
        | 0xac00..=0xd7a3
        | 0xf900..=0xfaff
        | 0xfe30..=0xfe6f
        | 0xff00..=0xff60
        | 0xffe0..=0xffe6
        | 0x1f300..=0x1f64f
        | 0x1f900..=0x1f9ff
        | 0x20000..=0x3fffd => 2,
        _ => 1,
    }
}

/// 行内の文字 1 つの位置。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cell {
    pub ch: char,
    /// 行頭からのバイト数。
    pub byte: usize,
    /// 先頭の桁。
    pub col: u32,
    /// 桁幅（タブは次のタブ位置まで）。
    pub cols: u32,
}

/// 行の各文字の桁の割り当て。
pub fn cells(text: &str) -> Vec<Cell> {
    let mut out = Vec::with_capacity(text.len());
    let mut col = 0u32;
    for (byte, ch) in text.char_indices() {
        let cols = if ch == '\t' {
            TAB_COLS - col % TAB_COLS
        } else {
            char_cols(ch)
        };
        out.push(Cell {
            ch,
            byte,
            col,
            cols,
        });
        col += cols;
    }
    out
}

/// 行の総桁数。
pub fn total_cols(text: &str) -> u32 {
    cells(text).last().map_or(0, |c| c.col + c.cols)
}

/// 行頭から `byte` までの桁数（`byte` が文字の境界でなければ次の境界まで）。
pub fn col_of_byte(text: &str, byte: usize) -> u32 {
    let mut col = 0;
    for c in cells(text) {
        if c.byte >= byte {
            return c.col;
        }
        col = c.col + c.cols;
    }
    col
}

/// 桁 `col`（小数。文字の左端が整数）に最も近い文字の境界のバイト位置。
/// 文字の右半分なら次の文字の手前。行末より右なら行の長さ。
pub fn byte_at_col(text: &str, col: f32) -> usize {
    if col <= 0.0 {
        return 0;
    }
    for c in cells(text) {
        let mid = c.col as f32 + c.cols as f32 / 2.0;
        if col < mid {
            return c.byte;
        }
    }
    text.len()
}

/// 描く部分。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Visible {
    /// タブを空白に、制御文字を `·` に置き換えた表示用の文字列。
    pub text: String,
    /// `text` の先頭の桁（`first_col` 以下でありうる）。
    pub col0: u32,
}

/// `first_col` から `cols` 桁ぶんの表示用の文字列。
pub fn visible_part(text: &str, first_col: u32, cols: u32) -> Visible {
    let end_col = first_col.saturating_add(cols);
    let mut out = String::new();
    let mut col0 = None;
    for c in cells(text) {
        if c.col + c.cols <= first_col && c.cols > 0 {
            continue;
        }
        if c.col >= end_col {
            break;
        }
        col0.get_or_insert(c.col);
        match c.ch {
            '\t' => out.extend(std::iter::repeat_n(' ', c.cols as usize)),
            ch if (ch as u32) < 0x20 || ch as u32 == 0x7f => out.push('\u{b7}'),
            ch => out.push(ch),
        }
    }
    Visible {
        text: out,
        col0: col0.unwrap_or(first_col),
    }
}

/// 行 `[row_start, row_end)` と選択 `[lo, hi)` の共通部分（行頭からのバイト数）。
/// 選択が行末より後ろ（改行）まで続くなら 3 つ目が `true`。
pub fn selection_in_row(
    row_start: u64,
    row_end: u64,
    lo: u64,
    hi: u64,
) -> Option<(usize, usize, bool)> {
    if lo >= hi || hi <= row_start || lo > row_end {
        return None;
    }
    let a = lo.max(row_start) - row_start;
    let b = hi.min(row_end) - row_start;
    Some((a as usize, b as usize, hi > row_end))
}

/// コピーする範囲。`max` を超えるなら `None`。
pub fn copy_range(a: u64, b: u64, max: u64) -> Option<(u64, u64)> {
    let (lo, hi) = normalize(a, b);
    (hi - lo <= max).then_some((lo, hi))
}

/// 行番号の表示桁数（最低 4）。
pub fn gutter_digits(max_line: u64) -> usize {
    max_line.max(1).to_string().len().max(4)
}

/// 「行ジャンプ」の入力を行番号（1 始まり）にする。空白と桁区切りの `,` は無視し、
/// 全角数字も受け付ける。0、数字以外、桁あふれは `None`。
pub fn parse_line_number(input: &str) -> Option<u64> {
    let digits: String = input
        .chars()
        .filter(|c| !c.is_whitespace() && *c != ',' && *c != '\u{ff0c}')
        .map(|c| match c {
            '\u{ff10}'..='\u{ff19}' => char::from_u32(c as u32 - 0xff10 + '0' as u32).unwrap_or(c),
            _ => c,
        })
        .collect();
    if digits.is_empty()
        || digits.len() > MAX_GOTO_DIGITS
        || !digits.chars().all(|c| c.is_ascii_digit())
    {
        return None;
    }
    digits.parse::<u64>().ok().filter(|n| *n >= 1)
}

/// 横スクロールの最大値（桁）。見えた最長の行に合わせて伸ばす。
pub fn hscroll_max(max_cols_seen: u32, view_cols: u32) -> u32 {
    max_cols_seen.saturating_sub(view_cols.saturating_sub(1).max(1))
}

/// 見える行数（部分的に見える最後の行を含めない）。最低 1。
pub fn rows_that_fit(height_px: i32, line_h: i32) -> usize {
    if line_h <= 0 {
        return 1;
    }
    (height_px / line_h).max(1) as usize
}
