//! `bigfile` 全体のテスト。小さな設定（ブロック・区切り・索引の間隔を数十バイト）で
//! 境界を踏ませ、素朴な実装と突き合わせる。

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use super::*;
use crate::core::{CancelToken, Completion, Error, Limits, Progress, TaskContext};
use crate::document::{LineCount, TextRange, TextSource};

fn temp_dir(tag: &str) -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "linote-bigfile-{tag}-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

struct TempFile {
    dir: PathBuf,
    path: PathBuf,
}

impl TempFile {
    fn new(tag: &str, content: &[u8]) -> Self {
        let dir = temp_dir(tag);
        let path = dir.join("data.txt");
        std::fs::write(&path, content).unwrap();
        Self { dir, path }
    }
}

impl Drop for TempFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn tiny_config() -> BigFileConfig {
    BigFileConfig {
        cache_budget_bytes: 4 * 64,
        index_budget_bytes: 1 << 20,
        block_bytes: 64,
        index_stride_bytes: 48,
        segment_bytes: 32,
        search_piece_bytes: 64,
    }
}

fn open_indexed(path: &Path, config: BigFileConfig) -> BigFile {
    let file = BigFile::open(path, config).unwrap();
    file.build_index(&TaskContext::detached()).unwrap();
    file
}

/// 再現できる乱数。
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

/// 日本語・絵文字・各種の行区切り・長い行が混ざった UTF-8 の本文。
fn mixed_text(seed: u64, target: usize) -> String {
    let mut rng = Rng(seed | 1);
    let words = [
        "abc",
        "日本語",
        "テキスト",
        "é",
        "😀",
        "x",
        " ",
        "needle",
        "Needle",
        "１２３",
        "\t",
        "ß",
    ];
    let mut text = String::new();
    while text.len() < target {
        let long = rng.below(12) == 0;
        let count = if long {
            20 + rng.below(120)
        } else {
            rng.below(9)
        };
        for _ in 0..count {
            text.push_str(words[rng.below(words.len() as u64) as usize]);
        }
        text.push_str(match rng.below(6) {
            0 => "\r\n",
            1 => "\r",
            2 => "\n\n",
            _ => "\n",
        });
    }
    if rng.below(2) == 0 {
        text.push_str("最後の行は区切りなし");
    }
    text
}

fn naive_line_starts(text: &[u8]) -> Vec<u64> {
    let mut starts = vec![0];
    for i in 0..text.len() {
        if text[i] == b'\n' || (text[i] == b'\r' && text.get(i + 1) != Some(&b'\n')) {
            starts.push(i as u64 + 1);
        }
    }
    starts
}

fn all_rows(file: &BigFile) -> Vec<Row> {
    let mut rows = Vec::new();
    let mut offset = 0;
    loop {
        let page = file.rows_forward(offset, 7).unwrap();
        let last = *page.last().unwrap();
        rows.extend(page);
        if last.last {
            return rows;
        }
        offset = last.next;
    }
}

/// 表示上の行が満たすべき性質を確かめる。
fn check_rows(text: &[u8], rows: &[Row], seg: u64, valid_utf8: bool) {
    let len = text.len() as u64;
    assert_eq!(rows[0].start, 0);
    assert!(rows[0].starts_line);
    for (i, row) in rows.iter().enumerate() {
        assert!(row.start <= row.end && row.end <= row.next && row.next <= len);
        assert!(row.len() <= 2 * seg + 4, "行が長すぎる: {row:?}");
        let content = &text[row.start as usize..row.end as usize];
        assert!(!content.iter().any(|&b| b == b'\n' || b == b'\r'));
        if valid_utf8 {
            assert!(
                std::str::from_utf8(content).is_ok(),
                "文字の途中で分かれた: {row:?}"
            );
        }
        let terminator = &text[row.end as usize..row.next as usize];
        if row.ends_line {
            assert!(matches!(terminator, b"\n" | b"\r" | b"\r\n"));
            if terminator == b"\r" {
                assert_ne!(text.get(row.next as usize), Some(&b'\n'));
            }
        } else {
            assert!(terminator.is_empty());
        }
        assert_eq!(row.last, i + 1 == rows.len());
        if let Some(next) = rows.get(i + 1) {
            assert_eq!(next.start, row.next);
            assert_eq!(next.starts_line, row.ends_line);
            assert!(row.ends_line || !row.is_empty(), "空の途中の行: {row:?}");
        } else {
            assert!(!row.ends_line && row.end == len);
        }
        // 短い行は分かれない。
        if row.starts_line && !row.ends_line && !row.last {
            assert!(row.len() + 4 > seg);
        }
    }
}

