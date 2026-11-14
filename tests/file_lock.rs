//! ロックファイルと原子的な書き込みの、プロセスをまたぐ動作（タスク A）。
//!
//! 子プロセスとして、このテスト実行ファイル自身を `child_*` のテストを指定して起動する
//! （子の役目は環境変数 `LINOTE_TEST_LOCK` があるときだけ動く）。

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use linote::core::TaskContext;
use linote::file::{is_temp_file_name, write_atomic, LockFile};

const ENV_LOCK: &str = "LINOTE_TEST_LOCK";
const ENV_DATA: &str = "LINOTE_TEST_DATA";

fn temp_dir(tag: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!("linote-it-{tag}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&path);
    fs::create_dir_all(&path).unwrap();
    path
}

/// 子プロセスで `test` を実行し、標準出力を返す。
fn run_child(test: &str, envs: &[(&str, &Path)]) -> String {
    let mut command = Command::new(std::env::current_exe().unwrap());
    command.args([
        "--exact",
        test,
        "--ignored",
        "--nocapture",
        "--test-threads=1",
    ]);
    for (key, value) in envs {
        command.env(key, value);
    }
    let output = command.output().unwrap();
    String::from_utf8_lossy(&output.stdout).into_owned()
}

#[test]
#[ignore = "子プロセス用"]
fn child_try_lock() {
    let Some(path) = std::env::var_os(ENV_LOCK) else {
        return;
    };
    match LockFile::try_acquire(Path::new(&path)) {
        Ok(_) => println!("RESULT=ACQUIRED"),
        Err(e) => println!("RESULT={}", e.code()),
    }
}

#[test]
#[ignore = "子プロセス用"]
fn child_lock_and_die() {
    let Some(path) = std::env::var_os(ENV_LOCK) else {
        return;
    };
    let lock = LockFile::try_acquire(Path::new(&path)).unwrap();
    println!("RESULT=ACQUIRED");
    // 解放せずに、後始末なしで終了する（強制終了の模擬）。
    std::mem::forget(lock);
    std::process::exit(0);
}

#[test]
#[ignore = "子プロセス用"]
fn child_write_under_lock() {
    let (Some(lock), Some(data)) = (std::env::var_os(ENV_LOCK), std::env::var_os(ENV_DATA)) else {
        return;
    };
    let ctx = TaskContext::detached();
    let pid = std::process::id();
    let mut done = 0;
    let mut attempts = 0;
    // ロックを取れたときだけ「読んで、数を足して、書く」。取れなければやり直す。
    while done < 20 && attempts < 20_000 {
        attempts += 1;
        let Ok(guard) = LockFile::try_acquire(Path::new(&lock)) else {
            std::thread::sleep(std::time::Duration::from_millis(1));
            continue;
        };
        let current: u64 = fs::read_to_string(&data).unwrap().trim().parse().unwrap();
        write_atomic(
            Path::new(&data),
            format!("{}\n", current + 1).as_bytes(),
            &ctx,
        )
        .unwrap();
        drop(guard);
        done += 1;
    }
    println!("RESULT=done:{done} pid:{pid}");
}

#[test]
fn lock_excludes_other_processes_and_dies_with_its_owner() {
    let dir = temp_dir("lock-proc");
    let path = dir.join(".lock");

    // 自分が持っている間、他のプロセスは取れない。
    let mine = LockFile::try_acquire(&path).unwrap();
    let out = run_child("child_try_lock", &[(ENV_LOCK, &path)]);
    assert!(out.contains("RESULT=busy"), "{out}");

    // 離せば取れる。
    drop(mine);
    let out = run_child("child_try_lock", &[(ENV_LOCK, &path)]);
    assert!(out.contains("RESULT=ACQUIRED"), "{out}");

    // 持ったまま終了したプロセスのロックは、OS が解放する。
    let out = run_child("child_lock_and_die", &[(ENV_LOCK, &path)]);
    assert!(out.contains("RESULT=ACQUIRED"), "{out}");
    assert!(path.exists());
    LockFile::try_acquire(&path).unwrap();

    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn locked_read_modify_write_from_several_processes_loses_nothing() {
    let dir = temp_dir("lock-rmw");
    let lock = dir.join(".lock");
    let data = dir.join("counter.txt");
    write_atomic(&data, b"0\n", &TaskContext::detached()).unwrap();

    let children: Vec<_> = (0..3)
        .map(|_| {
            let (lock, data) = (lock.clone(), data.clone());
            std::thread::spawn(move || {
                run_child(
                    "child_write_under_lock",
                    &[(ENV_LOCK, &lock), (ENV_DATA, &data)],
                )
            })
        })
        .collect();
    for child in children {
        let out = child.join().unwrap();
        assert!(out.contains("RESULT=done:20"), "{out}");
    }

    // 3 プロセス × 20 回の更新が一つも失われていない。
    assert_eq!(fs::read_to_string(&data).unwrap(), "60\n");
    // 一時ファイルが残っていない。
    for entry in fs::read_dir(&dir).unwrap() {
        assert!(!is_temp_file_name(&entry.unwrap().file_name()));
    }
    fs::remove_dir_all(&dir).unwrap();
}
