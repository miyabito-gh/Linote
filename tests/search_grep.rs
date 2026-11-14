//! Grep の試験: 文字コード混在のフォルダー、除外、件数上限、中止、無効なパターン。

use std::fs;
use std::path::PathBuf;

use linote::core::{Completion, Error, Limits, TaskContext};
use linote::search::{grep_collect, GrepOptions, SearchOptions};

fn temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("linote-grep-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn utf16le(s: &str) -> Vec<u8> {
    let mut v = vec![0xFF, 0xFE];
    for u in s.encode_utf16() {
        v.extend(u.to_le_bytes());
    }
    v
}

fn cp932(s: &str) -> Vec<u8> {
    encoding_rs::SHIFT_JIS.encode(s).0.into_owned()
}

fn opts(root: &PathBuf, q: &str) -> GrepOptions {
    GrepOptions::new(root, SearchOptions::literal(q))
}

#[test]
fn finds_across_encodings_and_skips_excluded() {
    let dir = temp_dir("enc");
    fs::write(dir.join("a_utf8.txt"), "first\nこんにちは 世界\nlast").unwrap();
    fs::write(dir.join("b_utf16.txt"), utf16le("x\r\n世界 です\r\n")).unwrap();
    fs::write(dir.join("c_cp932.txt"), cp932("日本語の世界\n")).unwrap();
    fs::write(dir.join("d.bin"), b"\x00\x01\x02 world \x00").unwrap();
    fs::create_dir_all(dir.join(".git")).unwrap();
    fs::write(dir.join(".git/config"), "世界").unwrap();
    fs::create_dir_all(dir.join(".hidden")).unwrap();
    fs::write(dir.join(".hidden/h.txt"), "世界").unwrap();
    fs::create_dir_all(dir.join("sub")).unwrap();
    fs::write(dir.join("sub/e.txt"), "no match\n  世界\n").unwrap();

    let out = grep_collect(&opts(&dir, "世界"), &TaskContext::detached()).unwrap();
    assert_eq!(out.completion, Completion::Complete);
    let (summary, files) = out.value;
    let names: Vec<String> = files
        .iter()
        .map(|f| f.path.file_name().unwrap().to_string_lossy().into_owned())
        .collect();
    assert_eq!(names, ["a_utf8.txt", "b_utf16.txt", "c_cp932.txt", "e.txt"]);
    assert_eq!(summary.hits, 4);
    assert_eq!(summary.files_skipped, 1, "バイナリ");

    let a = &files[0].hits[0];
    assert_eq!((a.line, a.column), (2, 7));
    assert_eq!(a.text, "こんにちは 世界");
    assert_eq!(a.ranges, vec![(16, 22)]);
    assert_eq!(files[1].hits[0].line, 2);
    assert_eq!(files[1].hits[0].text, "世界 です");
    assert_eq!(files[1].hits[0].encoding, "utf-16le");
    assert_eq!(files[2].hits[0].encoding, "cp932");
    assert_eq!(files[3].hits[0].column, 3);

    let json = a.to_json();
    assert_eq!(json["line"], 2);
    assert_eq!(json["ranges"][0][0], 16);

    // 隠しフォルダーも対象にできる。
    let mut o = opts(&dir, "世界");
    o.include_hidden = true;
    o.exclude_dirs.clear();
    let out = grep_collect(&o, &TaskContext::detached()).unwrap();
    assert_eq!(out.value.0.hits, 6);
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn limits_large_files_extensions_and_hit_cap() {
    let dir = temp_dir("limits");
    fs::write(dir.join("many.txt"), "hit\n".repeat(50)).unwrap();
    fs::write(dir.join("big.txt"), "hit ".repeat(1000)).unwrap();
    fs::write(dir.join("other.md"), "hit").unwrap();

    let mut o = opts(&dir, "hit");
    o.max_file_bytes = 1000;
    o.extensions = vec!["txt".into()];
    let out = grep_collect(&o, &TaskContext::detached()).unwrap();
    assert_eq!(out.value.0.files_skipped, 1);
    assert_eq!(out.value.0.hits, 50);

    let ctx = TaskContext::detached().with_limits(Limits::items(10));
    let out = grep_collect(&opts(&dir, "hit"), &ctx).unwrap();
    assert_eq!(out.completion, Completion::ItemLimit);
    assert_eq!(out.value.0.hits, 10);
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn cancel_and_invalid_pattern_and_missing_root() {
    let dir = temp_dir("cancel");
    fs::write(dir.join("a.txt"), "hit").unwrap();
    let ctx = TaskContext::detached();
    ctx.cancel.cancel();
    let out = grep_collect(&opts(&dir, "hit"), &ctx).unwrap();
    assert_eq!(out.completion, Completion::Cancelled);
    assert_eq!(out.value.0.hits, 0);

    let bad = GrepOptions::new(
        &dir,
        SearchOptions {
            query: "(".into(),
            regex: true,
            ..Default::default()
        },
    );
    let e = grep_collect(&bad, &TaskContext::detached()).unwrap_err();
    assert!(matches!(e, Error::InvalidArgument { .. }));

    let e = grep_collect(&opts(&dir.join("nope"), "x"), &TaskContext::detached()).unwrap_err();
    assert!(matches!(e, Error::Io { .. }));
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn long_lines_are_excerpted_around_the_match() {
    let dir = temp_dir("long");
    let line = format!("{}needle{}", "a".repeat(2000), "b".repeat(2000));
    fs::write(dir.join("l.txt"), &line).unwrap();
    let out = grep_collect(&opts(&dir, "needle"), &TaskContext::detached()).unwrap();
    let hit = &out.value.1[0].hits[0];
    assert_eq!(hit.column, 2001);
    assert!(hit.text.len() <= 240);
    let (s, e) = hit.ranges[0];
    assert_eq!(&hit.text[s..e], "needle");
    let _ = fs::remove_dir_all(&dir);
}
