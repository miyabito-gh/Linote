//! `scripts/gen-large-fixtures.py` が作るデータを使ったテストと、1 GiB 級での計測。
//!
//! 普段のテストは小さめ（数 MiB）のデータを `target/tmp/bigfile-fixtures/` に一度だけ作って
//! 使う（固定シードなので内容は毎回同じ）。Python 3 が要る。Wine の中からは
//! `start /unix` で Linux 側の Python を呼ぶ。どちらも使えない環境では、あらかじめ
//!
//! ```text
//! python3 scripts/gen-large-fixtures.py --out <dir> \
//!     --lines-size 6MiB --longline-size 3MiB --mixed-size 2MiB
//! ```
//!
//! で作ったディレクトリを環境変数 `LINOTE_BIGFILE_FIXTURES` に指定する。
//!
//! # 1 GiB 級の計測
//!
//! ```text
//! python3 scripts/gen-large-fixtures.py            # target/fixtures-large/（約 1.4 GiB）
//! cargo test --release --lib --target x86_64-unknown-linux-gnu \
//!     bigfile::fixture_tests::measure_large_fixtures -- --ignored --nocapture
//! ```
//!
//! Windows 向け（Wine）なら `--target` を外す。別の場所のデータを使うなら
//! `LINOTE_BIGFILE_LARGE_FIXTURES` にディレクトリを指定する。

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, OnceLock};
use std::time::Instant;

use serde_json::Value;

use super::*;
use crate::core::{CancelToken, Completion, Error, Limits, Progress, TaskContext};
use crate::document::{LineCount, TextRange, TextSource};

const SMALL_SIZES: [(&str, &str); 3] = [
    ("--lines-size", "6MiB"),
    ("--longline-size", "3MiB"),
    ("--mixed-size", "2MiB"),
];

struct Fixtures {
    dir: PathBuf,
    manifest: Value,
}

struct Hit {
    needle: String,
    /// 1 始まり。
    line: u64,
    offset: u64,
}

impl Fixtures {
    fn load(dir: &Path) -> Option<Self> {
        let text = std::fs::read_to_string(dir.join("manifest.json")).ok()?;
        let manifest: Value = serde_json::from_str(&text).ok()?;
        let files = manifest.get("files")?;
        ["lines.txt", "longline.txt", "mixed.txt"]
            .iter()
            .all(|name| files.get(name).is_some() && dir.join(name).is_file())
            .then(|| Self {
                dir: dir.to_path_buf(),
                manifest,
            })
    }

    fn path(&self, name: &str) -> PathBuf {
        self.dir.join(name)
    }

    fn number(&self, name: &str, key: &str) -> u64 {
        self.manifest["files"][name][key].as_u64().unwrap()
    }

    fn hits(&self, name: &str) -> Vec<Hit> {
        self.manifest["files"][name]["hits"]
            .as_array()
            .unwrap()
            .iter()
            .map(|h| Hit {
                needle: h["needle"].as_str().unwrap().to_owned(),
                line: h["line"].as_u64().unwrap(),
                offset: h["byte_offset"].as_u64().unwrap(),
            })
            .collect()
    }
}

/// Wine の中のパス（`Z:\home\...`）を Linux 側のパスにする。
fn unix_path(path: &Path) -> Option<String> {
    let text = path.to_str()?;
    let rest = text.strip_prefix("Z:").or_else(|| text.strip_prefix("z:"))?;
    Some(rest.replace('\\', "/"))
}

fn run_generator(out: &Path) {
    let script = Path::new("scripts").join("gen-large-fixtures.py");
    let sizes = SMALL_SIZES.iter().flat_map(|(k, v)| [*k, *v]);
    let quiet = |command: &mut Command| {
        let _ = command
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    };
    for python in ["python3", "python"] {
        if out.join("manifest.json").is_file() {
            return;
        }
        let mut command = Command::new(python);
        command.arg(&script).arg("--out").arg(out).args(sizes.clone());
        quiet(&mut command);
    }
    // Wine: Linux 側の Python を呼ぶ。終了コードは当てにならないので、できた物で判断する。
    let cwd = std::env::current_dir().unwrap();
    if let (false, Some(script), Some(out)) = (
        out.join("manifest.json").is_file(),
        unix_path(&cwd.join(&script)),
        unix_path(&cwd.join(out)),
    ) {
        let mut command = Command::new("cmd");
        command
            .args(["/c", "start", "/wait", "/unix", "/usr/bin/env", "python3"])
            .arg(script)
            .arg("--out")
            .arg(out)
            .args(sizes);
        quiet(&mut command);
    }
}

