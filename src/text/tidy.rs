//! 保存時の整形（V5）の純粋な計算。行末の空白の除去と、最終行の改行の補い。
//!
//! 本文（UTF-8 のバイト列）を受け取り、置き換える範囲の一覧（[`TextEdit`]。位置は本文の
//! 先頭からのバイト位置、開始位置の昇順で重ならない）を返す。適用は後ろの編集から順に行う
//! （位置がずれない）。編集欄の「行末の空白を削除」（`ui::editor::transform`）も共用する。

/// 置き換え 1 件（`start..end` を `text` に置き換える）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextEdit {
    pub start: usize,
    pub end: usize,
    pub text: String,
}

fn is_eol(b: u8) -> bool {
    b == b'\n' || b == b'\r'
}

/// 行末（改行の直前、または本文の末尾）にある空白とタブを取り除く編集。
pub fn trailing_whitespace_edits(text: &[u8]) -> Vec<TextEdit> {
    let mut edits = Vec::new();
    let mut i = 0;
    while i < text.len() {
        if text[i] == b' ' || text[i] == b'\t' {
            let start = i;
            while i < text.len() && (text[i] == b' ' || text[i] == b'\t') {
                i += 1;
            }
            if i == text.len() || is_eol(text[i]) {
                edits.push(TextEdit {
                    start,
                    end: i,
                    text: String::new(),
                });
            }
        } else {
            i += 1;
        }
    }
    edits
}

/// 保存時の整形の指定。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TidyOptions<'a> {
    /// 行末の空白を除く。
    pub trim_trailing_whitespace: bool,
    /// 最終行の改行を補う。
    pub ensure_final_newline: bool,
    /// 補う改行（文書の改行形式。`"\r\n"` など）。
    pub eol: &'a str,
}

/// 保存時の整形の編集。変化がなければ空。
///
/// - 本文が空（行末の空白を除いた結果が空になる場合を含む）なら、改行は足さない。
/// - 最終行の改行は、本文の末尾に `opts.eol` を足す。
pub fn save_tidy_edits(text: &[u8], opts: &TidyOptions<'_>) -> Vec<TextEdit> {
    let mut edits = if opts.trim_trailing_whitespace {
        trailing_whitespace_edits(text)
    } else {
        Vec::new()
    };
    if opts.ensure_final_newline {
        // 整形後の本文の最後のバイト。
        let mut end = text.len();
        if opts.trim_trailing_whitespace {
            while end > 0 && (text[end - 1] == b' ' || text[end - 1] == b'\t') {
                end -= 1;
            }
        }
        if end > 0 && !is_eol(text[end - 1]) {
            edits.push(TextEdit {
                start: text.len(),
                end: text.len(),
                text: opts.eol.to_owned(),
            });
        }
    }
    edits
}

/// 編集を適用した本文（試験と、編集欄を介さない用途のため）。
pub fn apply(text: &str, edits: &[TextEdit]) -> String {
    let mut bytes = text.as_bytes().to_vec();
    for e in edits.iter().rev() {
        bytes.splice(e.start..e.end, e.text.bytes());
    }
    String::from_utf8(bytes).expect("編集は文字境界で行う")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tidy(text: &str, trim: bool, nl: bool, eol: &str) -> String {
        let opts = TidyOptions {
            trim_trailing_whitespace: trim,
            ensure_final_newline: nl,
            eol,
        };
        apply(text, &save_tidy_edits(text.as_bytes(), &opts))
    }

    #[test]
    fn trims_in_every_eol_style() {
        assert_eq!(tidy("a  \r\nb\t\r\n", true, false, "\r\n"), "a\r\nb\r\n");
        assert_eq!(tidy("a  \nb \n c ", true, false, "\n"), "a\nb\n c");
        assert_eq!(tidy("a  \rb\t\r", true, false, "\r"), "a\rb\r");
        // 行の途中の空白と全角空白は残す。
        assert_eq!(tidy("a b\u{3000}\n", true, false, "\n"), "a b\u{3000}\n");
    }

    #[test]
    fn final_newline_follows_the_document_eol() {
        assert_eq!(tidy("a", false, true, "\r\n"), "a\r\n");
        assert_eq!(tidy("a", false, true, "\n"), "a\n");
        assert_eq!(tidy("a", false, true, "\r"), "a\r");
        assert_eq!(tidy("a\n", false, true, "\r\n"), "a\n");
        assert_eq!(tidy("a\r\n", false, true, "\n"), "a\r\n");
    }

    #[test]
    fn empty_body_gets_no_newline() {
        assert_eq!(tidy("", true, true, "\n"), "");
        assert_eq!(tidy("   ", true, true, "\n"), "");
        assert!(save_tidy_edits(
            b"",
            &TidyOptions {
                trim_trailing_whitespace: true,
                ensure_final_newline: true,
                eol: "\n"
            }
        )
        .is_empty());
    }

    #[test]
    fn whitespace_only_last_line() {
        // 空白だけの最終行は、除去すれば空の行になり、直前の改行で終わる。
        assert_eq!(tidy("a\n   ", true, true, "\n"), "a\n");
        // 除去しないなら、空白で終わるので改行を足す。
        assert_eq!(tidy("a\n   ", false, true, "\n"), "a\n   \n");
        assert_eq!(tidy("a  ", true, true, "\n"), "a\n");
    }

    #[test]
    fn no_change_gives_no_edits() {
        let opts = TidyOptions {
            trim_trailing_whitespace: true,
            ensure_final_newline: true,
            eol: "\r\n",
        };
        assert!(save_tidy_edits(b"a\r\nb\r\n", &opts).is_empty());
        let off = TidyOptions {
            trim_trailing_whitespace: false,
            ensure_final_newline: false,
            eol: "\n",
        };
        assert!(save_tidy_edits(b"a  \nb", &off).is_empty());
    }
}
