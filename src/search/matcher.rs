//! 文書内の検索と置換。[`TextSource`] を範囲ごとに読み、Rust の `regex` で検索する。
//!
//! - 本文は 1 MiB ほどの窓で読む。窓の終わりに触れた一致は、窓を進めて確かめ直す
//!   （窓をまたぐ一致、単語境界、`$` の誤判定を避ける）。
//! - UTF-8 として不正なバイトは検索の対象にしない（そこで文字列を区切る）。
//! - 長さ 0 の一致は無視する。
//! - 後ろ向きの検索は、窓の境界で 64 KiB を超える一致を見落とすことがある。

use std::ops::Range;

use regex::{Regex, RegexBuilder};

use crate::core::{Completion, Error, Outcome, Result, TaskContext};
use crate::document::{EditTarget, TextRange, TextSource};

const CHUNK: u64 = 1 << 20;
const MAX_WINDOW: u64 = 64 << 20;
const OVERLAP: u64 = 64 << 10;
const MAX_PATTERN_BYTES: usize = 4096;
const REGEX_SIZE_LIMIT: usize = 4 << 20;
/// 現在の選択を置換するときに読む最大の長さ。
const MAX_REPLACE_TARGET: u64 = 1 << 20;

/// 検索条件（検索欄の状態）。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SearchOptions {
    pub query: String,
    /// 正規表現として扱う（Rust `regex` の構文。後方参照・先読み後読みは使えない）。
    pub regex: bool,
    pub case_sensitive: bool,
    /// 単語単位（前後を `\b` で挟む）。
    pub whole_word: bool,
}

impl SearchOptions {
    pub fn literal(query: &str) -> Self {
        Self {
            query: query.to_owned(),
            ..Self::default()
        }
    }

    /// 検索器を作る。空・長すぎる・無効なパターンは [`Error::InvalidArgument`]
    /// （メッセージは検索欄にそのまま出せる）。
    pub fn compile(&self) -> Result<Searcher> {
        if self.query.is_empty() {
            return Err(Error::invalid_argument("pattern", "検索文字列が空です"));
        }
        if self.query.len() > MAX_PATTERN_BYTES {
            return Err(Error::invalid_argument(
                "pattern",
                format!("検索文字列が長すぎます（上限 {MAX_PATTERN_BYTES} バイト）"),
            ));
        }
        let mut source = if self.regex {
            self.query.clone()
        } else {
            regex::escape(&self.query)
        };
        if self.whole_word {
            source = format!(r"\b(?:{source})\b");
        }
        let regex = RegexBuilder::new(&source)
            .case_insensitive(!self.case_sensitive)
            .size_limit(REGEX_SIZE_LIMIT)
            .dfa_size_limit(REGEX_SIZE_LIMIT)
            .build()
            .map_err(|e| Error::invalid_argument("pattern", short_regex_error(&e)))?;
        Ok(Searcher {
            regex,
            regex_mode: self.regex,
        })
    }
}

/// `regex` のエラーは複数行になるので、要点の 1 行だけにする。
fn short_regex_error(e: &regex::Error) -> String {
    let text = e.to_string();
    text.lines()
        .rev()
        .find(|l| l.contains("error:"))
        .map(|l| l.trim().trim_start_matches("error:").trim().to_owned())
        .unwrap_or_else(|| text.lines().last().unwrap_or("").trim().to_owned())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    /// `from` 以降で始まる最初の一致。
    Forward,
    /// `from` 以前で終わる最後の一致。
    Backward,
}

/// 検索の一致。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Hit {
    pub range: TextRange,
    /// 文書の端を越えて反対側から見つけた。
    pub wrapped: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ReplaceSummary {
    pub replaced: u64,
}

/// コンパイル済みの検索条件。`Send + Sync`。
#[derive(Debug, Clone)]
pub struct Searcher {
    regex: Regex,
    regex_mode: bool,
}

/// UTF-8 として妥当な区間（窓の先頭からのバイト位置つき）に分ける。
/// 末尾の切れた文字は含めない。戻り値の `usize` は読み終えた位置。
fn valid_segments(bytes: &[u8]) -> (Vec<(usize, &str)>, usize) {
    let mut out = Vec::new();
    let mut pos = 0;
    while pos < bytes.len() {
        match std::str::from_utf8(&bytes[pos..]) {
            Ok(s) => {
                out.push((pos, s));
                return (out, bytes.len());
            }
            Err(e) => {
                let v = e.valid_up_to();
                if v > 0 {
                    if let Ok(s) = std::str::from_utf8(&bytes[pos..pos + v]) {
                        out.push((pos, s));
                    }
                }
                match e.error_len() {
                    Some(n) => pos += v + n,
                    None => return (out, pos + v),
                }
            }
        }
    }
    (out, pos)
}