fn fixtures() -> &'static Fixtures {
    static FIXTURES: OnceLock<Fixtures> = OnceLock::new();
    FIXTURES.get_or_init(|| {
        if let Some(dir) = std::env::var_os("LINOTE_BIGFILE_FIXTURES") {
            return Fixtures::load(Path::new(&dir))
                .expect("LINOTE_BIGFILE_FIXTURES に manifest.json と 3 つのファイルがない");
        }
        let dir = Path::new("target").join("tmp").join("bigfile-fixtures");
        if let Some(found) = Fixtures::load(&dir) {
            return found;
        }
        // 別のディレクトリに作ってから名前を変える（作りかけを他のテスト実行に見せない）。
        let staging = dir.with_file_name(format!("bigfile-fixtures-{}", std::process::id()));
        std::fs::create_dir_all(&staging).unwrap();
        run_generator(&staging);
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::rename(&staging, &dir);
        let _ = std::fs::remove_dir_all(&staging);
        Fixtures::load(&dir).expect(
            "テストデータを作れない。python3 を使えるようにするか、\
             scripts/gen-large-fixtures.py で作ったディレクトリを LINOTE_BIGFILE_FIXTURES に指定する",
        )
    })
}

fn open(path: &Path) -> BigFile {
    BigFile::open(path, BigFileConfig::default()).unwrap()
}

fn find_all(file: &BigFile, pattern: &SearchPattern) -> Vec<TextRange> {
    let mut hits = Vec::new();
    let mut from = 0;
    loop {
        let outcome = file
            .find(pattern, from, Direction::Forward, &TaskContext::detached())
            .unwrap();
        assert_eq!(outcome.completion, Completion::Complete);
        match outcome.value.hit {
            Some(hit) => {
                hits.push(hit);
                from = hit.end;
            }
            None => return hits,
        }
    }
}

fn find_all_backward(file: &BigFile, pattern: &SearchPattern) -> Vec<TextRange> {
    let mut hits = Vec::new();
    let mut from = file.len();
    loop {
        let outcome = file
            .find(pattern, from, Direction::Backward, &TaskContext::detached())
            .unwrap();
        match outcome.value.hit {
            Some(hit) => {
                hits.push(hit);
                from = hit.start;
            }
            None => {
                hits.reverse();
                return hits;
            }
        }
    }
}

fn within_budget(file: &BigFile) {
    let usage = file.memory_usage();
    assert!(usage.cache_bytes <= file.config().cache_budget_bytes, "{usage:?}");
    assert!(usage.index_bytes <= file.config().index_budget_bytes, "{usage:?}");
}

