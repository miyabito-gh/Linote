//! 編集コマンド（行末の空白の削除、タブ⇔スペース、日時の挿入）の純粋な計算。
//!
//! 本文（UTF-8 のバイト列）を受け取り、置き換える範囲の一覧（[`Edit`]。バイト位置は
//! 渡した本文の先頭から）を返す。適用は後ろの編集から順に行うこと（位置がずれない）。
//! 桁は文字数で数える（全角文字も 1 桁。Scintilla の表示上のタブ位置とは、
//! 全角文字を含む行で異なることがある）。

/// 置き換え 1 件（`start..end` を `text` に置き換える）。
pub use crate::text::tidy::TextEdit as Edit;

fn is_eol(b: u8) -> bool {
    b == b'\n' || b == b'\r'
}

/// 行末（改行の直前、または本文の末尾）にある空白とタブを取り除く編集（保存時の整形と共用）。
pub use crate::text::tidy::trailing_whitespace_edits;

/// タブ文字をタブ位置（`width` 桁ごと）に合わせたスペースにする編集。
pub fn tabs_to_spaces_edits(text: &[u8], width: usize) -> Vec<Edit> {
    let width = width.max(1);
    let mut edits = Vec::new();
    let mut col = 0usize;
    for (i, &b) in text.iter().enumerate() {
        match b {
            b'\t' => {
                let n = width - col % width;
                edits.push(Edit {
                    start: i,
                    end: i + 1,
                    text: " ".repeat(n),
                });
                col += n;
            }
            b if is_eol(b) => col = 0,
            // UTF-8 の続きのバイトは桁に数えない。
            b if b & 0xC0 == 0x80 => {}
            _ => col += 1,
        }
    }
    edits
}

/// 行頭のインデント（スペースとタブ）を、タブ中心の表記（`width` 桁ぶんをタブ 1 つ、
/// 余りをスペース）にそろえる編集。行の途中の空白は変えない。
pub fn leading_spaces_to_tabs_edits(text: &[u8], width: usize) -> Vec<Edit> {
    let width = width.max(1);
    let mut edits = Vec::new();
    let mut i = 0;
    while i < text.len() {
        // 行頭。
        let start = i;
        let mut col = 0usize;
        while i < text.len() && (text[i] == b' ' || text[i] == b'\t') {
            col += if text[i] == b'\t' {
                width - col % width
            } else {
                1
            };
            i += 1;
        }
        if i > start {
            let new = format!("{}{}", "\t".repeat(col / width), " ".repeat(col % width));
            if new.as_bytes() != &text[start..i] {
                edits.push(Edit {
                    start,
                    end: i,
                    text: new,
                });
            }
        }
        // 次の行頭まで。CRLF は 1 つの改行として読み飛ばす。
        while i < text.len() && !is_eol(text[i]) {
            i += 1;
        }
        if i < text.len() {
            if text[i] == b'\r' && text.get(i + 1) == Some(&b'\n') {
                i += 1;
            }
            i += 1;
        }
    }
    edits
}

/// `edits` を適用した結果（試験用。実際の適用は編集欄に後ろから行う）。
pub fn apply(text: &str, edits: &[Edit]) -> String {
    let mut out = text.as_bytes().to_vec();
    for e in edits.iter().rev() {
        out.splice(e.start..e.end, e.text.bytes());
    }
    String::from_utf8(out).unwrap_or_default()
}

/// 挿入する日時の文字列（`2026-11-14 09:05`）。
pub fn format_datetime(year: u16, month: u16, day: u16, hour: u16, minute: u16) -> String {
    format!("{year:04}-{month:02}-{day:02} {hour:02}:{minute:02}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trims_trailing_whitespace_only_at_line_ends() {
        let t = "a  \nb c \t\r\n  d\n e \n \nf  ";
        let r = apply(t, &trailing_whitespace_edits(t.as_bytes()));
        assert_eq!(r, "a\nb c\r\n  d\n e\n\nf");
    }

    #[test]
    fn tabs_become_spaces_by_column() {
        let t = "\ta\n  \tb\nあ\tc";
        let r = apply(t, &tabs_to_spaces_edits(t.as_bytes(), 4));
        assert_eq!(r, "    a\n    b\nあ   c");
    }

    #[test]
    fn leading_indent_becomes_tabs() {
        let t = "        x\n      y\n  \t z\nq        r\r\n    w";
        let r = apply(t, &leading_spaces_to_tabs_edits(t.as_bytes(), 4));
        assert_eq!(r, "\t\tx\n\t  y\n\t z\nq        r\r\n\tw");
    }

    #[test]
    fn formats_date_time() {
        assert_eq!(format_datetime(2026, 1, 2, 3, 4), "2026-01-02 03:04");
    }
}
