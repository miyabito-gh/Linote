//! 復元データの、プロセスをまたぐ動作（タスク D）。
//!
//! 子プロセスとして、このテスト実行ファイル自身を `child_*` のテストを指定して起動する
//! （子の役目は環境変数 `LINOTE_TEST_ROOT` があるときだけ動く）。

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use linote::appdata::AppPaths;
use linote::core::{TaskContext, Version};
use linote::recovery::{RecoveryKind, RecoveryStore, Snapshot, WriteOutcome};
use linote::text::TextFormat;

const ENV_ROOT: &str = "LINOTE_TEST_ROOT";
const ENV_TAG: &str = "LINOTE_TEST_TAG";

fn temp_dir(tag: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!("linote-it-{tag}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&path);
    fs::create_dir_all(&path).unwrap();
    path
}

fn run_child(test: &str, root: &Path, tag: &str) -> std::process::Child {
    Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            test,
            "--ignored",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(ENV_ROOT, root)
        .env(ENV_TAG, tag)
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap()
}

fn finish(child: std::process::Child) -> String {
    String::from_utf8_lossy(&child.wait_with_output().unwrap().stdout).into_owned()
}

fn snapshot(version: u64, text: String) -> Snapshot {
    Snapshot {
        kind: RecoveryKind::File,
        source_path: Some(PathBuf::from("C:\\shared\\same.txt")),
        format: TextFormat::DEFAULT,
        file_stamp: None,
        title: "same.txt".into(),
        version: Version::from_raw(version),
        text,
    }
}

fn store(root: &Path) -> RecoveryStore {
    RecoveryStore::new(&AppPaths::with_root(root))
}

/// 書き出してから、後始末なしで終了する（強制終了の模擬）。書き込み途中の一時ファイルも残す。
#[test]
#[ignore = "子プロセス用"]
fn child_write_and_die() {
    let (Some(root), Ok(tag)) = (std::env::var_os(ENV_ROOT), std::env::var(ENV_TAG)) else {
        return;
    };
    let s = store(Path::new(&root));
    let slot = s.open_slot().unwrap();
    slot.write(
        &snapshot(1, format!("crashed {tag}")),
        &TaskContext::detached(),
    )
    .unwrap();
    fs::write(
        s.dir()
            .join(format!(".~linote-{}-0-0.tmp", std::process::id())),
        b"{\"par",
    )
    .unwrap();
    println!("RESULT=WROTE");
    std::mem::forget(slot);
    std::process::exit(0);
}

/// 同じ文書を開いた別ウィンドウ。自分の枠にだけ、版を進めながら書き続ける。
#[test]
#[ignore = "子プロセス用"]
fn child_write_many() {
    let (Some(root), Ok(tag)) = (std::env::var_os(ENV_ROOT), std::env::var(ENV_TAG)) else {
        return;
    };
    let slot = store(Path::new(&root)).open_slot().unwrap();
    let ctx = TaskContext::detached();
    for v in 1..=60u64 {
        let text = format!("{tag}:{v}:{}", "x".repeat(2000));
        assert_eq!(
            slot.write(&snapshot(v, text), &ctx).unwrap(),
            WriteOutcome::Written
        );
    }
    println!("RESULT=DONE");
    // 落ちた扱いで手放す（データは残る）。
    slot.release_keeping_data();
}

/// 走査して、見えたものを報告する。
#[test]
#[ignore = "子プロセス用"]
fn child_scan() {
    let Some(root) = std::env::var_os(ENV_ROOT) else {
        return;
    };
    let r = store(Path::new(&root)).scan().unwrap();
    println!(
        "RESULT=candidates:{} live:{} quarantined:{}",
        r.candidates.len(),
        r.live.len(),
        r.quarantined.len()
    );
}

#[test]
fn crashed_process_leaves_an_orphan_that_is_restored_and_partial_writes_are_ignored() {
    let root = temp_dir("rec-crash");
    let out = finish(run_child("child_write_and_die", &root, "A"));
    assert!(out.contains("RESULT=WROTE"), "{out}");

    let s = store(&root);
    let mut r = s.scan().unwrap();
    assert!(r.quarantined.is_empty(), "{:?}", r.quarantined);
    assert!(r.live.is_empty());
    assert_eq!(r.candidates.len(), 1);
    let c = r.candidates.pop().unwrap();
    assert_eq!(c.read_text().unwrap(), "crashed A");
    assert_eq!(
        c.meta.source_path.as_deref(),
        Some(Path::new("C:\\shared\\same.txt"))
    );
    c.discard().unwrap();
    fs::remove_dir_all(&root).unwrap();
}

#[test]
fn live_process_is_not_an_orphan_for_another_process() {
    let root = temp_dir("rec-live");
    let s = store(&root);
    let slot = s.open_slot().unwrap();
    slot.write(&snapshot(1, "mine".into()), &TaskContext::detached())
        .unwrap();

    let out = finish(run_child("child_scan", &root, ""));
    assert!(
        out.contains("RESULT=candidates:0 live:1 quarantined:0"),
        "{out}"
    );
    // 別プロセスの走査は、データも所有も壊さない。
    slot.write(&snapshot(2, "mine2".into()), &TaskContext::detached())
        .unwrap();

    slot.release_keeping_data();
    let out = finish(run_child("child_scan", &root, ""));
    assert!(
        out.contains("RESULT=candidates:1 live:0 quarantined:0"),
        "{out}"
    );
    fs::remove_dir_all(&root).unwrap();
}

#[test]
fn concurrent_windows_and_scans_never_corrupt_or_overwrite() {
    let root = temp_dir("rec-concurrent");
    let children: Vec<_> = ["A", "B", "C"]
        .iter()
        .map(|t| run_child("child_write_many", &root, t))
        .collect();

    // 書き込みの最中に、走査を繰り返す（別プロセスの MCP を想定）。壊れたものは見えない。
    let s = store(&root);
    for _ in 0..100 {
        let r = s.scan().unwrap();
        assert!(r.quarantined.is_empty(), "{:?}", r.quarantined);
        assert!(r.warnings.is_empty(), "{:?}", r.warnings);
        // 引き取れたものは持ち主が落ちたもの（終了済みの子）。読めるはず。
        for c in &r.candidates {
            c.read_text().unwrap();
        }
    }
    for child in children {
        let out = finish(child);
        assert!(out.contains("RESULT=DONE"), "{out}");
    }

    let r = s.scan().unwrap();
    assert!(r.quarantined.is_empty());
    assert_eq!(r.candidates.len(), 3, "3 ウィンドウ分が別々に残る");
    let mut tags: Vec<String> = r
        .candidates
        .iter()
        .map(|c| {
            let text = c.read_text().unwrap();
            assert!(text.contains(":60:"), "最後の版が残る");
            text.split(':').next().unwrap().to_owned()
        })
        .collect();
    tags.sort();
    assert_eq!(tags, ["A", "B", "C"]);
    drop(r); // ロックを手放してから消す（Windows では開いたままだと消せない）。
    fs::remove_dir_all(&root).unwrap();
}