#[test]
fn many_short_lines() {
    let fx = fixtures();
    let name = "lines.txt";
    let (bytes, lines) = (fx.number(name, "bytes"), fx.number(name, "lines"));
    let file = open(&fx.path(name));
    assert_eq!(file.len(), bytes);
    assert!(file.looks_like_utf8().unwrap() && !file.has_bom().unwrap());

    // 先頭の表示は索引を待たない。
    let first = file.rows_forward(0, 50).unwrap();
    assert_eq!(first.len(), 50);
    assert_eq!(file.row_text(&first[0]).unwrap().len(), 79);
    assert!(file.row_text(&first[49]).unwrap().starts_with("0000000050 "));
    assert!(file.memory_usage().cache_bytes <= 2 * DEFAULT_BLOCK_BYTES as u64);

    file.build_index(&TaskContext::detached()).unwrap();
    // 行 N（1 始まり）の先頭は (N-1)*80。末尾の改行のあとに空の最後の行がある。
    assert_eq!(file.line_count(), LineCount::Exact(lines + 1));
    for line in [1, 2, 817, 818, 819, 820, lines / 2, lines - 1, lines] {
        assert_eq!(file.line_start(line - 1).unwrap(), Some((line - 1) * 80));
        assert_eq!(file.line_of_offset((line - 1) * 80 + 79).unwrap(), Some(line - 1));
        let row = file.row_at((line - 1) * 80 + 40).unwrap();
        assert_eq!(file.row_text(&row).unwrap()[..10], format!("{line:010}"));
    }
    assert_eq!(file.line_start(lines).unwrap(), Some(bytes));
    assert_eq!(file.line_start(lines + 1).unwrap(), None);

    // 末尾への移動。
    let tail = file.last_rows(3).unwrap();
    assert_eq!(tail.len(), 3);
    assert!(tail[2].last && tail[2].is_empty());
    assert_eq!(tail[1].start, (lines - 1) * 80);

    // 検索: 埋め込まれた文字列の位置と一致する。
    let expected = fx.hits(name);
    let pattern = SearchPattern::literal("LINOTE_NEEDLE_LINES_", true).unwrap();
    let hits = find_all(&file, &pattern);
    assert_eq!(
        hits.iter().map(|h| h.start).collect::<Vec<_>>(),
        expected.iter().map(|h| h.offset).collect::<Vec<_>>()
    );
    assert_eq!(find_all_backward(&file, &pattern), hits);
    for hit in &expected {
        assert_eq!(file.line_of_offset(hit.offset).unwrap(), Some(hit.line - 1));
        let range = TextRange::new(hit.offset, hit.offset + hit.needle.len() as u64);
        assert_eq!(file.read_text(range, 100).unwrap(), hit.needle);
    }
    for (source, case_sensitive) in [
        (r"^\d{10} LINOTE_NEEDLE_LINES_\d", true),
        ("linote_needle_LINES_[0-9]", false),
    ] {
        let pattern = SearchPattern::regex(source, case_sensitive).unwrap();
        let regex_hits = find_all(&file, &pattern);
        assert_eq!(regex_hits.len(), expected.len(), "{source}");
        assert_eq!(find_all_backward(&file, &pattern), regex_hits, "{source}");
        let last = file
            .find(&pattern, 0, Direction::Forward, &TaskContext::detached())
            .unwrap();
        assert!(!last.value.notes.split_long_line);
    }
    within_budget(&file);
}

#[test]
fn one_very_long_line() {
    let fx = fixtures();
    let name = "longline.txt";
    let first_line = fx.number(name, "first_line_bytes");
    let file = open(&fx.path(name));
    let seg = file.config().segment_bytes as u64;

    // 先頭の表示: 長大な 1 行でも、読むのは最初の数ブロックだけ。
    let first = file.rows_forward(0, 40).unwrap();
    assert_eq!(first.len(), 40);
    assert!(first.iter().all(|r| r.len() <= 2 * seg + 4 && !r.ends_line));
    assert!(first[0].starts_line && !first[1].starts_line);
    assert!(file.memory_usage().cache_bytes <= 8 * DEFAULT_BLOCK_BYTES as u64);

    // 行の途中へ飛んでも、読む量は一定。
    let middle = file.row_at(first_line / 2).unwrap();
    assert!(middle.contains(first_line / 2) && middle.len() <= 2 * seg + 4);
    let around = file.rows_backward(middle.start, 30).unwrap();
    assert_eq!(around.len(), 30);
    assert_eq!(around[29].next, middle.start);
    assert!(file.memory_usage().cache_bytes <= 16 * DEFAULT_BLOCK_BYTES as u64);

    file.build_index(&TaskContext::detached()).unwrap();
    assert_eq!(
        file.line_count(),
        LineCount::Exact(fx.number(name, "lines") + 1)
    );
    assert_eq!(file.line_start(1).unwrap(), Some(first_line + 1));
    assert_eq!(file.line_of_offset(first_line).unwrap(), Some(0));
    assert_eq!(file.line_of_offset(first_line + 1).unwrap(), Some(1));
    let second = file.row_at(first_line + 1).unwrap();
    assert_eq!(file.row_text(&second).unwrap(), "second line");
    let tail = file.last_rows(2).unwrap();
    assert_eq!(file.row_text(&tail[0]).unwrap(), "last line");

    let expected = fx.hits(name);
    let pattern = SearchPattern::literal("linote_needle_longline_", false).unwrap();
    let hits = find_all(&file, &pattern);
    assert_eq!(
        hits.iter().map(|h| h.start).collect::<Vec<_>>(),
        expected.iter().map(|h| h.offset).collect::<Vec<_>>()
    );
    assert_eq!(find_all_backward(&file, &pattern), hits);
    for hit in &expected {
        assert_eq!(file.line_of_offset(hit.offset).unwrap(), Some(hit.line - 1));
    }

    // 正規表現は長大な行を分けて探し、そのことを返す。
    let pattern = SearchPattern::regex(r"LINOTE_NEEDLE_LONGLINE_\d", true).unwrap();
    let outcome = file
        .find(&pattern, 0, Direction::Forward, &TaskContext::detached())
        .unwrap();
    assert_eq!(outcome.value.hit.map(|h| h.start), Some(expected[0].offset));
    assert!(outcome.value.notes.line_based && outcome.value.notes.split_long_line);
    let last = file
        .find(&pattern, file.len(), Direction::Backward, &TaskContext::detached())
        .unwrap();
    assert_eq!(
        last.value.hit.map(|h| h.start),
        Some(expected.last().unwrap().offset)
    );
    assert!(!last.value.notes.split_long_line);
    within_budget(&file);
    // 索引は数個の控えだけ。
    assert!(file.memory_usage().index_bytes <= 64 * 1024);
}

