//! 疎な行索引。
//!
//! 行の先頭のうち、前の控えから `stride` バイト以上離れた最初のものだけを
//! 「控え」（行番号とバイト位置の組）として記録する。したがって:
//!
//! - 控えの数は「走査したバイト数 ÷ `stride`」以下。短い行が大量にあっても、改行が
//!   ほぼない長大な 1 行でも増えない。
//! - 控えと控えの間にある行の先頭は、すべて手前の控えから `scan_bound` バイト未満に
//!   ある。行ジャンプ・行番号の計算は、控えから高々 `scan_bound` バイトを走査すれば済む
//!   （長大な行の残りは走査しなくてよい）。
//! - 控えの数が上限に達したら `stride` を倍にして間引く。メモリは上限を超えない。

use super::lines::{count_breaks, find_break_end};

/// 行の先頭の控え。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Checkpoint {
    /// 行番号（0 始まり）。
    pub line: u64,
    /// その行の先頭のバイト位置。
    pub offset: u64,
}

pub(crate) const CHECKPOINT_BYTES: u64 = std::mem::size_of::<Checkpoint>() as u64;

pub(crate) struct LineIndex {
    entries: Vec<Checkpoint>,
    stride: u64,
    scan_bound: u64,
    /// ここまで走査した（この位置より前で終わる行区切りはすべて数えた）。
    scanned_to: u64,
    /// `scanned_to` を含む行の行番号。
    line_at_scanned: u64,
    complete: bool,
    max_entries: usize,
}

/// 走査を再開するための状態の写し。
#[derive(Debug, Clone, Copy)]
pub(crate) struct ScanState {
    pub scanned_to: u64,
    pub line: u64,
    pub last_checkpoint: u64,
    pub stride: u64,
}

impl LineIndex {
    pub(crate) fn new(stride: u64, max_entries: usize) -> Self {
        Self {
            entries: vec![Checkpoint { line: 0, offset: 0 }],
            stride: stride.max(1),
            scan_bound: stride.max(1),
            scanned_to: 0,
            line_at_scanned: 0,
            complete: false,
            max_entries: max_entries.max(2),
        }
    }

    /// 無効化: 控えを捨て、領域も返す。以後は使わない。
    pub(crate) fn clear(&mut self) {
        self.entries = Vec::new();
        self.scanned_to = 0;
        self.line_at_scanned = 0;
        self.complete = false;
    }

    pub(crate) fn is_complete(&self) -> bool {
        self.complete
    }

    pub(crate) fn scanned_to(&self) -> u64 {
        self.scanned_to
    }

    /// これまでに確かめた行数（走査が済んだ範囲の行区切りの数 + 1）。
    pub(crate) fn lines_seen(&self) -> u64 {
        self.line_at_scanned + 1
    }

    pub(crate) fn scan_bound(&self) -> u64 {
        self.scan_bound
    }

    #[cfg(test)]
    pub(crate) fn stride(&self) -> u64 {
        self.stride
    }

    pub(crate) fn memory_bytes(&self) -> u64 {
        self.entries.capacity() as u64 * CHECKPOINT_BYTES
    }

    #[cfg(test)]
    pub(crate) fn checkpoints(&self) -> usize {
        self.entries.len()
    }

    pub(crate) fn scan_state(&self) -> ScanState {
        ScanState {
            scanned_to: self.scanned_to,
            line: self.line_at_scanned,
            last_checkpoint: self.entries.last().map_or(0, |c| c.offset),
            stride: self.stride,
        }
    }

    /// `line` 以前で最も近い控え。`line` がまだ走査の済んでいない行なら `None`。
    pub(crate) fn checkpoint_for_line(&self, line: u64) -> Option<Checkpoint> {
        if line > self.line_at_scanned {
            return None;
        }
        let i = self.entries.partition_point(|c| c.line <= line);
        self.entries.get(i.checked_sub(1)?).copied()
    }

    /// `offset` 以前で最も近い控え。`offset` がまだ走査の済んでいない位置なら `None`。
    pub(crate) fn checkpoint_for_offset(&self, offset: u64) -> Option<Checkpoint> {
        if offset > self.scanned_to {
            return None;
        }
        let i = self.entries.partition_point(|c| c.offset <= offset);
        self.entries.get(i.checked_sub(1)?).copied()
    }

    /// 走査の結果を足す。`found` は [`scan_chunk`] が見つけた控え（位置の昇順）。
    pub(crate) fn extend(&mut self, found: &[Checkpoint], scanned_to: u64, line: u64) {
        for &checkpoint in found {
            // 間引きで `stride` が変わったあとは、新しい間隔に満たない控えを捨てる。
            let last = self.entries.last().map_or(0, |c| c.offset);
            if checkpoint.offset - last < self.stride {
                continue;
            }
            if self.entries.len() >= self.max_entries {
                self.thin();
                let last = self.entries.last().map_or(0, |c| c.offset);
                if checkpoint.offset - last < self.stride {
                    continue;
                }
            }
            if self.entries.len() == self.entries.capacity() {
                let room = self.max_entries - self.entries.len();
                self.entries
                    .reserve_exact(self.entries.len().clamp(64, 1 << 16).min(room));
            }
            self.entries.push(checkpoint);
        }
        self.scanned_to = scanned_to;
        self.line_at_scanned = line;
    }

    pub(crate) fn finish(&mut self) {
        self.complete = true;
    }

