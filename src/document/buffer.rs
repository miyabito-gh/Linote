//! 本文へのアクセスの取り決め。
//!
//! 本文の実体は種類ごとに違う（Scintilla、巨大ファイルの部分読み込み、テスト用の
//! メモリ上の文字列）。中核の処理（検索、コマンド）はこのトレイト越しに扱う。
//!
//! 位置の取り決め:
//! - 本文は UTF-8。位置は**先頭からのバイト数**（`u64`）。Scintilla の位置と同じ。
//! - 行番号は内部では 0 始まり。画面とコマンドの引数では 1 始まり。
//! - 行の区切りは `\n`、`\r\n`、単独の `\r`。
//!
//! 実装する側:
//! - `ui::editor`（タスク B）: Scintilla に対する [`EditTarget`]。UI スレッド専用。
//! - `bigfile`（タスク J）: 巨大ファイルに対する [`TextSource`]。`Send + Sync` にして
//!   バックグラウンドの検索からも読めるようにする。
//! - [`MemoryText`] — テスト用、および GUI なしで本文を扱う処理用。

use crate::core::{Error, Result};

/// 本文中の範囲（UTF-8 のバイト位置、`start <= end`、`end` は含まない）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TextRange {
    pub start: u64,
    pub end: u64,
}

impl TextRange {
    pub const fn new(start: u64, end: u64) -> Self {
        Self { start, end }
    }

    /// 長さ 0 の範囲（挿入位置）。
    pub const fn at(position: u64) -> Self {
        Self::new(position, position)
    }

    pub const fn len(&self) -> u64 {
        self.end.saturating_sub(self.start)
    }

    pub const fn is_empty(&self) -> bool {
        self.end <= self.start
    }

    /// `start <= end <= len` を確かめる。
    pub fn check_within(&self, len: u64) -> Result<()> {
        if self.start <= self.end && self.end <= len {
            Ok(())
        } else {
            Err(Error::invalid_argument(
                "range",
                format!(
                    "{}..{} は本文（{len} バイト）の範囲外です",
                    self.start, self.end
                ),
            ))
        }
    }
}

/// 選択範囲。`anchor` が選択の始点、`caret` がカーソル位置（どちらが前でもよい）。
/// 矩形選択・複数選択は UI 側だけで扱い、ここでは主選択だけを表す。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Selection {
    pub anchor: u64,
    pub caret: u64,
}

impl Selection {
    pub const fn caret_at(position: u64) -> Self {
        Self {
            anchor: position,
            caret: position,
        }
    }

    pub fn range(&self) -> TextRange {
        TextRange::new(self.anchor.min(self.caret), self.anchor.max(self.caret))
    }
}

/// 行数。巨大ファイルでは索引の作成中は確定しない。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineCount {
    /// 確定した行数（空の本文でも 1）。
    Exact(u64),
    /// 少なくともこの行数（索引の作成中）。
    AtLeast(u64),
}

/// 読み取り専用の本文。
pub trait TextSource {
    /// 本文の長さ（バイト）。
    fn len(&self) -> u64;

    fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// `offset` から `buf` に読めるだけ読み、読んだバイト数を返す（末尾では 0）。
    /// 文字の途中で切れることがある。呼び出し側が境界を扱う。
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<usize>;

    fn line_count(&self) -> LineCount;

    /// `line`（0 始まり）の先頭のバイト位置。その行が（まだ）存在しなければ `None`。
    fn line_start(&self, line: u64) -> Result<Option<u64>>;

    /// 範囲を丸ごと読む。`max_bytes` を超える範囲は [`Error::LimitExceeded`]
    /// （巨大ファイルの全体を誤って読み込まないため、上限を必ず指定する）。
    fn read_range(&self, range: TextRange, max_bytes: u64) -> Result<Vec<u8>> {
        range.check_within(self.len())?;
        if range.len() > max_bytes {
            return Err(Error::LimitExceeded {
                what: "読み取る範囲".into(),
                limit: max_bytes,
            });
        }
        let mut out = vec![0u8; range.len() as usize];
        let mut filled = 0;
        while filled < out.len() {
            let n = self.read_at(range.start + filled as u64, &mut out[filled..])?;
            if n == 0 {
                return Err(Error::Internal(
                    "本文が読み取りの途中で短くなりました".into(),
                ));
            }
            filled += n;
        }
        Ok(out)
    }
}

/// 編集できる本文。[`Capability::Edit`](super::Capability::Edit) を持つ文書だけが実装する。
///
/// 変更は必ずコマンド経由で行う（`docs/architecture.md` の「コマンド」）。
pub trait EditTarget: TextSource {
    fn selection(&self) -> Selection;

    fn set_selection(&mut self, selection: Selection);

    /// `range` を `text` で置き換える（挿入は長さ 0 の範囲、削除は空の `text`）。
    /// 範囲が本文の外、または文字の途中なら [`Error::InvalidArgument`]。
    fn replace(&mut self, range: TextRange, text: &str) -> Result<()>;

    /// ここから [`end_undo_group`](Self::end_undo_group) までの変更を Undo の 1 単位に
    /// まとめる（Scintilla の `SCI_BEGINUNDOACTION`）。入れ子にできる。
    fn begin_undo_group(&mut self);