#[test]
fn mixed_japanese_text() {
    let fx = fixtures();
    let name = "mixed.txt";
    let file = open(&fx.path(name));
    file.build_index(&TaskContext::detached()).unwrap();
    assert_eq!(
        file.line_count(),
        LineCount::Exact(fx.number(name, "lines") + 1)
    );
    let expected = fx.hits(name);
    for (pattern, skip) in [
        (SearchPattern::literal("検索LINOTE_NEEDLE_MIXED_", true).unwrap(), 6),
        (SearchPattern::regex(r"検索linote_needle_mixed_\d終", false).unwrap(), 6),
    ] {
        let hits = find_all(&file, &pattern);
        assert_eq!(
            hits.iter().map(|h| h.start + skip).collect::<Vec<_>>(),
            expected.iter().map(|h| h.offset).collect::<Vec<_>>()
        );
        assert_eq!(find_all_backward(&file, &pattern), hits);
    }
    for hit in &expected {
        assert_eq!(file.line_of_offset(hit.offset).unwrap(), Some(hit.line - 1));
        let start = file.line_start(hit.line - 1).unwrap().unwrap();
        let row = file.row_at(start).unwrap();
        assert!(file
            .row_text(&row)
            .unwrap()
            .starts_with(&format!("{:08} 検索{}", hit.line, hit.needle)));
    }
    // 表示上の行は文字の途中で分かれない（小さな区切りで全体を確かめる）。
    let config = BigFileConfig {
        segment_bytes: 48,
        ..BigFileConfig::default()
    };
    let small = BigFile::open(fx.path(name), config).unwrap();
    let mut offset = 0;
    let mut split = 0;
    loop {
        let rows = small.rows_forward(offset, 500).unwrap();
        for row in &rows {
            let bytes = small.read_range(row.range(), 200).unwrap();
            assert!(std::str::from_utf8(&bytes).is_ok(), "{row:?}");
            split += !row.ends_line as u32;
        }
        let last = rows.last().unwrap();
        if last.last {
            break;
        }
        offset = last.next;
    }
    assert!(split > 1000);
    within_budget(&small);
}

#[test]
fn a_running_search_stops_soon_after_cancel() {
    let fx = fixtures();
    let file = open(&fx.path("lines.txt"));
    for pattern in [
        SearchPattern::literal("no such text anywhere", true).unwrap(),
        SearchPattern::regex("no such text [a-z]+where", true).unwrap(),
    ] {
        for direction in [Direction::Forward, Direction::Backward] {
            // 最初の進捗の報告（4 MiB 進んだところ）で中止する。
            let cancel = CancelToken::new();
            let sink = {
                let cancel = cancel.clone();
                move |_: Progress| cancel.cancel()
            };
            let ctx = TaskContext::new(cancel, Arc::new(sink), Limits::UNLIMITED);
            let from = match direction {
                Direction::Forward => 0,
                Direction::Backward => file.len(),
            };
            let outcome = file.find(&pattern, from, direction, &ctx).unwrap();
            assert_eq!(outcome.completion, Completion::Cancelled);
            assert_eq!(outcome.value.hit, None);
            let scanned = outcome.value.stopped_at.abs_diff(from);
            assert!(
                (4 << 20..5 << 20).contains(&scanned),
                "{direction:?}: {scanned} バイト調べて止まった"
            );
        }
    }
}