#[test]
fn big_file_is_send_and_sync() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<BigFile>();
    assert_send_sync::<SearchPattern>();
}

#[test]
fn reads_match_the_file_at_every_offset_and_size() {
    let text = mixed_text(1, 3000);
    let tmp = TempFile::new("read", text.as_bytes());
    let file = BigFile::open(&tmp.path, tiny_config()).unwrap();
    assert_eq!(file.len(), text.len() as u64);
    let mut rng = Rng(7);
    for _ in 0..500 {
        let offset = rng.below(text.len() as u64 + 10);
        let size = rng.below(700) as usize;
        let mut buf = vec![0u8; size];
        let n = file.read_at(offset, &mut buf).unwrap();
        let start = (offset as usize).min(text.len());
        let end = (start + size).min(text.len());
        assert_eq!(&buf[..n], &text.as_bytes()[start..end]);
    }
    assert_eq!(
        file.read_range(TextRange::new(10, 2000), 4000).unwrap(),
        &text.as_bytes()[10..2000]
    );
    assert!(matches!(
        file.read_range(TextRange::new(0, 2000), 1999),
        Err(Error::LimitExceeded { .. })
    ));
    assert!(matches!(
        file.read_range(TextRange::new(0, text.len() as u64 + 1), u64::MAX),
        Err(Error::InvalidArgument { .. })
    ));
}

#[test]
fn cache_never_holds_more_than_its_budget() {
    let text = mixed_text(2, 20_000);
    let tmp = TempFile::new("cache", text.as_bytes());
    let config = tiny_config();
    let file = open_indexed(&tmp.path, config);
    let mut rng = Rng(3);
    let mut buf = [0u8; 100];
    for _ in 0..2000 {
        let offset = rng.below(text.len() as u64);
        file.read_at(offset, &mut buf).unwrap();
        let _ = file.rows_forward(offset, 3).unwrap();
        let _ = file.line_of_offset(offset).unwrap();
        assert!(file.memory_usage().cache_bytes <= config.cache_budget_bytes);
    }
    assert!(file.memory_usage().cache_bytes > 0);
    // 1 ブロックに満たない予算は 1 ブロックに丸める。
    let small = BigFile::open(
        &tmp.path,
        BigFileConfig {
            cache_budget_bytes: 1,
            ..config
        },
    )
    .unwrap();
    for offset in [0u64, 500, 5000, 100] {
        small.read_at(offset, &mut buf).unwrap();
        assert!(small.memory_usage().cache_bytes <= 64);
    }
}

#[test]
fn line_lookups_match_a_naive_scan() {
    for seed in 1..6 {
        let text = mixed_text(seed, 6000);
        let starts = naive_line_starts(text.as_bytes());
        let tmp = TempFile::new("lines", text.as_bytes());
        let file = BigFile::open(&tmp.path, tiny_config()).unwrap();

        // 索引を作る前: 行 0 だけが分かる。
        assert_eq!(file.line_count(), LineCount::AtLeast(1));
        assert_eq!(file.line_start(0).unwrap(), Some(0));
        assert_eq!(file.line_start(1).unwrap(), None);
        assert_eq!(file.line_of_offset(1).unwrap(), None);

        file.build_index(&TaskContext::detached()).unwrap();
        assert_eq!(file.line_count(), LineCount::Exact(starts.len() as u64));
        let status = file.index_status();
        assert!(status.complete);
        assert_eq!(status.indexed_bytes, text.len() as u64);
        for (line, &start) in starts.iter().enumerate() {
            assert_eq!(
                file.line_start(line as u64).unwrap(),
                Some(start),
                "行 {line}"
            );
        }
        assert_eq!(file.line_start(starts.len() as u64).unwrap(), None);
        assert_eq!(file.line_start(u64::MAX).unwrap(), None);
        for offset in 0..=text.len() as u64 {
            // `\r\n` の `\n` を指す位置は、その区切りで終わる行に属する。
            let mid_crlf = offset > 0
                && text.as_bytes().get(offset as usize) == Some(&b'\n')
                && text.as_bytes()[offset as usize - 1] == b'\r';
            let expected = starts.partition_point(|&s| s <= offset) as u64 - 1;
            assert_eq!(
                file.line_of_offset(offset).unwrap(),
                Some(expected),
                "位置 {offset}（\\r\\n の途中: {mid_crlf}）"
            );
        }
    }
}