fn read_window(src: &dyn TextSource, pos: u64, len: u64) -> Result<Vec<u8>> {
    let want = len.min(src.len().saturating_sub(pos)) as usize;
    let mut buf = vec![0u8; want];
    let mut got = 0;
    while got < want {
        let n = src.read_at(pos + got as u64, &mut buf[got..])?;
        if n == 0 {
            break;
        }
        got += n;
    }
    buf.truncate(got);
    Ok(buf)
}

/// 先頭の継続バイトの数（文字の途中から始まった窓の補正）。
fn lead_skip(bytes: &[u8]) -> usize {
    bytes
        .iter()
        .take(3)
        .take_while(|b| (**b & 0xC0) == 0x80)
        .count()
}

impl Searcher {
    /// 前向きに走査し、一致ごとに `visit(範囲, キャプチャ)` を呼ぶ（`false` で止める）。
    fn scan_forward(
        &self,
        src: &dyn TextSource,
        from: u64,
        ctx: &TaskContext,
        mut visit: impl FnMut(Range<u64>, &regex::Captures<'_>) -> bool,
    ) -> Result<Completion> {
        let len = src.len();
        let mut pos = from.min(len);
        let mut window = CHUNK;
        // これより前で始まる一致は報告済み（窓の重なりでの重複を避ける）。
        let mut min_start = pos;
        while pos < len {
            if ctx.cancel.is_cancelled() {
                return Ok(Completion::Cancelled);
            }
            let raw = read_window(src, pos, window)?;
            if raw.is_empty() {
                break;
            }
            let skip = if pos > 0 { lead_skip(&raw) } else { 0 };
            let at_eof = pos + raw.len() as u64 >= len;
            let (segments, consumed) = valid_segments(&raw[skip..]);
            let base = pos + skip as u64;
            let mut next = base + consumed as u64;
            if !at_eof {
                // 窓の終わりで切れた一致を次の窓で拾うため、少し重ねる。
                next = next.saturating_sub(OVERLAP).max(pos + 1);
            }
            let mut grow = false;
            'segments: for (i, (offset, text)) in segments.iter().enumerate() {
                let last = i + 1 == segments.len();
                let is_final = !last || at_eof || consumed < raw.len() - skip;
                for caps in self.regex.captures_iter(text) {
                    let m = caps.get(0).expect("group 0");
                    if m.is_empty() {
                        continue;
                    }
                    let start = base + (*offset + m.start()) as u64;
                    let end = base + (*offset + m.end()) as u64;
                    if start < min_start {
                        continue;
                    }
                    if !is_final && m.end() == text.len() && window < MAX_WINDOW {
                        // 窓の終わりに触れた一致は、窓を進めて確かめ直す。
                        if start > pos {
                            next = start;
                        } else {
                            grow = true;
                        }
                        break 'segments;
                    }
                    min_start = end;
                    if !visit(start..end, &caps) {
                        return Ok(Completion::Complete);
                    }
                }
            }
            if grow {
                window = (window * 2).min(MAX_WINDOW);
                continue;
            }
            window = CHUNK;
            if next <= pos {
                next = pos + raw.len() as u64;
            }
            pos = next;
            ctx.report(pos.saturating_sub(from), Some(len.saturating_sub(from)));
        }
        Ok(Completion::Complete)
    }

    /// `before` 以前で終わる最後の一致。
    fn scan_backward(
        &self,
        src: &dyn TextSource,
        before: u64,
        ctx: &TaskContext,
    ) -> Result<(Option<Range<u64>>, Completion)> {
        let mut end = before.min(src.len());
        loop {
            if ctx.cancel.is_cancelled() {
                return Ok((None, Completion::Cancelled));
            }
            let ws = end.saturating_sub(CHUNK);
            let raw = read_window(src, ws, end - ws)?;
            let skip = if ws > 0 { lead_skip(&raw) } else { 0 };
            let base = ws + skip as u64;
            let safe_from = if ws == 0 { 0 } else { ws + OVERLAP };
            let (segments, _) = valid_segments(&raw[skip.min(raw.len())..]);
            for (offset, text) in segments.iter().rev() {
                let mut best = None;
                for m in self.regex.find_iter(text) {
                    let start = base + (*offset + m.start()) as u64;
                    if !m.is_empty() && start >= safe_from {
                        best = Some(start..base + (*offset + m.end()) as u64);
                    }
                }
                if best.is_some() {
                    return Ok((best, Completion::Complete));
                }
            }
            if ws == 0 {
                return Ok((None, Completion::Complete));
            }
            end = ws + 2 * OVERLAP;
        }
    }