#[test]
fn a_second_index_build_is_refused_while_one_is_running() {
    let fx = fixtures();
    let file = Arc::new(open(&fx.path("lines.txt")));
    let (started_tx, started_rx) = std::sync::mpsc::channel();
    let (resume_tx, resume_rx) = std::sync::mpsc::channel::<()>();
    let resume_rx = std::sync::Mutex::new(resume_rx);
    let sink = move |p: Progress| {
        // 最初の報告で止まり、合図を待つ。
        if p.done == INDEX_CHUNK_BYTES as u64 {
            started_tx.send(()).unwrap();
            resume_rx.lock().unwrap().recv().unwrap();
        }
    };
    let ctx = TaskContext::new(CancelToken::new(), Arc::new(sink), Limits::UNLIMITED);
    let worker = {
        let file = Arc::clone(&file);
        std::thread::spawn(move || file.build_index(&ctx))
    };
    started_rx.recv().unwrap();
    assert!(matches!(
        file.build_index(&TaskContext::detached()),
        Err(Error::Busy { .. })
    ));
    // 作成中でも、済んだ範囲は読める。
    assert_eq!(file.line_start(100).unwrap(), Some(8000));
    assert!(matches!(file.line_count(), LineCount::AtLeast(n) if n > 13_000));
    resume_tx.send(()).unwrap();
    worker.join().unwrap().unwrap();
    assert!(file.index_status().complete);
}