#[test]
fn empty_and_tiny_files() {
    for (content, lines) in [
        (&b""[..], 1u64),
        (b"a", 1),
        (b"\n", 2),
        (b"\r\n", 2),
        (b"\r", 2),
        (b"a\n", 2),
    ] {
        let tmp = TempFile::new("tiny", content);
        let file = open_indexed(&tmp.path, tiny_config());
        assert_eq!(file.line_count(), LineCount::Exact(lines));
        let rows = all_rows(&file);
        assert_eq!(rows.len() as u64, lines);
        check_rows(content, &rows, 32, true);
        assert_eq!(file.last_rows(5).unwrap(), rows);
        let mut buf = [0u8; 4];
        assert_eq!(file.read_at(content.len() as u64, &mut buf).unwrap(), 0);
        let hit = file
            .find(
                &SearchPattern::regex("^$", true).unwrap(),
                0,
                Direction::Forward,
                &TaskContext::detached(),
            )
            .unwrap();
        assert_eq!(hit.value.hit.is_some(), content != b"a");
    }
}

#[test]
fn rows_tile_the_file_and_never_split_a_character() {
    for seed in 1..8 {
        let text = mixed_text(seed, 5000);
        let tmp = TempFile::new("rows", text.as_bytes());
        let file = BigFile::open(&tmp.path, tiny_config()).unwrap();
        let rows = all_rows(&file);
        check_rows(text.as_bytes(), &rows, 32, true);

        // どの位置から求めても同じ行になる。
        for offset in 0..=text.len() as u64 {
            let expected = rows.iter().find(|r| r.contains(offset)).unwrap();
            assert_eq!(&file.row_at(offset).unwrap(), expected, "位置 {offset}");
        }
        // 後方へ。
        for (i, row) in rows.iter().enumerate().step_by(3) {
            for count in [1usize, 4, 50] {
                let got = file.rows_backward(row.start, count).unwrap();
                assert_eq!(
                    got,
                    rows[i.saturating_sub(count)..i],
                    "行 {i} の前 {count} 行"
                );
            }
        }
        assert_eq!(
            file.last_rows(10).unwrap(),
            rows[rows.len().saturating_sub(10)..]
        );
        assert_eq!(file.row_text(&rows[0]).unwrap().as_bytes(), {
            let r = rows[0];
            &text.as_bytes()[r.start as usize..r.end as usize]
        });
    }
}

#[test]
fn a_very_long_line_is_split_at_character_boundaries_with_bounded_reads() {
    // 改行のない 1 行。1〜4 バイトの文字が混ざる。
    let mut text = String::new();
    let mut rng = Rng(11);
    while text.len() < 40_000 {
        text.push(['a', 'é', '日', '😀'][rng.below(4) as usize]);
    }
    for tail in ["", "\n", "\r\nend"] {
        let content = format!("{text}{tail}");
        let tmp = TempFile::new("long", content.as_bytes());
        let config = tiny_config();
        let file = open_indexed(&tmp.path, config);
        let rows = all_rows(&file);
        check_rows(content.as_bytes(), &rows, 32, true);
        assert!(rows.len() > 40_000 / 68);
        let starts = naive_line_starts(content.as_bytes());
        // 行の途中のどこからでも、同じ分かれ目になる。
        let mut rng = Rng(5);
        for _ in 0..300 {
            let offset = rng.below(content.len() as u64 + 1);
            let expected = rows.iter().find(|r| r.contains(offset)).unwrap();
            assert_eq!(&file.row_at(offset).unwrap(), expected);
            assert_eq!(
                file.line_of_offset(offset).unwrap(),
                Some(starts.partition_point(|&s| s <= offset) as u64 - 1)
            );
            let snapped = file.snap_to_char_boundary(offset).unwrap();
            assert!(content.is_char_boundary(snapped as usize) && offset - snapped <= 3);
        }
        // 長大な行でも、キャッシュも索引も増えない。
        let usage = file.memory_usage();
        assert!(usage.cache_bytes <= config.cache_budget_bytes);
        assert!(usage.index_bytes <= 4096);
    }
}

#[test]
fn invalid_utf8_and_binary_data_do_not_break_rows() {
    let mut rng = Rng(99);
    let mut data: Vec<u8> = (0..6000).map(|_| rng.next() as u8).collect();
    // 継続バイトだけが続く部分（文字の境界が見つからない）。
    data.splice(1000..1000, std::iter::repeat_n(0x80u8, 500));
    let tmp = TempFile::new("binary", &data);
    let file = open_indexed(&tmp.path, tiny_config());
    let rows = all_rows(&file);
    check_rows(&data, &rows, 32, false);
    assert_eq!(
        file.line_count(),
        LineCount::Exact(naive_line_starts(&data).len() as u64)
    );
    assert!(!file.looks_like_utf8().unwrap());
    let _ = file.row_text(&rows[3]).unwrap();
}

