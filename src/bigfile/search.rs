//! 巨大ファイルの検索。指定位置から前方または後方へ、最初のヒットを 1 つ返す。
//!
//! - **文字列検索**（[`SearchKind::Literal`]）: 行に関係なくバイト列として探す。長大な行の
//!   中でも、行をまたいでも見つかる。制約はない。
//! - **正規表現検索**（[`SearchKind::Regex`]）: **行単位**。1 行ずつ（行区切りを除いて）
//!   正規表現にかけるので、行をまたぐ一致は見つからない。`^`・`$` は行の先頭・末尾に合う。
//!   長大な行は表示上の行と同じ規則（[`super::rows`]）で `search_piece_bytes` ごとに分け、
//!   分けた単位ごとに探す。その場合、分かれ目をまたぐ一致は見つからず、`^`・`$` は
//!   分かれ目にも合う。分けた行に出会ったかどうかを [`SearchNotes::split_long_line`] で返す
//!   （UI が案内できるように）。
//!
//! 大文字小文字を区別しない検索は Unicode の単純な畳み込みによる（`regex` の `(?i)`）。
//! 不正な UTF-8 の部分には一致しない。
//!
//! 作業領域は検索 1 回につき一定（[`SearchPattern::working_bytes`]）で、キャッシュは
//! 使わない（表示用のキャッシュを追い出さない）。

use regex::bytes::{Regex, RegexBuilder};

use crate::core::{Completion, Error, Outcome, Result, TaskContext};
use crate::document::{TextRange, TextSource};

use super::lines::{is_terminator, rfind_terminator};
use super::rows::{locate, next_row, row_in_window, Row, RowState, Window};
use super::source::BigFile;
use super::{BigFileConfig, MAX_PATTERN_BYTES};

/// 後方の文字列検索で、読み込んだ範囲を末尾から調べるときの単位。
const LITERAL_BACKWARD_PIECE: usize = 64 * 1024;
/// 進捗を報告する間隔（バイト）。
const PROGRESS_INTERVAL_BYTES: u64 = 4 * 1024 * 1024;
/// 正規表現をコンパイルした結果の大きさの上限と、検索時の作業表の上限。
const REGEX_SIZE_LIMIT: usize = 4 * 1024 * 1024;
const REGEX_DFA_SIZE_LIMIT: usize = 4 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchKind {
    /// 文字列検索。
    Literal,
    /// 正規表現検索（行単位）。
    Regex,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    /// `from` 以降で始まる最初の一致。
    Forward,
    /// `from` 以前で終わる最後の一致。
    Backward,
}

/// コンパイル済みの検索条件。`Send + Sync`。UI スレッドで作ってワーカーへ渡せる。
#[derive(Debug, Clone)]
pub struct SearchPattern {
    regex: Regex,
    kind: SearchKind,
    /// 文字列検索で、一致の長さがこれを超えない（バイト）。
    max_match_bytes: usize,
}

impl SearchPattern {
    /// 検索条件を作る。空の文字列、長すぎる文字列（[`MAX_PATTERN_BYTES`] 超）、無効な
    /// 正規表現は [`Error::InvalidArgument`]。
    pub fn new(text: &str, kind: SearchKind, case_sensitive: bool) -> Result<Self> {
        if text.is_empty() {
            return Err(Error::invalid_argument("pattern", "検索文字列が空です"));
        }
        if text.len() > MAX_PATTERN_BYTES {
            return Err(Error::invalid_argument(
                "pattern",
                format!("検索文字列が長すぎます（上限 {MAX_PATTERN_BYTES} バイト）"),
            ));
        }
        let source = match kind {
            SearchKind::Literal => regex::escape(text),
            SearchKind::Regex => text.to_owned(),
        };
        let regex = RegexBuilder::new(&source)
            .case_insensitive(!case_sensitive)
            .unicode(true)
            .size_limit(REGEX_SIZE_LIMIT)
            .dfa_size_limit(REGEX_DFA_SIZE_LIMIT)
            .build()
            .map_err(|e| Error::invalid_argument("pattern", e.to_string()))?;
        Ok(Self {
            regex,
            kind,
            // 大文字小文字の畳み込みで、1 文字が別の長さの 1 文字に合うことがある。
            max_match_bytes: text.chars().count() * 4,
        })
    }