    /// `from` から `direction` に最初の一致を探す。`wrap` なら端で反対側へ回り込む。
    /// 中止されたら `None` と [`Completion::Cancelled`]。
    pub fn find(
        &self,
        src: &dyn TextSource,
        from: u64,
        direction: Direction,
        wrap: bool,
        ctx: &TaskContext,
    ) -> Result<Outcome<Option<Hit>>> {
        let wrap_from = match direction {
            Direction::Forward => 0,
            Direction::Backward => u64::MAX,
        };
        let mut passes = vec![(from, false)];
        if wrap {
            passes.push((wrap_from, true));
        }
        for (start, wrapped) in passes {
            let (found, completion) = match direction {
                Direction::Forward => {
                    let mut found = None;
                    let c = self.scan_forward(src, start, ctx, |r, _| {
                        found = Some(r);
                        false
                    })?;
                    (found, c)
                }
                Direction::Backward => self.scan_backward(src, start, ctx)?,
            };
            if completion == Completion::Cancelled {
                return Ok(Outcome::new(None, completion));
            }
            if let Some(r) = found {
                let range = TextRange::new(r.start, r.end);
                return Ok(Outcome::complete(Some(Hit { range, wrapped })));
            }
        }
        Ok(Outcome::complete(None))
    }

    /// 1 行の中の一致（長さ 0 を除く）のバイト範囲。Grep 用。
    pub(super) fn line_matches(&self, line: &str) -> Vec<(usize, usize)> {
        self.regex
            .find_iter(line)
            .filter(|m| !m.is_empty())
            .map(|m| (m.start(), m.end()))
            .collect()
    }

    /// 一致の数。`ctx.limits.max_items` に達したら [`Completion::ItemLimit`]。
    pub fn count(&self, src: &dyn TextSource, ctx: &TaskContext) -> Result<Outcome<u64>> {
        let cap = ctx.limits.max_items.unwrap_or(u64::MAX);
        let mut n = 0u64;
        let mut hit_cap = false;
        let completion = self.scan_forward(src, 0, ctx, |_, _| {
            n += 1;
            if n >= cap {
                hit_cap = true;
                return false;
            }
            true
        })?;
        let completion = if hit_cap {
            Completion::ItemLimit
        } else {
            completion
        };
        Ok(Outcome::new(n, completion))
    }

    /// 一致した文字列 `matched` を置き換えた結果。正規表現では `$1`、`${name}`、
    /// `$$` を展開する。`matched` 全体に一致しない（選択が一致でない）ときは `None`。
    pub fn expand(&self, matched: &str, replacement: &str) -> Option<String> {
        let caps = self.regex.captures(matched)?;
        if caps.get(0)?.range() != (0..matched.len()) {
            return None;
        }
        if !self.regex_mode {
            return Some(replacement.to_owned());
        }
        let mut out = String::new();
        caps.expand(replacement, &mut out);
        Some(out)
    }

    /// 文書の `range` が一致なら、置換後の文字列を返す（選択の置換で使う）。
    pub fn replacement_for(
        &self,
        src: &dyn TextSource,
        range: TextRange,
        replacement: &str,
    ) -> Result<Option<String>> {
        if range.is_empty() {
            return Ok(None);
        }
        let bytes = src.read_range(range, MAX_REPLACE_TARGET)?;
        Ok(std::str::from_utf8(&bytes)
            .ok()
            .and_then(|t| self.expand(t, replacement)))
    }