fn naive_regex_hits(text: &[u8], regex: &regex::bytes::Regex) -> Vec<TextRange> {
    let starts = naive_line_starts(text);
    let mut hits = Vec::new();
    for (i, &start) in starts.iter().enumerate() {
        let end = match starts.get(i + 1) {
            None => text.len(),
            Some(&next) => {
                let next = next as usize;
                if text[next - 1] == b'\n' && next - 1 > start as usize && text[next - 2] == b'\r' {
                    next - 2
                } else {
                    next - 1
                }
            }
        };
        for m in regex.find_iter(&text[start as usize..end]) {
            hits.push(TextRange::new(
                start + m.start() as u64,
                start + m.end() as u64,
            ));
        }
    }
    hits
}

fn find(file: &BigFile, pattern: &SearchPattern, from: u64, direction: Direction) -> SearchResult {
    let outcome = file
        .find(pattern, from, direction, &TaskContext::detached())
        .unwrap();
    assert_eq!(outcome.completion, Completion::Complete);
    outcome.value
}

#[test]
fn regex_search_matches_a_naive_line_by_line_search() {
    // 行が検索の区切り（ここでは 4096 バイト）より短いので、行単位の結果と一致する。
    let config = BigFileConfig {
        search_piece_bytes: 4096,
        ..tiny_config()
    };
    for (seed, source, case_sensitive) in [
        (1u64, "needle", true),
        (2, "needle", false),
        (3, "^.{0,3}$", true),
        (4, "日本語+テ", true),
        (5, "[a-c]+\\s*$", true),
        (6, "^x|😀😀", true),
        (7, "(?i)SS|ß", true),
    ] {
        let text = mixed_text(seed, 9000);
        let tmp = TempFile::new("regex", text.as_bytes());
        let file = BigFile::open(&tmp.path, config).unwrap();
        let pattern = SearchPattern::regex(source, case_sensitive).unwrap();
        let reference = regex::bytes::RegexBuilder::new(source)
            .case_insensitive(!case_sensitive)
            .build()
            .unwrap();
        let hits = naive_regex_hits(text.as_bytes(), &reference);
        assert!(!hits.is_empty(), "{source} のヒットがない");
        let mut rng = Rng(seed);
        for _ in 0..150 {
            let from = rng.below(text.len() as u64 + 1);
            let forward = find(&file, &pattern, from, Direction::Forward);
            // 行の途中から探すときは、その位置から後ろで正規表現をかけ直した結果になる。
            let expected = hits.iter().find(|h| h.start >= from).copied();
            match forward.hit {
                Some(got) => {
                    assert!(got.start >= from, "{source} 前方 {from}");
                    assert!(expected.is_none_or(|want| got.start <= want.start));
                }
                None => assert_eq!(expected, None, "{source} 前方 {from}"),
            }
            assert!(forward.notes.line_based && !forward.notes.split_long_line);

            let backward = find(&file, &pattern, from, Direction::Backward);
            let expected = hits.iter().rev().find(|h| h.end <= from).copied();
            assert_eq!(backward.hit, expected, "{source} 後方 {from}");
        }
        // 前方へ順にたどると、素朴な検索と同じ列になる。
        let mut walked = Vec::new();
        let mut from = 0;
        while let Some(hit) = find(&file, &pattern, from, Direction::Forward).hit {
            walked.push(hit);
            from = if hit.is_empty() { hit.end + 1 } else { hit.end };
            if from > text.len() as u64 {
                break;
            }
        }
        let expected: Vec<_> = hits.clone();
        assert_eq!(walked.len(), expected.len(), "{source}");
        assert_eq!(walked, expected, "{source}");
    }
}

