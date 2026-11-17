//! 本文の最初の一文からメモの名前を作る（GUI 非依存の純粋な関数）。
//!
//! 名前を決めていない文書（無題など）をメモとして保存するときに使う。
//!
//! 規則:
//! 1. 最初の「文字のある」行を使う。空行、コードフェンス（```）、記号だけの行（`---` など）は飛ばす。
//! 2. 行頭の Markdown の記号を外す（見出し `#`、引用 `>`、箇条書き `-` `*` `+`、
//!    チェックリスト `[ ]` `[x]`、番号 `1.` `1)`）。
//! 3. 最初の文の終わり（`。` `．` `.`（後ろが空白か行末のとき））までを取る。終わりの記号は名前に含めない。
//! 4. 制御文字は空白にし、連続する空白は 1 つにまとめ、前後を除く。
//! 5. [`MAX_NAME_CHARS`] 文字を超えたら切って「…」を付ける（結合文字や絵文字の途中では切らない）。
//! 6. 何も残らなければ [`DEFAULT_NAME`]。

/// 名前の最大の文字数（「…」は含まない）。
pub const MAX_NAME_CHARS: usize = 40;

/// 本文から名前が作れないときの名前。
pub const DEFAULT_NAME: &str = "無題のメモ";

/// 本文の最初の一文から、メモの名前を作る。
pub fn derive_name(body: &str) -> String {
    for line in body.lines() {
        let content = strip_markers(line);
        if !content.chars().any(char::is_alphanumeric) {
            // 空行、コードフェンス、記号だけの行。
            continue;
        }
        let sentence = first_sentence(&content);
        let name = tidy(sentence);
        if !name.is_empty() {
            return truncate(&name);
        }
    }
    DEFAULT_NAME.to_owned()
}

/// 行頭の Markdown の記号（見出し・引用・箇条書き・チェックリスト・番号）を外す。
pub(super) fn strip_markers(line: &str) -> String {
    let mut s = line.trim();
    if s.starts_with("```") || s.starts_with("~~~") {
        return String::new();
    }
    loop {
        let before = s;
        s = s.trim_start();
        // 見出し・引用。
        if let Some(rest) = s.strip_prefix('#').or_else(|| s.strip_prefix('>')) {
            s = rest;
            continue;
        }
        // 箇条書き（記号の後ろに空白があるときだけ。`-5` や `*強調*` は残す）。
        if let Some(rest) = ['-', '*', '+', '・']
            .iter()
            .find_map(|m| s.strip_prefix(*m))
            .filter(|rest| rest.starts_with(char::is_whitespace))
        {
            s = rest;
            continue;
        }
        // チェックリスト。
        if let Some(rest) = ["[ ]", "[x]", "[X]"]
            .iter()
            .find_map(|m| s.strip_prefix(*m))
            .filter(|rest| rest.starts_with(char::is_whitespace))
        {
            s = rest;
            continue;
        }
        // 番号（`1.` `1)` の後ろに空白）。
        let digits = s.chars().take_while(char::is_ascii_digit).count();
        if (1..=9).contains(&digits) {
            let rest = &s[digits..];
            if let Some(rest) = rest.strip_prefix('.').or_else(|| rest.strip_prefix(')')) {
                if rest.starts_with(char::is_whitespace) {
                    s = rest;
                    continue;
                }
            }
        }
        if s == before {
            break;
        }
    }
    s.trim().to_owned()
}

/// 最初の文（終わりの記号の手前まで）。`.` は後ろが空白か行末のときだけ終わりとみなす
/// （`3.14` や `a.md` では切らない）。
fn first_sentence(s: &str) -> &str {
    let mut chars = s.char_indices().peekable();
    while let Some((i, c)) = chars.next() {
        let ends = match c {
            '。' | '．' => true,
            '.' => chars.peek().is_none_or(|&(_, n)| n.is_whitespace()),
            _ => false,
        };
        if ends {
            return &s[..i];
        }
    }
    s
}

/// 制御文字を空白にして、連続する空白を 1 つにまとめる。
pub(super) fn tidy(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut space = true;
    for c in s.chars() {
        if c.is_control() || c.is_whitespace() {
            if !space {
                out.push(' ');
                space = true;
            }
        } else {
            out.push(c);
            space = false;
        }
    }
    out.trim_end().to_owned()
}

/// 前の文字とつながる文字（結合文字、濁点、異体字選択子、ゼロ幅接合子）か。
fn joins_previous(c: char) -> bool {
    matches!(c,
        '\u{0300}'..='\u{036F}'
        | '\u{3099}' | '\u{309A}'
        | '\u{200D}'
        | '\u{FE00}'..='\u{FE0F}'
        | '\u{1F3FB}'..='\u{1F3FF}')
}