    fn end_undo_group(&mut self);
}

/// メモリ上の文字列による本文。テスト用、および GUI なしの処理用。Undo は持たない。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MemoryText {
    text: String,
    selection: Selection,
    undo_depth: u32,
}

impl MemoryText {
    pub fn new(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            selection: Selection::default(),
            undo_depth: 0,
        }
    }

    pub fn as_str(&self) -> &str {
        &self.text
    }

    /// 開いたままの Undo のまとまりの数（テストで対応を確かめるため）。
    pub fn open_undo_groups(&self) -> u32 {
        self.undo_depth
    }

    fn line_starts(&self) -> impl Iterator<Item = u64> + '_ {
        let bytes = self.text.as_bytes();
        std::iter::once(0).chain(bytes.iter().enumerate().filter_map(move |(i, &b)| {
            let ends_line = b == b'\n' || (b == b'\r' && bytes.get(i + 1) != Some(&b'\n'));
            ends_line.then_some(i as u64 + 1)
        }))
    }
}

impl TextSource for MemoryText {
    fn len(&self) -> u64 {
        self.text.len() as u64
    }

    fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<usize> {
        let bytes = self.text.as_bytes();
        let start = (offset.min(bytes.len() as u64)) as usize;
        let n = buf.len().min(bytes.len() - start);
        buf[..n].copy_from_slice(&bytes[start..start + n]);
        Ok(n)
    }

    fn line_count(&self) -> LineCount {
        LineCount::Exact(self.line_starts().count() as u64)
    }

    fn line_start(&self, line: u64) -> Result<Option<u64>> {
        Ok(usize::try_from(line)
            .ok()
            .and_then(|line| self.line_starts().nth(line)))
    }
}

impl EditTarget for MemoryText {
    fn selection(&self) -> Selection {
        self.selection
    }

    fn set_selection(&mut self, selection: Selection) {
        let len = self.len();
        self.selection = Selection {
            anchor: selection.anchor.min(len),
            caret: selection.caret.min(len),
        };
    }

    fn replace(&mut self, range: TextRange, text: &str) -> Result<()> {
        range.check_within(self.len())?;
        let (start, end) = (range.start as usize, range.end as usize);
        if !self.text.is_char_boundary(start) || !self.text.is_char_boundary(end) {
            return Err(Error::invalid_argument("range", "文字の途中を指しています"));
        }
        self.text.replace_range(start..end, text);
        self.selection = Selection::caret_at((start + text.len()) as u64);
        Ok(())
    }

    fn begin_undo_group(&mut self) {
        self.undo_depth += 1;
    }

    fn end_undo_group(&mut self) {
        self.undo_depth = self.undo_depth.saturating_sub(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lines_split_on_all_three_line_endings() {
        let t = MemoryText::new("a\nb\r\nc\rd");
        assert_eq!(t.line_count(), LineCount::Exact(4));
        let starts: Vec<_> = (0..5).map(|l| t.line_start(l).unwrap()).collect();
        assert_eq!(starts, vec![Some(0), Some(2), Some(5), Some(7), None]);
        assert_eq!(MemoryText::new("").line_count(), LineCount::Exact(1));
        assert_eq!(MemoryText::new("a\n").line_count(), LineCount::Exact(2));
    }

    #[test]
    fn read_range_checks_bounds_and_limit() {
        let t = MemoryText::new("日本語abc");
        assert_eq!(t.read_range(TextRange::new(9, 12), 100).unwrap(), b"abc");
        assert_eq!(t.read_range(TextRange::at(12), 0).unwrap(), b"");
        assert!(matches!(
            t.read_range(TextRange::new(0, 12), 5),
            Err(Error::LimitExceeded { limit: 5, .. })
        ));
        for bad in [TextRange::new(0, 13), TextRange::new(5, 4)] {
            assert!(matches!(
                t.read_range(bad, 100),
                Err(Error::InvalidArgument { .. })
            ));
        }
        let mut buf = [0u8; 4];
        assert_eq!(t.read_at(10, &mut buf).unwrap(), 2);
        assert_eq!(t.read_at(99, &mut buf).unwrap(), 0);
    }

    #[test]
    fn replace_edits_and_rejects_split_characters() {
        let mut t = MemoryText::new("日本語");
        t.replace(TextRange::new(3, 6), "x").unwrap();
        assert_eq!(t.as_str(), "日x語");
        assert_eq!(t.selection(), Selection::caret_at(4));
        t.replace(TextRange::at(0), "タブ\t").unwrap();
        assert_eq!(t.as_str(), "タブ\t日x語");
        assert!(t.replace(TextRange::new(1, 3), "").is_err());
        assert!(t.replace(TextRange::new(0, 99), "").is_err());
        assert_eq!(t.as_str(), "タブ\t日x語");
    }

    #[test]
    fn selection_range_is_ordered() {
        let s = Selection {
            anchor: 9,
            caret: 2,
        };
        assert_eq!(s.range(), TextRange::new(2, 9));
        assert_eq!(s.range().len(), 7);
        assert!(TextRange::at(3).is_empty());
    }
}