#[test]
fn literal_search_finds_matches_across_window_and_line_boundaries() {
    let text = mixed_text(21, 12_000);
    let tmp = TempFile::new("literal", text.as_bytes());
    let file = BigFile::open(&tmp.path, tiny_config()).unwrap();
    for (needle, case_sensitive) in [
        ("needle", true),
        ("NEEDLE", false),
        ("日本語テキスト", true),
        ("\n\n", true),
        ("x\r\n", true),
        ("😀", true),
        ("ß", false),
    ] {
        let pattern = SearchPattern::literal(needle, case_sensitive).unwrap();
        let reference = regex::bytes::RegexBuilder::new(&regex::escape(needle))
            .case_insensitive(!case_sensitive)
            .build()
            .unwrap();
        // 重なりも含めたすべての一致の先頭。
        let mut hits = Vec::new();
        let mut at = 0;
        while let Some(m) = reference.find_at(text.as_bytes(), at) {
            hits.push(TextRange::new(m.start() as u64, m.end() as u64));
            at = m.start() + 1;
        }
        assert!(!hits.is_empty(), "{needle:?} のヒットがない");
        let mut rng = Rng(77);
        for _ in 0..200 {
            let from = rng.below(text.len() as u64 + 1);
            let forward = find(&file, &pattern, from, Direction::Forward);
            assert_eq!(
                forward.hit,
                hits.iter().find(|h| h.start >= from).copied(),
                "{needle:?} 前方 {from}"
            );
            assert!(!forward.notes.line_based && !forward.notes.split_long_line);
            let backward = find(&file, &pattern, from, Direction::Backward);
            assert_eq!(
                backward.hit,
                hits.iter().rev().find(|h| h.end <= from).copied(),
                "{needle:?} 後方 {from}"
            );
        }
    }
    assert!(SearchPattern::literal("", true).is_err());
    assert!(SearchPattern::regex("(", true).is_err());
    assert!(SearchPattern::literal(&"a".repeat(MAX_PATTERN_BYTES + 1), true).is_err());
    assert!(SearchPattern::literal("(", true).is_ok());
}

#[test]
fn regex_search_in_a_long_line_works_piece_by_piece_and_says_so() {
    // 1 行が検索の区切り（64 バイト）よりずっと長い。
    let mut line = String::new();
    for i in 0..400 {
        line.push_str(&format!("語{i:04}-"));
    }
    let content = format!("first\n{line}\nlast needle\n");
    let tmp = TempFile::new("longregex", content.as_bytes());
    let file = BigFile::open(&tmp.path, tiny_config()).unwrap();
    let pattern = SearchPattern::regex("語0[0-9]{3}-", true).unwrap();
    assert_eq!(pattern.notes(file.config()).piece_bytes, Some(64));

    // 前方へたどる: 分かれ目をまたぐものを除いて見つかり、分けたことが分かる。
    let mut count = 0;
    let mut from = 0;
    let mut split = false;
    while let Some(hit) = {
        let result = find(&file, &pattern, from, Direction::Forward);
        split |= result.notes.split_long_line;
        result.hit
    } {
        let text = &content.as_bytes()[hit.start as usize..hit.end as usize];
        assert!(std::str::from_utf8(text).unwrap().starts_with("語0"));
        count += 1;
        from = hit.end;
    }
    assert!(split);
    assert!((300..=400).contains(&count), "{count}");

    // 後方へたどっても同じ数。
    let mut back = 0;
    let mut from = content.len() as u64;
    while let Some(hit) = find(&file, &pattern, from, Direction::Backward).hit {
        back += 1;
        from = hit.start;
    }
    assert_eq!(back, count);

    // 文字列検索は分かれ目に関係なく全部見つかる。
    let literal = SearchPattern::literal("-語", true).unwrap();
    let mut all = 0;
    let mut from = 0;
    while let Some(hit) = find(&file, &literal, from, Direction::Forward).hit {
        all += 1;
        from = hit.end;
    }
    assert_eq!(all, 399);

    // 普通の行だけを通る検索では、分けたとは言わない。
    let tail = SearchPattern::regex("needle$", true).unwrap();
    let from = content.len() as u64;
    let result = find(&file, &tail, from, Direction::Backward);
    assert_eq!(result.hit.map(|h| h.len()), Some(6));
    assert!(!result.notes.split_long_line);
}

#[test]
fn search_and_indexing_can_be_cancelled() {
    let text = mixed_text(31, 200_000);
    let tmp = TempFile::new("cancel", text.as_bytes());
    let file = BigFile::open(&tmp.path, tiny_config()).unwrap();

    // すでに中止されている。
    let cancelled = CancelToken::new();
    cancelled.cancel();
    let ctx = TaskContext::new(
        cancelled,
        Arc::new(crate::core::NoProgress),
        Limits::UNLIMITED,
    );
    assert!(matches!(file.build_index(&ctx), Err(Error::Cancelled)));
    for pattern in [
        SearchPattern::literal("никогда", true).unwrap(),
        SearchPattern::regex("никогда", true).unwrap(),
    ] {
        for (from, direction) in [
            (0, Direction::Forward),
            (text.len() as u64, Direction::Backward),
        ] {
            let outcome = file.find(&pattern, from, direction, &ctx).unwrap();
            assert_eq!(outcome.completion, Completion::Cancelled);
            assert_eq!(outcome.value.hit, None);
        }
    }
    assert!(!file.index_status().complete);
    // 中止のあとも使え、索引は作り直せる。
    file.build_index(&TaskContext::detached()).unwrap();
    assert!(file.index_status().complete);
}

