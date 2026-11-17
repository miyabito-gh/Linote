//! メモ一覧に出す、本文の冒頭の文（GUI 非依存の純粋な関数）。
//!
//! 名前（[`super::derive_name`] が本文の最初の一文から作ることが多い）と重なる部分は飛ばして、
//! その続きを出す。一覧の 2 行目（右の本文の欄を出せない狭い窓）で使う。
//!
//! 規則:
//! 1. 行ごとに、行頭の Markdown の記号を外す（[`super::name::strip_markers`] と同じ）。
//! 2. 記号だけの行・空行・コードフェンスは飛ばす。
//! 3. 名前と同じ行は飛ばす。名前で始まる行は、名前とその直後の文の終わりを除いた続きを使う。
//! 4. 残った行を空白でつないで、[`MAX_LEAD_CHARS`] 文字で切る（切ったら「…」）。

use super::name::{strip_markers, tidy};

/// 冒頭の文の最大の文字数（「…」は含まない）。
pub const MAX_LEAD_CHARS: usize = 80;

/// 本文 `body` から、名前 `name` と重ならない冒頭の文を作る。なければ空。
pub fn lead_text(name: &str, body: &str) -> String {
    let name = tidy(name);
    let mut out = String::new();
    let mut count = 0usize;
    for line in body.lines() {
        let cleaned = strip_markers(line);
        if !cleaned.chars().any(char::is_alphanumeric) {
            continue;
        }
        let text = tidy(&cleaned);
        let text = match text.strip_prefix(name.as_str()) {
            // 名前そのものの行は飛ばす。名前で始まる行は、その続きだけを使う。
            Some(rest) if !name.is_empty() => rest
                .trim_start_matches(['。', '．', '.', '、', ',', ' '])
                .trim()
                .to_owned(),
            _ => text,
        };
        if text.is_empty() {
            continue;
        }
        if !out.is_empty() {
            out.push(' ');
            count += 1;
        }
        for c in text.chars() {
            if count >= MAX_LEAD_CHARS {
                out.push('…');
                return out;
            }
            out.push(c);
            count += 1;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skips_the_line_that_is_the_name() {
        assert_eq!(
            lead_text("買い物リスト", "# 買い物リスト\n牛乳と卵、パン。"),
            "牛乳と卵、パン。"
        );
    }

    #[test]
    fn uses_the_rest_of_a_line_that_starts_with_the_name() {
        assert_eq!(
            lead_text("牛乳と卵", "牛乳と卵。週末に買う\n次の行"),
            "週末に買う 次の行"
        );
    }

    #[test]
    fn markers_blank_and_symbol_lines_are_dropped() {
        assert_eq!(
            lead_text("題", "---\n\n- 項目A\n- [x] 項目B\n```\n1. 番号"),
            "項目A 項目B 番号"
        );
    }

    #[test]
    fn empty_when_nothing_remains() {
        assert_eq!(lead_text("名前", ""), "");
        assert_eq!(lead_text("名前", "名前"), "");
        assert_eq!(lead_text("", "  \n---\n"), "");
    }

    #[test]
    fn long_text_is_cut_with_an_ellipsis() {
        let body = "あ".repeat(200);
        let lead = lead_text("x", &body);
        assert_eq!(lead.chars().count(), MAX_LEAD_CHARS + 1);
        assert!(lead.ends_with('…'));
        let exact = "い".repeat(MAX_LEAD_CHARS);
        assert_eq!(lead_text("x", &exact), exact);
    }

    #[test]
    fn control_characters_become_spaces() {
        assert_eq!(lead_text("x", "a\u{0007}b\tc"), "a b c");
    }
}
