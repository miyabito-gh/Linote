//! メモの保存・読み込み・検索・衝突検出。

use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};

use linote::appdata::AppPaths;
use linote::core::{Completion, Error, Limits, MemoId, TaskContext, Version};
use linote::memo::{MemoStore, SaveOutcome};

struct Tmp(PathBuf);

impl Tmp {
    fn new() -> Self {
        static N: AtomicU32 = AtomicU32::new(0);
        let p = std::env::temp_dir().join(format!(
            "linote-memo-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        Tmp(p)
    }
    fn store(&self) -> MemoStore {
        MemoStore::new(&AppPaths::with_root(&self.0))
    }
}

impl Drop for Tmp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn ctx() -> TaskContext {
    TaskContext::detached()
}

#[test]
fn nothing_is_created_until_first_save() {
    let tmp = Tmp::new();
    let store = tmp.store();
    assert!(!store.dir().exists());
    let listing = store.list(&ctx()).unwrap();
    assert!(listing.value.memos.is_empty());
    assert!(!store.dir().exists());
}

#[test]
fn create_load_roundtrip_utf8() {
    let tmp = Tmp::new();
    let store = tmp.store();
    let m = store
        .create("買い物 \"リスト\"", "牛乳\r\n卵\n\u{1F600}")
        .unwrap();
    assert_eq!(m.version, Version::INITIAL);
    assert_eq!(store.load(&m.id).unwrap(), m);
}

#[test]
fn update_bumps_version_and_rejects_stale() {
    let tmp = Tmp::new();
    let store = tmp.store();
    let m = store.create("a", "one").unwrap();
    let m2 = store.update(&m.id, m.version, "two").unwrap();
    assert_eq!(m2.version, m.version.next());
    // 同じ内容なら版は進まない
    let m3 = store.update(&m.id, m2.version, "two").unwrap();
    assert_eq!(m3.version, m2.version);
    // 古い版
    match store.update(&m.id, m.version, "three") {
        Err(Error::VersionMismatch {
            expected, actual, ..
        }) => {
            assert_eq!(expected, m.version);
            assert_eq!(actual, m2.version);
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(store.load(&m.id).unwrap().body, "two");
}

#[test]
fn conflicting_autosave_forks_instead_of_overwriting() {
    let tmp = Tmp::new();
    let a = tmp.store();
    let b = tmp.store(); // 別ウィンドウ・別プロセス相当
    let m = a.create("日記", "base").unwrap();
    let ma = match a.save_or_fork(&m.id, m.version, "from a").unwrap() {
        SaveOutcome::Saved(m) => m,
        other => panic!("{other:?}"),
    };
    match b.save_or_fork(&m.id, m.version, "from b").unwrap() {
        SaveOutcome::Forked {
            memo,
            original_actual,
        } => {
            assert_eq!(original_actual, ma.version);
            assert_eq!(memo.body, "from b");
            assert!(memo.name.starts_with("日記"));
            assert_ne!(memo.id, m.id);
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(a.load(&m.id).unwrap().body, "from a");
    assert_eq!(a.list(&ctx()).unwrap().value.memos.len(), 2);
}

#[test]
fn rename_and_delete_check_version() {
    let tmp = Tmp::new();
    let store = tmp.store();
    let m = store.create("x", "").unwrap();
    let r = store.rename(&m.id, m.version, "y").unwrap();
    assert_eq!(r.name, "y");
    assert!(matches!(
        store.delete(&m.id, m.version),
        Err(Error::VersionMismatch { .. })
    ));
    store.delete(&m.id, r.version).unwrap();
    assert!(matches!(store.load(&m.id), Err(Error::NotFound { .. })));
    assert!(matches!(
        store.delete(&m.id, r.version),
        Err(Error::NotFound { .. })
    ));
}

#[test]
fn list_skips_corrupt_and_temp_files_newest_first() {
    let tmp = Tmp::new();
    let store = tmp.store();
    let old = store.create("old", "").unwrap();
    std::thread::sleep(std::time::Duration::from_millis(5));
    let new = store.create("new", "").unwrap();
    std::fs::write(store.dir().join("broken.memo"), b"{not json").unwrap();
    std::fs::write(store.dir().join(".~linote-1-2-3.tmp"), b"x").unwrap();
    std::fs::write(store.dir().join("note.txt"), b"x").unwrap();
    let l = store.list(&ctx()).unwrap().value;
    assert_eq!(l.skipped, 1);
    let ids: Vec<_> = l.memos.iter().map(|m| m.id.clone()).collect();
    assert_eq!(ids, vec![new.id, old.id]);
}

#[test]
fn search_folds_case_width_and_kana_and_ands_terms() {
    let tmp = Tmp::new();
    let store = tmp.store();
    let a = store
        .create("Meeting", "ＡＢＣ の議事録\n次回は来週")
        .unwrap();
    store.create("料理", "かれーのレシピ").unwrap();
    let hit = |q: &str| -> Vec<MemoId> {
        store
            .search(q, &ctx())
            .unwrap()
            .value
            .into_iter()
            .map(|h| h.id)
            .collect()
    };
    assert_eq!(hit("abc"), vec![a.id.clone()]);
    assert_eq!(hit("MEETING 来週"), vec![a.id.clone()]);
    assert!(hit("meeting カレー").is_empty());
    assert_eq!(hit("カレー").len(), 1);
    assert_eq!(hit("").len(), 2);
    let h = &store.search("議事録", &ctx()).unwrap().value[0];
    assert!(!h.name_match);
    assert!(h.snippet.as_deref().unwrap().contains("議事録"));
}

#[test]
fn search_respects_limits_and_cancel() {
    let tmp = Tmp::new();
    let store = tmp.store();
    for i in 0..5 {
        store.create(&format!("n{i}"), "x").unwrap();
    }
    let limited = TaskContext::detached().with_limits(Limits::items(2));
    let out = store.search("", &limited).unwrap();
    assert_eq!(out.completion, Completion::ItemLimit);
    assert_eq!(out.value.len(), 2);

    let c = ctx();
    c.cancel.cancel();
    let out = store.search("", &c).unwrap();
    assert_eq!(out.completion, Completion::Cancelled);
}

#[test]
fn concurrent_updates_never_lose_a_write_silently() {
    let tmp = Tmp::new();
    let store = tmp.store();
    let m = store.create("c", "0").unwrap();
    let threads: Vec<_> = (0..4)
        .map(|t| {
            let store = store.clone();
            let id = m.id.clone();
            std::thread::spawn(move || {
                let mut won = 0;
                for i in 0..10 {
                    let cur = store.load(&id).unwrap();
                    if store.update(&id, cur.version, &format!("{t}-{i}")).is_ok() {
                        won += 1;
                    }
                }
                won
            })
        })
        .collect();
    let won: u64 = threads.into_iter().map(|t| t.join().unwrap()).sum();
    // 成功した更新の数だけ版が進む（二重に成功して上書きし合ったりしない）。
    assert_eq!(store.load(&m.id).unwrap().version.as_u64(), won);
}

#[test]
fn background_save_reports_result() {
    let tmp = Tmp::new();
    let store = tmp.store();
    let m = store.create("bg", "a").unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    store.save_in_background(m.id.clone(), m.version, "b".into(), move |r| {
        tx.send(r).unwrap();
    });
    let r = rx.recv_timeout(std::time::Duration::from_secs(10)).unwrap();
    assert!(matches!(r, Ok(SaveOutcome::Saved(ref s)) if s.body == "b"));
}