#[test]
fn indexing_reports_progress_resumes_after_cancel_and_refuses_to_run_twice() {
    let content = vec![b'\n'; 3 * INDEX_CHUNK_BYTES + 123];
    let tmp = TempFile::new("progress", &content);
    let file = Arc::new(BigFile::open(&tmp.path, BigFileConfig::default()).unwrap());

    // 最初の進捗の報告で中止する。
    let cancel = CancelToken::new();
    let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
    let sink = {
        let (cancel, seen) = (cancel.clone(), Arc::clone(&seen));
        move |p: Progress| {
            seen.lock().unwrap().push(p);
            cancel.cancel();
        }
    };
    let ctx = TaskContext::new(cancel, Arc::new(sink), Limits::UNLIMITED);
    assert!(matches!(file.build_index(&ctx), Err(Error::Cancelled)));
    assert_eq!(
        *seen.lock().unwrap(),
        vec![Progress::new(
            INDEX_CHUNK_BYTES as u64,
            Some(content.len() as u64)
        )]
    );
    let status = file.index_status();
    assert!(!status.complete);
    assert_eq!(status.indexed_bytes, INDEX_CHUNK_BYTES as u64);
    assert_eq!(
        file.line_count(),
        LineCount::AtLeast(INDEX_CHUNK_BYTES as u64 + 1)
    );
    // 済んだ範囲は行ジャンプできる。その先はまだ。
    assert_eq!(file.line_start(1000).unwrap(), Some(1000));
    assert_eq!(file.line_start(2 * INDEX_CHUNK_BYTES as u64).unwrap(), None);

    // 続きから再開する（ワーカーで）。
    let (tx, rx) = std::sync::mpsc::channel();
    let pool = crate::core::WorkerPool::new(1);
    file.spawn_index(&pool, TaskContext::detached(), move |result| {
        tx.send(result).unwrap();
    });
    rx.recv().unwrap().unwrap();
    assert_eq!(
        file.line_count(),
        LineCount::Exact(content.len() as u64 + 1)
    );
    assert_eq!(
        file.line_start(content.len() as u64).unwrap(),
        Some(content.len() as u64)
    );
    // 完成後に呼んでも何もしない。
    file.build_index(&TaskContext::detached()).unwrap();
}

#[test]
fn index_memory_stays_within_budget_for_many_short_lines_and_for_one_long_line() {
    let budget = 4096u64;
    let config = BigFileConfig {
        index_budget_bytes: budget,
        index_stride_bytes: 16,
        ..tiny_config()
    };
    let many_lines = vec![b'\n'; 300_000];
    let mut one_line = vec![b'a'; 300_000];
    one_line.extend_from_slice(b"\nend");
    for content in [many_lines, one_line] {
        let tmp = TempFile::new("budget", &content);
        let file = open_indexed(&tmp.path, config);
        let usage = file.memory_usage();
        assert!(usage.index_bytes <= budget, "{usage:?}");
        let starts = naive_line_starts(&content);
        assert_eq!(file.line_count(), LineCount::Exact(starts.len() as u64));
        let mut rng = Rng(8);
        for _ in 0..200 {
            let line = rng.below(starts.len() as u64);
            assert_eq!(file.line_start(line).unwrap(), Some(starts[line as usize]));
            let offset = rng.below(content.len() as u64 + 1);
            assert_eq!(
                file.line_of_offset(offset).unwrap(),
                Some(starts.partition_point(|&s| s <= offset) as u64 - 1)
            );
        }
        assert!(file.memory_usage().cache_bytes <= config.cache_budget_bytes);
    }
}

fn assert_invalidated(file: &BigFile) {
    assert!(file.is_invalidated());
    let mut buf = [0u8; 8];
    assert!(matches!(
        file.read_at(0, &mut buf),
        Err(Error::ExternalChange { .. })
    ));
    assert!(matches!(
        file.line_start(0),
        Err(Error::ExternalChange { .. })
    ));
    assert!(matches!(
        file.line_of_offset(0),
        Err(Error::ExternalChange { .. })
    ));
    assert!(matches!(
        file.rows_forward(0, 1),
        Err(Error::ExternalChange { .. })
    ));
    assert!(matches!(
        file.last_rows(1),
        Err(Error::ExternalChange { .. })
    ));
    assert!(matches!(
        file.build_index(&TaskContext::detached()),
        Err(Error::ExternalChange { .. })
    ));
    assert!(matches!(
        file.find(
            &SearchPattern::literal("a", true).unwrap(),
            0,
            Direction::Forward,
            &TaskContext::detached()
        ),
        Err(Error::ExternalChange { .. })
    ));
    assert!(matches!(
        file.check_external_change(),
        Err(Error::ExternalChange { .. })
    ));
    // キャッシュも索引も捨てている。
    let usage = file.memory_usage();
    assert_eq!((usage.cache_bytes, usage.index_bytes), (0, 0));
    assert_eq!(file.line_count(), LineCount::AtLeast(1));
}