    pub fn literal(text: &str, case_sensitive: bool) -> Result<Self> {
        Self::new(text, SearchKind::Literal, case_sensitive)
    }

    pub fn regex(text: &str, case_sensitive: bool) -> Result<Self> {
        Self::new(text, SearchKind::Regex, case_sensitive)
    }

    pub fn kind(&self) -> SearchKind {
        self.kind
    }

    /// この条件での検索の制約（検索する前に画面で案内するため）。
    /// `split_long_line` は偽（検索してみないと分からない）。
    pub fn notes(&self, config: &BigFileConfig) -> SearchNotes {
        match self.kind {
            SearchKind::Literal => SearchNotes {
                line_based: false,
                piece_bytes: None,
                split_long_line: false,
            },
            SearchKind::Regex => SearchNotes {
                line_based: true,
                piece_bytes: Some(config.normalized().search_piece_bytes as u64),
                split_long_line: false,
            },
        }
    }

    fn window_bytes(&self, config: &BigFileConfig) -> usize {
        match self.kind {
            SearchKind::Literal | SearchKind::Regex => {
                (4 * config.search_piece_bytes as usize).max(4 * self.max_match_bytes) + 64
            }
        }
    }

    /// 検索 1 回が使う作業領域の目安（読み込み用の領域 + 正規表現の作業表の上限）。
    pub fn working_bytes(&self, config: &BigFileConfig) -> u64 {
        (self.window_bytes(&config.normalized()) + REGEX_SIZE_LIMIT + REGEX_DFA_SIZE_LIMIT) as u64
    }
}

/// 検索の制約。画面の案内に使う。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SearchNotes {
    /// 行単位の検索か（行をまたぐ一致は見つからない）。
    pub line_based: bool,
    /// 長大な行を分ける単位（バイト）。1 行がこれ以上なら分けて探すことがある。
    /// 行単位でない検索では `None`。
    pub piece_bytes: Option<u64>,
    /// 今回の検索で、分けて探した長大な行があったか。真なら、分かれ目をまたぐ一致を
    /// 見逃した可能性がある。
    pub split_long_line: bool,
}

/// 検索の結果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SearchResult {
    /// 見つかった一致の範囲。
    pub hit: Option<TextRange>,
    /// 調べ終えた位置。前方なら `[from, stopped_at)` に、後方なら `[stopped_at, from)` に
    /// （報告した一致のほかに）一致がない。中止された検索を続けるときの目安。
    pub stopped_at: u64,
    pub notes: SearchNotes,
}

/// 進捗の報告を間引く。
struct Reporter<'a> {
    ctx: &'a TaskContext,
    origin: u64,
    total: u64,
    reported: u64,
}

impl<'a> Reporter<'a> {
    fn new(ctx: &'a TaskContext, origin: u64, total: u64) -> Self {
        Self {
            ctx,
            origin,
            total,
            reported: 0,
        }
    }

    fn at(&mut self, position: u64) {
        let done = position.abs_diff(self.origin);
        if done >= self.reported + PROGRESS_INTERVAL_BYTES {
            self.reported = done;
            self.ctx.report(done, Some(self.total));
        }
    }
}

impl BigFile {
    /// `from` から `direction` の向きに探し、最初の一致を返す。**ワーカースレッドで呼ぶ。**
    ///
    /// - 前方: `from` 以降で始まる最初の一致。空の一致（`^$` など）も返すので、続けて探す
    ///   ときは、空の一致なら 1 文字先から探す。折り返し（末尾まで探したら先頭から）は
    ///   呼び出し側が `from` を変えて呼び直す。
    /// - 後方: `from` 以前で終わる最後の一致。
    /// - 見つからずに端まで達したら `hit` が `None` で [`Completion::Complete`]。
    /// - 中止されたら `hit` が `None` で [`Completion::Cancelled`]（エラーにしない）。
    /// - 進捗の単位はバイト（`done` = 調べた量、`total` = `from` から端までの量）。
    /// - 外部変更を検知したら [`Error::ExternalChange`]。それまでに返したヒットも古い
    ///   （UI は文書の版を進めて捨てる）。
    pub fn find(
        &self,
        pattern: &SearchPattern,
        from: u64,
        direction: Direction,
        ctx: &TaskContext,
    ) -> Result<Outcome<SearchResult>> {
        self.check_external_change()?;
        let from = from.min(self.len());
        let mut notes = pattern.notes(self.config());
        let (hit, stopped_at, completion) = match (pattern.kind, direction) {
            (SearchKind::Literal, Direction::Forward) => {
                self.literal_forward(pattern, from, ctx)?
            }
            (SearchKind::Literal, Direction::Backward) => {
                self.literal_backward(pattern, from, ctx)?
            }
            (SearchKind::Regex, Direction::Forward) => {
                self.regex_forward(pattern, from, ctx, &mut notes)?
            }
            (SearchKind::Regex, Direction::Backward) => {
                self.regex_backward(pattern, from, ctx, &mut notes)?
            }
        };
        Ok(Outcome::new(
            SearchResult {
                hit,
                stopped_at,
                notes,
            },
            completion,
        ))
    }