    /// 間隔を倍にして控えを間引く。間引いたあとも「控えの間の行の先頭は手前の控えから
    /// `scan_bound` 未満」が成り立つ（捨てた控えは残した控えから新しい間隔未満にあり、
    /// その先の行の先頭は捨てた控えから古い `scan_bound` 未満にあるため）。
    fn thin(&mut self) {
        while self.entries.len() >= self.max_entries {
            let stride = self.stride.saturating_mul(2);
            let mut kept = 0usize;
            for i in 1..self.entries.len() {
                if self.entries[i].offset - self.entries[kept].offset >= stride {
                    kept += 1;
                    self.entries[kept] = self.entries[i];
                }
            }
            self.entries.truncate(kept + 1);
            self.scan_bound = stride.saturating_add(self.scan_bound);
            self.stride = stride;
        }
    }
}

/// `buf`（本文の `base` から始まる部分）を走査して、行区切りを数え、控えを集める。
///
/// `after` は `buf` の直後のバイト（本文の末尾なら `None`）。`state` を進め、見つけた控えを
/// `found` に足す。
pub(crate) fn scan_chunk(
    buf: &[u8],
    after: Option<u8>,
    state: &mut ScanState,
    found: &mut Vec<Checkpoint>,
) {
    let base = state.scanned_to;
    let mut i = 0usize;
    while i < buf.len() {
        // この位置より前で終わる区切りは、次の行の先頭が控えの間隔に届かない。数えるだけ。
        let threshold = (state.last_checkpoint + state.stride - 1).saturating_sub(base);
        if threshold > i as u64 {
            let end = threshold.min(buf.len() as u64) as usize;
            let next = if end < buf.len() {
                Some(buf[end])
            } else {
                after
            };
            state.line += count_breaks(&buf[i..end], next);
            i = end;
            continue;
        }
        match find_break_end(buf, i, after) {
            Some(j) => {
                state.line += 1;
                let offset = base + j as u64 + 1;
                found.push(Checkpoint {
                    line: state.line,
                    offset,
                });
                state.last_checkpoint = offset;
                i = j + 1;
            }
            None => break,
        }
    }
    state.scanned_to = base + buf.len() as u64;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line_starts(text: &[u8]) -> Vec<u64> {
        let mut starts = vec![0];
        for i in 0..text.len() {
            if text[i] == b'\n' || (text[i] == b'\r' && text.get(i + 1) != Some(&b'\n')) {
                starts.push(i as u64 + 1);
            }
        }
        starts
    }

    fn build(text: &[u8], chunk: usize, stride: u64, max_entries: usize) -> LineIndex {
        let mut index = LineIndex::new(stride, max_entries);
        let mut pos = 0;
        while pos < text.len() {
            let end = (pos + chunk).min(text.len());
            let mut state = index.scan_state();
            let mut found = Vec::new();
            scan_chunk(
                &text[pos..end],
                text.get(end).copied(),
                &mut state,
                &mut found,
            );
            index.extend(&found, state.scanned_to, state.line);
            pos = end;
        }
        index.finish();
        index
    }

    fn check(text: &[u8], chunk: usize, stride: u64, max_entries: usize) {
        let starts = line_starts(text);
        let index = build(text, chunk, stride, max_entries);
        assert_eq!(index.lines_seen(), starts.len() as u64);
        assert!(index.checkpoints() <= max_entries.max(2));
        for (line, &start) in starts.iter().enumerate() {
            let cp = index.checkpoint_for_line(line as u64).unwrap();
            assert!(cp.line <= line as u64 && cp.offset <= start);
            assert_eq!(starts[cp.line as usize], cp.offset);
            assert!(
                start - cp.offset < index.scan_bound() || start == cp.offset,
                "行 {line} の先頭 {start} が控え {cp:?} から遠い（上限 {}）",
                index.scan_bound()
            );
        }
        assert!(index.checkpoint_for_line(starts.len() as u64).is_none());
        for offset in 0..=text.len() as u64 {
            let cp = index.checkpoint_for_offset(offset).unwrap();
            assert!(cp.offset <= offset);
        }
    }

    #[test]
    fn checkpoints_are_line_starts_within_the_scan_bound() {
        let mut text = Vec::new();
        for i in 0..400u32 {
            text.extend_from_slice(format!("line {i}").as_bytes());
            text.extend_from_slice(match i % 4 {
                0 => b"\n",
                1 => b"\r\n",
                2 => b"\r",
                _ => b"\n\n",
            });
            if i % 97 == 0 {
                text.extend(std::iter::repeat_n(b'x', 300));
            }
        }
        for chunk in [1, 2, 7, 64, 100_000] {
            for stride in [1, 8, 64, 1000] {
                check(&text, chunk, stride, 1 << 20);
                check(&text, chunk, stride, 8);
            }
        }
        check(b"", 4, 4, 4);
        check(b"\r\n", 1, 1, 4);
        check(&vec![b'\n'; 5000], 33, 4, 16);
        check(&vec![b'a'; 5000], 33, 4, 16);
    }

    #[test]
    fn thinning_keeps_the_number_of_checkpoints_bounded() {
        let text = vec![b'\n'; 100_000];
        let index = build(&text, 4096, 16, 32);
        assert!(index.checkpoints() <= 32);
        assert!(index.stride() > 16);
        assert!(index.memory_bytes() <= 32 * CHECKPOINT_BYTES);
        assert_eq!(index.lines_seen(), 100_001);
    }
}