#[test]
fn truncation_invalidates_cache_index_and_search() {
    let text = mixed_text(41, 30_000);
    let tmp = TempFile::new("truncate", text.as_bytes());
    let file = open_indexed(&tmp.path, tiny_config());
    let mut buf = [0u8; 32];
    file.read_at(0, &mut buf).unwrap();
    file.check_external_change().unwrap();

    // 他のアプリが書き込み用に開いて切り詰める（共有読み取りなので妨げない）。
    let writer = std::fs::OpenOptions::new()
        .write(true)
        .open(&tmp.path)
        .unwrap();
    writer.set_len(100).unwrap();
    drop(writer);

    // キャッシュにない範囲を読もうとした時点で検知する。
    let far = text.len() as u64 - 50;
    assert!(matches!(
        file.read_at(far, &mut buf),
        Err(Error::ExternalChange { .. })
    ));
    assert_invalidated(&file);

    // 開き直すと新しい内容が読める。
    let reopened = file.reopen().unwrap();
    assert_eq!(reopened.len(), 100);
    reopened.build_index(&TaskContext::detached()).unwrap();
    assert_eq!(
        reopened.read_range(TextRange::new(0, 100), 100).unwrap(),
        &text.as_bytes()[..100]
    );
}

#[test]
fn truncation_is_detected_by_running_index_and_search() {
    let content = vec![b'x'; 200_000];
    let tmp = TempFile::new("truncate-running", &content);
    let pattern = SearchPattern::regex("y", true).unwrap();
    for search in [false, true] {
        std::fs::write(&tmp.path, &content).unwrap();
        let file = BigFile::open(&tmp.path, tiny_config()).unwrap();
        // 開いたあと、処理の前に切り詰められた。
        let writer = std::fs::OpenOptions::new()
            .write(true)
            .open(&tmp.path)
            .unwrap();
        writer.set_len(10).unwrap();
        drop(writer);
        let result = if search {
            file.find(&pattern, 0, Direction::Forward, &TaskContext::detached())
                .map(|_| ())
        } else {
            file.build_index(&TaskContext::detached())
        };
        assert!(
            matches!(result, Err(Error::ExternalChange { .. })),
            "{result:?}"
        );
        assert_invalidated(&file);
    }
}

#[test]
fn growth_and_in_place_rewrite_are_detected() {
    let tmp = TempFile::new("grow", b"line 1\nline 2\n");
    let file = open_indexed(&tmp.path, tiny_config());
    let mut buf = [0u8; 4];
    file.read_at(0, &mut buf).unwrap();

    // 追記（共有書き込みを妨げない）。
    let mut writer = std::fs::OpenOptions::new()
        .append(true)
        .open(&tmp.path)
        .unwrap();
    std::io::Write::write_all(&mut writer, b"line 3\n").unwrap();
    drop(writer);
    assert!(matches!(
        file.check_external_change(),
        Err(Error::ExternalChange { .. })
    ));
    assert_invalidated(&file);

    // 同じ長さの上書き: 更新時刻で検知する。
    let file = open_indexed(&tmp.path, tiny_config());
    let len = file.len();
    let writer = std::fs::OpenOptions::new()
        .write(true)
        .open(&tmp.path)
        .unwrap();
    let later = std::time::SystemTime::now() + std::time::Duration::from_secs(5);
    writer.set_modified(later).unwrap();
    drop(writer);
    assert_eq!(std::fs::metadata(&tmp.path).unwrap().len(), len);
    assert!(matches!(
        file.check_external_change(),
        Err(Error::ExternalChange { .. })
    ));
    assert_invalidated(&file);
}