    fn literal_forward(
        &self,
        pattern: &SearchPattern,
        from: u64,
        ctx: &TaskContext,
    ) -> Result<Scan> {
        let len = self.len();
        let overlap = pattern.max_match_bytes;
        let mut window = Window::new(self, pattern.window_bytes(self.config()), true);
        let mut reporter = Reporter::new(ctx, from, len - from);
        let mut pos = from;
        loop {
            if ctx.cancel.is_cancelled() {
                return Ok((None, pos, Completion::Cancelled));
            }
            window.load(pos)?;
            let bytes = window.bytes();
            let at_end = window.end() == len;
            // 読み込んだ範囲の末尾近くで始まる一致は、範囲からはみ出して見えないことが
            // ある。末尾から `overlap` 以内で始まるものは次の回に回す。
            if let Some(m) = pattern.regex.find(bytes) {
                if at_end || m.start() + overlap <= bytes.len() {
                    let hit = TextRange::new(pos + m.start() as u64, pos + m.end() as u64);
                    return Ok((Some(hit), hit.start, Completion::Complete));
                }
            }
            if at_end {
                return Ok((None, len, Completion::Complete));
            }
            pos += (bytes.len() - overlap) as u64;
            reporter.at(pos);
        }
    }

    fn literal_backward(
        &self,
        pattern: &SearchPattern,
        from: u64,
        ctx: &TaskContext,
    ) -> Result<Scan> {
        let overlap = pattern.max_match_bytes;
        let mut window = Window::new(self, pattern.window_bytes(self.config()), true);
        let capacity = window.capacity() as u64;
        let mut reporter = Reporter::new(ctx, from, from);
        let mut hi = from;
        loop {
            if ctx.cancel.is_cancelled() {
                return Ok((None, hi, Completion::Cancelled));
            }
            let lo = hi.saturating_sub(capacity);
            window.load_len(lo, (hi - lo) as usize)?;
            if let Some((start, end)) = last_literal_match(&pattern.regex, window.bytes(), overlap)
            {
                let hit = TextRange::new(lo + start as u64, lo + end as u64);
                return Ok((Some(hit), hit.end, Completion::Complete));
            }
            if lo == 0 {
                return Ok((None, 0, Completion::Complete));
            }
            // `lo` をまたぐ一致を次の回で拾えるよう、`overlap` だけ重ねる。
            hi = lo + overlap as u64;
            reporter.at(hi);
        }
    }

    fn regex_forward(
        &self,
        pattern: &SearchPattern,
        from: u64,
        ctx: &TaskContext,
        notes: &mut SearchNotes,
    ) -> Result<Scan> {
        let len = self.len();
        let piece = self.config().search_piece_bytes as u64;
        let mut window = Window::new(self, pattern.window_bytes(self.config()), true);
        let mut reporter = Reporter::new(ctx, from, len - from);
        let (mut state, _) = locate(&mut window, piece, from)?;
        loop {
            if ctx.cancel.is_cancelled() {
                return Ok((None, state.pos.max(from), Completion::Cancelled));
            }
            let row = next_row(&mut window, piece, &mut state)?;
            note_split(&row, notes);
            if from <= row.end {
                let content = window.slice(row.start, row.end);
                let at = from.saturating_sub(row.start) as usize;
                if let Some(m) = pattern.regex.find_at(content, at) {
                    let hit =
                        TextRange::new(row.start + m.start() as u64, row.start + m.end() as u64);
                    return Ok((Some(hit), hit.start, Completion::Complete));
                }
            }
            if row.last {
                return Ok((None, len, Completion::Complete));
            }
            reporter.at(row.next);
        }
    }