    /// すべて置換する。Undo は 1 単位。上限・中止で途中になる場合は何も変更しない
    /// （`Outcome` の終わり方で分かる）。
    pub fn replace_all<T: EditTarget>(
        &self,
        target: &mut T,
        replacement: &str,
        ctx: &TaskContext,
    ) -> Result<Outcome<ReplaceSummary>> {
        let cap = ctx.limits.max_items.unwrap_or(u64::MAX);
        let mut edits: Vec<(Range<u64>, String)> = Vec::new();
        let mut over = false;
        let completion = self.scan_forward(&*target, 0, ctx, |r, caps| {
            if edits.len() as u64 >= cap {
                over = true;
                return false;
            }
            let text = if self.regex_mode {
                let mut s = String::new();
                caps.expand(replacement, &mut s);
                s
            } else {
                replacement.to_owned()
            };
            edits.push((r, text));
            true
        })?;
        let completion = if over {
            Completion::ItemLimit
        } else {
            completion
        };
        if !completion.is_complete() {
            return Ok(Outcome::new(ReplaceSummary::default(), completion));
        }
        let replaced = edits.len() as u64;
        target.begin_undo_group();
        let mut result = Ok(());
        for (r, text) in edits.iter().rev() {
            result = target.replace(TextRange::new(r.start, r.end), text);
            if result.is_err() {
                break;
            }
        }
        target.end_undo_group();
        result?;
        Ok(Outcome::complete(ReplaceSummary { replaced }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::MemoryText;

    fn ctx() -> TaskContext {
        TaskContext::detached()
    }

    fn find(
        text: &str,
        opts: &SearchOptions,
        from: u64,
        dir: Direction,
        wrap: bool,
    ) -> Option<Hit> {
        let s = opts.compile().unwrap();
        s.find(&MemoryText::new(text), from, dir, wrap, &ctx())
            .unwrap()
            .value
    }

    #[test]
    fn literal_forward_backward_and_wrap() {
        let o = SearchOptions::literal("ab");
        let t = "ab cd ab ef";
        let h = find(t, &o, 0, Direction::Forward, false).unwrap();
        assert_eq!((h.range.start, h.range.end), (0, 2));
        let h = find(t, &o, 1, Direction::Forward, false).unwrap();
        assert_eq!(h.range.start, 6);
        assert!(find(t, &o, 7, Direction::Forward, false).is_none());
        let h = find(t, &o, 7, Direction::Forward, true).unwrap();
        assert!(h.wrapped && h.range.start == 0);
        let h = find(t, &o, 8, Direction::Backward, false).unwrap();
        assert_eq!(h.range.start, 6);
        let h = find(t, &o, 5, Direction::Backward, false).unwrap();
        assert_eq!(h.range.start, 0);
        let h = find(t, &o, 1, Direction::Backward, true).unwrap();
        assert!(h.wrapped && h.range.start == 6);
    }

    #[test]
    fn case_and_whole_word() {
        let mut o = SearchOptions::literal("Cat");
        assert!(find("a cat", &o, 0, Direction::Forward, false).is_some());
        o.case_sensitive = true;
        assert!(find("a cat", &o, 0, Direction::Forward, false).is_none());
        let mut o = SearchOptions::literal("cat");
        o.whole_word = true;
        let h = find("concat cat", &o, 0, Direction::Forward, false).unwrap();
        assert_eq!(h.range.start, 7);
    }

    #[test]
    fn regex_and_multibyte_positions() {
        let o = SearchOptions {
            query: r"\d+".into(),
            regex: true,
            ..Default::default()
        };
        let h = find("あい 123 う", &o, 0, Direction::Forward, false).unwrap();
        assert_eq!((h.range.start, h.range.end), (7, 10));
        // 文字の途中から始めても壊れない。
        let h = find("あい 123", &o, 1, Direction::Forward, false).unwrap();
        assert_eq!(h.range.start, 7);
    }

    #[test]
    fn invalid_patterns_report_errors() {
        for q in ["", "(", "a{2,1}"] {
            let o = SearchOptions {
                query: q.into(),
                regex: true,
                ..Default::default()
            };
            let e = o.compile().unwrap_err();
            assert!(matches!(e, Error::InvalidArgument { .. }), "{q}: {e:?}");
        }
        // 後方参照・先読みは対象外。
        let o = SearchOptions {
            query: r"(a)\1".into(),
            regex: true,
            ..Default::default()
        };
        assert!(o.compile().is_err());
        let o = SearchOptions {
            query: "(?=a)".into(),
            regex: true,
            ..Default::default()
        };
        assert!(o.compile().is_err());
    }

    #[test]
    fn match_crossing_window_boundary_is_found() {
        let mut text = "x".repeat(CHUNK as usize - 3);
        text.push_str("needle");
        text.push_str(&"y".repeat(1000));
        let o = SearchOptions::literal("needle");
        let h = find(&text, &o, 0, Direction::Forward, false).unwrap();
        assert_eq!(h.range.start, CHUNK - 3);
        let h = find(&text, &o, text.len() as u64, Direction::Backward, false).unwrap();
        assert_eq!(h.range.start, CHUNK - 3);
        // 単語単位でも窓の境界で誤判定しない。
        let mut text = "x ".repeat(CHUNK as usize / 2 - 2);
        text.push_str("word");
        text.push_str(" tail");
        let mut o = SearchOptions::literal("word");
        o.whole_word = true;
        assert!(find(&text, &o, 0, Direction::Forward, false).is_some());
    }

    #[test]
    fn invalid_utf8_is_skipped_not_fatal() {
        struct Raw(Vec<u8>);
        impl TextSource for Raw {
            fn len(&self) -> u64 {
                self.0.len() as u64
            }
            fn read_at(&self, o: u64, buf: &mut [u8]) -> Result<usize> {
                let s = (o as usize).min(self.0.len());
                let n = buf.len().min(self.0.len() - s);
                buf[..n].copy_from_slice(&self.0[s..s + n]);
                Ok(n)
            }
            fn line_count(&self) -> crate::document::LineCount {
                unimplemented!()
            }
            fn line_start(&self, _: u64) -> Result<Option<u64>> {
                unimplemented!()
            }
        }
        let mut bytes = b"aa ".to_vec();
        bytes.extend([0xFF, 0xFE]);
        bytes.extend(b" bb");
        let s = SearchOptions::literal("bb").compile().unwrap();
        let h = s
            .find(&Raw(bytes), 0, Direction::Forward, false, &ctx())
            .unwrap()
            .value
            .unwrap();
        assert_eq!(h.range.start, 6);
    }

    #[test]
    fn cancel_and_count_limit() {
        let t = MemoryText::new("a a a a a");
        let s = SearchOptions::literal("a").compile().unwrap();
        let c = ctx();
        c.cancel.cancel();
        let out = s.find(&t, 0, Direction::Forward, true, &c).unwrap();
        assert_eq!(out.completion, Completion::Cancelled);
        assert!(out.value.is_none());
        let out = s.count(&t, &ctx()).unwrap();
        assert_eq!((out.value, out.completion), (5, Completion::Complete));
        let out = s
            .count(&t, &ctx().with_limits(crate::core::Limits::items(3)))
            .unwrap();
        assert_eq!((out.value, out.completion), (3, Completion::ItemLimit));
    }

    #[test]
    fn replace_all_literal_regex_and_undo_group() {
        let mut t = MemoryText::new("foo.bar foo.baz");
        let s = SearchOptions::literal("foo.").compile().unwrap();
        let out = s.replace_all(&mut t, "$1-", &ctx()).unwrap();
        assert_eq!(out.value.replaced, 2);
        assert_eq!(t.as_str(), "$1-bar $1-baz");
        assert_eq!(t.open_undo_groups(), 0);

        let mut t = MemoryText::new("2024-01-05 and 2025-12-31");
        let o = SearchOptions {
            query: r"(\d+)-(\d+)-(\d+)".into(),
            regex: true,
            ..Default::default()
        };
        let s = o.compile().unwrap();
        s.replace_all(&mut t, "$3/$2/$1", &ctx()).unwrap();
        assert_eq!(t.as_str(), "05/01/2024 and 31/12/2025");
    }

    #[test]
    fn replace_all_over_limit_changes_nothing() {
        let mut t = MemoryText::new("a a a");
        let s = SearchOptions::literal("a").compile().unwrap();
        let out = s
            .replace_all(
                &mut t,
                "b",
                &ctx().with_limits(crate::core::Limits::items(2)),
            )
            .unwrap();
        assert_eq!(out.completion, Completion::ItemLimit);
        assert_eq!(t.as_str(), "a a a");
    }

    #[test]
    fn count_across_many_windows_has_no_duplicates_or_gaps() {
        // 窓の大きさと互いに素な周期で並べ、境界のあらゆる位置を通す。
        let unit = "abcdefg needle ";
        let n = 3 * CHUNK as usize / unit.len() + 7;
        let s = SearchOptions::literal("needle").compile().unwrap();
        let out = s.count(&MemoryText::new(unit.repeat(n)), &ctx()).unwrap();
        assert_eq!(out.value, n as u64);
    }

    #[test]
    fn replacement_for_selection() {
        let t = MemoryText::new("abc 12");
        let o = SearchOptions {
            query: r"(\d)(\d)".into(),
            regex: true,
            ..Default::default()
        };
        let s = o.compile().unwrap();
        let r = s.replacement_for(&t, TextRange::new(4, 6), "$2$1").unwrap();
        assert_eq!(r.as_deref(), Some("21"));
        let r = s.replacement_for(&t, TextRange::new(0, 3), "x").unwrap();
        assert_eq!(r, None);
    }
}