#[test]
fn replacement_and_deletion_are_detected() {
    let tmp = TempFile::new("replace", b"old content\nmore\n");
    let file = open_indexed(&tmp.path, tiny_config());
    let mut buf = [0u8; 4];
    file.read_at(0, &mut buf).unwrap();

    // 別のファイルで置き換える（一時ファイル経由の保存と同じ形）。開いているファイルの
    // 置換を OS が拒む場合は、その場での書き換えで代える。
    let other = tmp.dir.join("new.txt");
    std::fs::write(&other, b"new content, longer than before\n").unwrap();
    if std::fs::rename(&other, &tmp.path).is_err() {
        std::fs::write(&tmp.path, b"new content, longer than before\n").unwrap();
    }
    assert!(matches!(
        file.check_external_change(),
        Err(Error::ExternalChange { .. })
    ));
    assert_invalidated(&file);
    let reopened = file.reopen().unwrap();
    assert_eq!(
        reopened.read_range(TextRange::new(0, 11), 11).unwrap(),
        b"new content"
    );

    // 削除。開いているファイルの削除を OS が拒む場合は確かめない。
    if std::fs::remove_file(&tmp.path).is_ok() && !tmp.path.exists() {
        assert!(matches!(
            reopened.check_external_change(),
            Err(Error::ExternalChange { .. })
        ));
        assert_invalidated(&reopened);
    }
    assert!(matches!(
        BigFile::open(tmp.dir.join("missing.txt"), tiny_config()),
        Err(Error::Io { .. })
    ));
    assert!(BigFile::open(&tmp.dir, tiny_config()).is_err());
}

#[test]
fn concurrent_readers_indexer_and_searchers_agree() {
    let text = mixed_text(51, 400_000);
    let starts = Arc::new(naive_line_starts(text.as_bytes()));
    let tmp = TempFile::new("threads", text.as_bytes());
    let file = Arc::new(BigFile::open(&tmp.path, tiny_config()).unwrap());
    let text = Arc::new(text);
    let mut handles = Vec::new();
    {
        let file = Arc::clone(&file);
        handles.push(std::thread::spawn(move || {
            file.build_index(&TaskContext::detached()).unwrap();
        }));
    }
    for t in 0..3u64 {
        let (file, text, starts) = (Arc::clone(&file), Arc::clone(&text), Arc::clone(&starts));
        handles.push(std::thread::spawn(move || {
            let mut rng = Rng(100 + t);
            let pattern = SearchPattern::literal("needle", true).unwrap();
            let mut buf = [0u8; 200];
            for _ in 0..400 {
                let offset = rng.below(text.len() as u64);
                let n = file.read_at(offset, &mut buf).unwrap();
                assert_eq!(
                    &buf[..n],
                    &text.as_bytes()[offset as usize..offset as usize + n]
                );
                // 索引が進んでいる途中でも、答えが返るなら正しい。
                let line = rng.below(starts.len() as u64);
                if let Some(start) = file.line_start(line).unwrap() {
                    assert_eq!(start, starts[line as usize]);
                }
                if let Some(line) = file.line_of_offset(offset).unwrap() {
                    assert_eq!(line, starts.partition_point(|&s| s <= offset) as u64 - 1);
                }
                let hit = file
                    .find(
                        &pattern,
                        offset,
                        Direction::Forward,
                        &TaskContext::detached(),
                    )
                    .unwrap()
                    .value
                    .hit;
                let expected = text.as_bytes()[offset as usize..]
                    .windows(6)
                    .position(|w| w == b"needle")
                    .map(|i| offset + i as u64);
                // 文字の途中から探した場合も、バイト位置としては同じ結果になる。
                assert_eq!(hit.map(|h| h.start), expected);
                assert!(file.memory_usage().cache_bytes <= file.config().cache_budget_bytes);
            }
        }));
    }
    for handle in handles {
        handle.join().unwrap();
    }
    // 2 つ目の索引作成は、完成済みなので何もしない。
    file.build_index(&TaskContext::detached()).unwrap();
    assert_eq!(file.line_count(), LineCount::Exact(starts.len() as u64));
}

#[test]
fn mode_thresholds() {
    let t = LargeModeThresholds::DEFAULT;
    assert_eq!(large_mode_reason(&FileProfile::of_size(1024), &t), None);
    assert_eq!(
        large_mode_reason(&FileProfile::of_size(t.max_bytes + 1), &t),
        Some(LargeModeReason::Size)
    );
    let profile = FileProfile::of_text(b"ab\r\ncdef\n\rg");
    assert_eq!(
        (profile.bytes, profile.lines, profile.max_line_bytes),
        (11, Some(4), Some(4))
    );
    let small = LargeModeThresholds {
        max_bytes: 100,
        max_lines: 3,
        max_line_bytes: 100,
    };
    assert_eq!(
        large_mode_reason(&profile, &small),
        Some(LargeModeReason::LineCount)
    );
    let small = LargeModeThresholds {
        max_lines: 10,
        max_line_bytes: 3,
        ..small
    };
    assert_eq!(
        large_mode_reason(&profile, &small),
        Some(LargeModeReason::LongLine)
    );
    assert_eq!(FileProfile::of_text(b"").lines, Some(1));
}