    fn regex_backward(
        &self,
        pattern: &SearchPattern,
        from: u64,
        ctx: &TaskContext,
        notes: &mut SearchNotes,
    ) -> Result<Scan> {
        let piece = self.config().search_piece_bytes as u64;
        let mut window = Window::new(self, pattern.window_bytes(self.config()), true);
        let mut reporter = Reporter::new(ctx, from, from);
        let found = |row: &Row, (start, end): (usize, usize)| -> Scan {
            let hit = TextRange::new(row.start + start as u64, row.start + end as u64);
            (Some(hit), hit.end, Completion::Complete)
        };

        // `from` を含む行。`from` より後ろの部分も文脈（`$` など）として正規表現に見せる。
        let (_, row) = locate(&mut window, piece, from)?;
        note_split(&row, notes);
        let limit = (from.min(row.end) - row.start) as usize;
        if let Some(m) = last_match(&pattern.regex, window.slice(row.start, row.end), limit) {
            return Ok(found(&row, m));
        }

        // それより前を、ひとかたまりずつ後ろから調べる。
        let mut hi = row.start;
        let mut rows: Vec<Row> = Vec::new();
        while hi > 0 {
            if ctx.cancel.is_cancelled() {
                return Ok((None, hi, Completion::Cancelled));
            }
            let (block, _) = locate(&mut window, piece, hi.saturating_sub(2 * piece))?;
            let base = block.pos;
            // `[base, hi)` と、分かれ目の判定に要る少し先までを読み込む。
            window.load(base)?;
            let mut end = hi;
            while end > base {
                // `[base, end)` の最後の行（の、この範囲にある部分）を取り出す。
                let bytes = window.slice(base, end);
                let mut body = bytes.len();
                if is_terminator(bytes[body - 1]) {
                    let crlf = bytes[body - 1] == b'\n' && body >= 2 && bytes[body - 2] == b'\r';
                    body -= if crlf { 2 } else { 1 };
                }
                let mut state = match rfind_terminator(&bytes[..body]) {
                    Some(i) => RowState::line_start(base + i as u64 + 1),
                    None => block,
                };
                let line_start = state.pos;
                // その行を前から表示上の行に分け、後ろから調べる。
                rows.clear();
                loop {
                    let row = row_in_window(&window, piece, &mut state)?;
                    rows.push(row);
                    if row.next >= end {
                        break;
                    }
                }
                for row in rows.iter().rev() {
                    note_split(row, notes);
                    let content = window.slice(row.start, row.end);
                    if let Some(m) = last_match(&pattern.regex, content, content.len()) {
                        return Ok(found(row, m));
                    }
                }
                end = line_start;
            }
            hi = base;
            reporter.at(hi);
        }
        Ok((None, 0, Completion::Complete))
    }
}

/// 検索の内部の結果: ヒット、調べ終えた位置、終わり方。
type Scan = (Option<TextRange>, u64, Completion);

fn note_split(row: &Row, notes: &mut SearchNotes) {
    if !row.starts_line || !(row.ends_line || row.last) {
        notes.split_long_line = true;
    }
}

/// `content` の中で、`limit` 以前で終わる最後の一致（前から重ならないように数えたもの）。
fn last_match(regex: &Regex, content: &[u8], limit: usize) -> Option<(usize, usize)> {
    let mut last = None;
    for m in regex.find_iter(content) {
        if m.end() <= limit {
            last = Some((m.start(), m.end()));
        } else if m.start() >= limit {
            break;
        }
    }
    last
}

/// `bytes` の中で最も後ろから始まる一致。一致の長さは `overlap` 以下であること。
fn last_literal_match(regex: &Regex, bytes: &[u8], overlap: usize) -> Option<(usize, usize)> {
    let mut hi = bytes.len();
    while hi > 0 {
        let lo = hi.saturating_sub(LITERAL_BACKWARD_PIECE);
        // `[lo, hi)` で始まる一致を調べる。
        let slice = &bytes[lo..(hi + overlap).min(bytes.len())];
        let mut last = None;
        let mut at = 0;
        while at < hi - lo {
            match regex.find_at(slice, at) {
                Some(m) if m.start() < hi - lo => {
                    last = Some((lo + m.start(), lo + m.end()));
                    at = m.start() + 1;
                }
                _ => break,
            }
        }
        if last.is_some() {
            return last;
        }
        hi = lo;
    }
    None
}