#[test]
fn truncating_a_fixture_while_indexing_stops_the_index() {
    let fx = fixtures();
    let dir = std::env::temp_dir().join(format!("linote-bigfile-fx-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("lines-copy.txt");
    std::fs::copy(fx.path("lines.txt"), &path).unwrap();
    let file = Arc::new(BigFile::open(&path, BigFileConfig::default()).unwrap());

    // 索引作成の途中（最初の報告のとき）に、他のアプリが切り詰める。
    let sink = {
        let path = path.clone();
        move |p: Progress| {
            if p.done == INDEX_CHUNK_BYTES as u64 {
                let writer = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
                writer.set_len(1000).unwrap();
            }
        }
    };
    let ctx = TaskContext::new(CancelToken::new(), Arc::new(sink), Limits::UNLIMITED);
    let result = file.build_index(&ctx);
    assert!(matches!(result, Err(Error::ExternalChange { .. })), "{result:?}");
    assert!(file.is_invalidated());
    let usage = file.memory_usage();
    assert_eq!((usage.cache_bytes, usage.index_bytes), (0, 0));
    assert!(matches!(file.line_start(10), Err(Error::ExternalChange { .. })));

    let reopened = file.reopen().unwrap();
    reopened.build_index(&TaskContext::detached()).unwrap();
    assert_eq!(reopened.len(), 1000);
    assert_eq!(reopened.line_count(), LineCount::Exact(13));
    drop((file, reopened));
    let _ = std::fs::remove_dir_all(&dir);
}

/// 1 GiB 級のデータでの計測。手順はこのファイルの先頭の説明を参照。
#[test]
#[ignore = "1 GiB 級のデータが要る（scripts/gen-large-fixtures.py）"]
fn measure_large_fixtures() {
    let dir = std::env::var_os("LINOTE_BIGFILE_LARGE_FIXTURES")
        .map(PathBuf::from)
        .unwrap_or_else(|| Path::new("target").join("fixtures-large"));
    let fx = Fixtures::load(&dir).expect("target/fixtures-large がない");
    let ms = |t: Instant| t.elapsed().as_secs_f64() * 1000.0;
    for name in ["lines.txt", "longline.txt", "mixed.txt"] {
        let hits = fx.hits(name);
        println!("== {name}（{} バイト）", fx.number(name, "bytes"));

        let t = Instant::now();
        let file = Arc::new(open(&fx.path(name)));
        let first = file.rows_forward(0, 100).unwrap();
        let texts: usize = first.iter().map(|r| file.row_text(r).unwrap().len()).sum();
        println!("開く + 先頭 100 行（{texts} バイト）: {:.2} ms", ms(t));

        let t = Instant::now();
        let tail = file.last_rows(100).unwrap();
        println!("末尾 100 行（索引なし）: {:.2} ms（{} 行）", ms(t), tail.len());

        let t = Instant::now();
        let middle = file.rows_forward(file.len() / 2, 100).unwrap();
        println!("中央 100 行（索引なし）: {:.2} ms（{} 行）", ms(t), middle.len());

        let t = Instant::now();
        file.build_index(&TaskContext::detached()).unwrap();
        let status = file.index_status();
        println!("索引作成: {:.0} ms（{} 行）", ms(t), status.lines);

        let t = Instant::now();
        let mut rng = 0x9E37_79B9_7F4A_7C15u64;
        for _ in 0..1000 {
            rng ^= rng << 13;
            rng ^= rng >> 7;
            rng ^= rng << 17;
            let line = rng % status.lines;
            let start = file.line_start(line).unwrap().unwrap();
            assert_eq!(file.line_of_offset(start).unwrap(), Some(line));
        }
        println!("行ジャンプ + 行番号の逆引き 1000 回: 平均 {:.3} ms", ms(t) / 1000.0);

        let last = hits.last().unwrap();
        for (label, pattern) in [
            ("文字列", SearchPattern::literal(&last.needle, true).unwrap()),
            ("文字列・大小無視", SearchPattern::literal(&last.needle, false).unwrap()),
            ("正規表現", SearchPattern::regex(&last.needle, true).unwrap()),
            ("正規表現・大小無視", SearchPattern::regex(&last.needle, false).unwrap()),
        ] {
            let t = Instant::now();
            let outcome = file
                .find(&pattern, 0, Direction::Forward, &TaskContext::detached())
                .unwrap();
            let hit = outcome.value.hit.expect("ヒットがない");
            println!(
                "検索（{label}、先頭から {} バイト先のヒットまで）: {:.0} ms{}",
                hit.start,
                ms(t),
                if outcome.value.notes.split_long_line {
                    "（長大行を分けて探した）"
                } else {
                    ""
                }
            );
            let t = Instant::now();
            let back = file
                .find(&pattern, file.len(), Direction::Backward, &TaskContext::detached())
                .unwrap();
            assert_eq!(back.value.hit, Some(hit));
            println!("検索（{label}、末尾から後方へ）: {:.1} ms", ms(t));
        }

        // 最初のヒット（先頭付近）と、中止の反応。
        let first_hit = SearchPattern::regex(r"LINOTE_NEEDLE_[A-Z]+_\d", true).unwrap();
        let t = Instant::now();
        let outcome = file
            .find(&first_hit, 0, Direction::Forward, &TaskContext::detached())
            .unwrap();
        println!(
            "検索（正規表現、最初のヒット {:?}）: {:.2} ms",
            outcome.value.hit.map(|h| h.start),
            ms(t)
        );
        let cancel = CancelToken::new();
        let ctx = TaskContext::new(cancel.clone(), Arc::new(crate::core::NoProgress), Limits::UNLIMITED);
        let searcher = {
            let file = Arc::clone(&file);
            std::thread::spawn(move || {
                let nothing = SearchPattern::regex("no such text [a-z]+where", false).unwrap();
                let t = Instant::now();
                let outcome = file.find(&nothing, 0, Direction::Forward, &ctx).unwrap();
                (outcome.completion, outcome.value.stopped_at, t.elapsed())
            })
        };
        std::thread::sleep(std::time::Duration::from_millis(100));
        let t = Instant::now();
        cancel.cancel();
        let (completion, stopped_at, ran) = searcher.join().unwrap();
        println!(
            "検索の中止: 合図から {:.2} ms で終了（{completion:?}、{stopped_at} バイトまで、開始から {:.0} ms）",
            ms(t),
            ran.as_secs_f64() * 1000.0
        );

        let usage = file.memory_usage();
        println!(
            "メモリ: キャッシュ {} KiB、索引 {} KiB",
            usage.cache_bytes / 1024,
            usage.index_bytes / 1024
        );
        within_budget(&file);
    }
}
