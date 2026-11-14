//! 改行形式の判定と統一。
//!
//! 本文（UTF-8）の改行は読み込み時に変換しない。ここでは数えるだけで、
//! 統一は利用者が明示的に求めたとき（[`normalize_eol`]）だけ行う。

use super::Eol;

/// 本文に含まれる改行の数（形式ごと）。`\r\n` は 1 個の CRLF として数える。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash)]
pub struct EolStats {
    pub crlf: u64,
    pub lf: u64,
    pub cr: u64,
}

impl EolStats {
    /// UTF-8 の本文を数える。CR・LF のバイトは多バイト文字の途中には現れないので、
    /// バイト単位で数えてよい。
    pub fn scan(text: &[u8]) -> Self {
        // 単純な数え上げ 3 回にして、自動ベクトル化に任せる（100 MiB でも数十ミリ秒）。
        let lf_all = text.iter().filter(|&&b| b == b'\n').count() as u64;
        let cr_all = text.iter().filter(|&&b| b == b'\r').count() as u64;
        let crlf = if cr_all == 0 || lf_all == 0 {
            0
        } else {
            text.iter()
                .zip(text.iter().skip(1))
                .filter(|&(&a, &b)| a == b'\r' && b == b'\n')
                .count() as u64
        };
        EolStats {
            crlf,
            lf: lf_all - crlf,
            cr: cr_all - crlf,
        }
    }

    /// 改行の総数。
    pub fn total(&self) -> u64 {
        self.crlf + self.lf + self.cr
    }

    /// 最も多い改行形式。改行が一つもなければ `None`。
    /// 同数のときは CRLF、LF、CR の順に優先する。
    pub fn dominant(&self) -> Option<Eol> {
        if self.total() == 0 {
            return None;
        }
        let mut best = (Eol::CrLf, self.crlf);
        for candidate in [(Eol::Lf, self.lf), (Eol::Cr, self.cr)] {
            if candidate.1 > best.1 {
                best = candidate;
            }
        }
        Some(best.0)
    }

    /// 2 種類以上の改行が混ざっているか。
    pub fn is_mixed(&self) -> bool {
        [self.crlf, self.lf, self.cr]
            .iter()
            .filter(|&&n| n > 0)
            .count()
            > 1
    }
}

/// 本文の改行をすべて `eol` に統一した文字列を返す（利用者が明示的に求めたとき用）。
pub fn normalize_eol(text: &str, eol: Eol) -> String {
    let target = eol.as_str();
    let bytes = text.as_bytes();
    let mut out = String::with_capacity(text.len() + text.len() / 16);
    let mut start = 0;
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if b == b'\n' || b == b'\r' {
            out.push_str(&text[start..i]);
            out.push_str(target);
            i += if b == b'\r' && bytes.get(i + 1) == Some(&b'\n') {
                2
            } else {
                1
            };
            start = i;
        } else {
            i += 1;
        }
    }
    out.push_str(&text[start..]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_each_kind() {
        let s = EolStats::scan(b"a\r\nb\nc\rd\r\n\r\r\n\n");
        assert_eq!(
            s,
            EolStats {
                crlf: 3,
                lf: 2,
                cr: 2
            }
        );
        assert!(s.is_mixed());
        assert_eq!(s.dominant(), Some(Eol::CrLf));
        assert_eq!(s.total(), 7);
    }

    #[test]
    fn edge_cases() {
        assert_eq!(EolStats::scan(b"").dominant(), None);
        assert_eq!(EolStats::scan(b"abc").dominant(), None);
        assert_eq!(EolStats::scan(b"\r").dominant(), Some(Eol::Cr));
        assert_eq!(EolStats::scan(b"\n").dominant(), Some(Eol::Lf));
        assert_eq!(EolStats::scan(b"\n\r").cr, 1);
        assert_eq!(EolStats::scan(b"\n\r").lf, 1);
        assert_eq!(EolStats::scan(b"\n\r").crlf, 0);
        assert!(!EolStats::scan(b"a\nb\n").is_mixed());
        // 同数なら CRLF > LF > CR。
        assert_eq!(EolStats::scan(b"\r\na\nb").dominant(), Some(Eol::CrLf));
        assert_eq!(EolStats::scan(b"\na\rb").dominant(), Some(Eol::Lf));
    }

    #[test]
    fn normalize() {
        let src = "a\r\nb\nc\rd\r\r\n";
        assert_eq!(normalize_eol(src, Eol::Lf), "a\nb\nc\nd\n\n");
        assert_eq!(normalize_eol(src, Eol::CrLf), "a\r\nb\r\nc\r\nd\r\n\r\n");
        assert_eq!(normalize_eol(src, Eol::Cr), "a\rb\rc\rd\r\r");
        assert_eq!(normalize_eol("日本語", Eol::Lf), "日本語");
        assert_eq!(normalize_eol("", Eol::Lf), "");
    }
}