fn truncate(name: &str) -> String {
    let chars: Vec<char> = name.chars().collect();
    if chars.len() <= MAX_NAME_CHARS {
        return name.to_owned();
    }
    let mut end = MAX_NAME_CHARS;
    // 切る位置の直後が前の文字につながるなら、その前の文字まで戻す。
    while end > 0 && joins_previous(chars[end]) {
        end -= 1;
    }
    // ゼロ幅接合子で続く絵文字の並びを、途中で切らない。
    if end > 0 && chars[end - 1] == '\u{200D}' {
        end -= 1;
    }
    let mut out: String = chars[..end].iter().collect();
    out = out.trim_end().to_owned();
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_sentence_of_plain_text() {
        assert_eq!(derive_name("今日の予定。明日の予定。"), "今日の予定");
        assert_eq!(derive_name("Buy milk. Then eggs."), "Buy milk");
        assert_eq!(derive_name("一行だけ"), "一行だけ");
    }

    #[test]
    fn dot_inside_a_word_does_not_end_the_sentence() {
        assert_eq!(
            derive_name("version 3.14 released"),
            "version 3.14 released"
        );
        assert_eq!(derive_name("open a.md now"), "open a.md now");
        assert_eq!(derive_name("end."), "end");
    }

    #[test]
    fn markdown_markers_are_stripped() {
        assert_eq!(derive_name("# 買い物リスト\n牛乳"), "買い物リスト");
        assert_eq!(derive_name("### 見出し3"), "見出し3");
        assert_eq!(derive_name("> 引用です"), "引用です");
        assert_eq!(derive_name("- 項目A\n- 項目B"), "項目A");
        assert_eq!(derive_name("* [ ] やること"), "やること");
        assert_eq!(derive_name("- [x] 済み"), "済み");
        assert_eq!(derive_name("1. 最初\n2. 次"), "最初");
        assert_eq!(derive_name("2) 括弧の番号"), "括弧の番号");
        // 記号の後ろに空白がなければ記号ではない。
        assert_eq!(derive_name("-5度の朝"), "-5度の朝");
        assert_eq!(derive_name("*強調*の語"), "*強調*の語");
        assert_eq!(derive_name("2026年の目標"), "2026年の目標");
    }

    #[test]
    fn blank_fence_and_symbol_lines_are_skipped() {
        assert_eq!(derive_name("\n\n  \n本題"), "本題");
        assert_eq!(derive_name("---\n本題\n---"), "本題");
        assert_eq!(derive_name("```rust\nfn main() {}\n```"), "fn main() {}");
        assert_eq!(derive_name("====\n***\n結論"), "結論");
    }

    #[test]
    fn empty_input_gets_the_default_name() {
        assert_eq!(derive_name(""), DEFAULT_NAME);
        assert_eq!(derive_name("  \n\t\n"), DEFAULT_NAME);
        assert_eq!(derive_name("---\n***\n"), DEFAULT_NAME);
        assert_eq!(derive_name("。"), DEFAULT_NAME);
    }

    #[test]
    fn control_characters_and_spaces_are_normalized() {
        assert_eq!(derive_name("a\u{0007}b\t\tc   d"), "a b c d");
        assert_eq!(derive_name("　全角　空白　"), "全角 空白");
    }

    #[test]
    fn long_names_are_cut_at_forty_characters() {
        let body = "あ".repeat(60);
        let n = derive_name(&body);
        assert_eq!(n.chars().count(), MAX_NAME_CHARS + 1);
        assert!(n.ends_with('…'));
        let exact = "い".repeat(MAX_NAME_CHARS);
        assert_eq!(derive_name(&exact), exact);
    }

    #[test]
    fn cut_does_not_split_combining_marks_or_emoji() {
        // 40 文字目の直後が濁点の結合文字。
        let mut s = "か".repeat(39);
        s.push('は');
        s.push('\u{3099}');
        s.push_str("あいう");
        let n = derive_name(&s);
        assert!(!n.contains('\u{3099}') || n.chars().filter(|&c| c == '\u{3099}').count() == 1);
        assert!(n.ends_with('…'));
        // 家族の絵文字（ZWJ でつながる）を途中で切らない。
        let family = "👨\u{200D}👩\u{200D}👧";
        let mut e = "あ".repeat(38);
        e.push_str(family);
        e.push_str(&"い".repeat(10));
        let n = derive_name(&e);
        assert!(n.ends_with('…'));
        let body: String = n.trim_end_matches('…').to_owned();
        assert!(!body.ends_with('\u{200D}'), "{body:?}");
    }

    #[test]
    fn the_name_is_one_line() {
        let n = derive_name("# タイトル\n\n本文が続く。");
        assert!(!n.contains('\n'));
    }
}
