use super::*;
use crate::file::testutil::TempDir;
use crate::text::TextFormat;

use super::super::format::RecoveryKind;

fn snap(version: u64, text: &str) -> Snapshot {
    Snapshot {
        kind: RecoveryKind::File,
        source_path: Some(PathBuf::from("C:\\a.txt")),
        format: TextFormat::DEFAULT,
        file_stamp: None,
        title: "a.txt".into(),
        version: Version::from_raw(version),
        text: text.into(),
    }
}

fn store(dir: &TempDir) -> RecoveryStore {
    RecoveryStore::new(&AppPaths::with_root(dir.path()))
}

fn ctx() -> TaskContext {
    TaskContext::detached()
}

#[test]
fn nothing_is_created_until_first_write() {
    let dir = TempDir::new("rec-lazy");
    let s = store(&dir);
    assert!(s.scan().unwrap().candidates.is_empty());
    assert!(dir.entries().is_empty());
}

#[test]
fn orphan_is_found_and_restorable_then_adopted() {
    let dir = TempDir::new("rec-orphan");
    let s = store(&dir);
    let slot = s.open_slot().unwrap();
    slot.write(&snap(3, "hello"), &ctx()).unwrap();

    // 生きている間は候補にならない。
    let r = s.scan().unwrap();
    assert!(r.candidates.is_empty());
    assert_eq!(r.live.len(), 1);
    assert_eq!(r.live[0].meta.as_ref().unwrap().title, "a.txt");

    // 持ち主が手放すと（落ちると）孤児になる。
    slot.release_keeping_data();
    let mut r = s.scan().unwrap();
    assert_eq!(r.candidates.len(), 1);
    let c = r.candidates.pop().unwrap();
    assert_eq!(c.read_text().unwrap(), "hello");
    assert_eq!(c.meta.doc_version, Version::from_raw(3));

    // 引き取っている間は、別の走査には候補として見えない。
    let r2 = s.scan().unwrap();
    assert!(r2.candidates.is_empty());
    assert_eq!(r2.live.len(), 1);

    // 引き継ぎ→保存で削除。
    let slot = c.into_slot();
    slot.write(&snap(1, "hello!"), &ctx()).unwrap();
    slot.saved(Version::from_raw(1)).unwrap();
    assert!(s.scan().unwrap().live.is_empty());
    slot.discard().unwrap();
    assert!(
        dir.path()
            .join("recovery")
            .read_dir()
            .unwrap()
            .next()
            .is_none(),
        "{:?}",
        dir.entries()
    );
}

#[test]
fn dropped_candidate_stays_orphan() {
    let dir = TempDir::new("rec-drop");
    let s = store(&dir);
    let slot = s.open_slot().unwrap();
    slot.write(&snap(1, "x"), &ctx()).unwrap();
    slot.release_keeping_data();
    drop(s.scan().unwrap());
    assert_eq!(s.scan().unwrap().candidates.len(), 1);
}

#[test]
fn stale_writes_never_overwrite_newer() {
    let dir = TempDir::new("rec-stale");
    let s = store(&dir);
    let slot = s.open_slot().unwrap();
    assert_eq!(
        slot.write(&snap(5, "new"), &ctx()).unwrap(),
        WriteOutcome::Written
    );
    assert_eq!(
        slot.write(&snap(4, "old"), &ctx()).unwrap(),
        WriteOutcome::Stale
    );
    assert_eq!(
        slot.write(&snap(5, "new2"), &ctx()).unwrap(),
        WriteOutcome::Written
    );
    slot.release_keeping_data();
    let c = s.scan().unwrap().candidates.pop().unwrap();
    assert_eq!(c.read_text().unwrap(), "new2");
}

#[test]
fn saved_keeps_newer_data_and_blocks_older_writes() {
    let dir = TempDir::new("rec-saved");
    let s = store(&dir);
    let slot = s.open_slot().unwrap();
    slot.write(&snap(7, "edited during save"), &ctx()).unwrap();
    slot.saved(Version::from_raw(6)).unwrap();
    assert_eq!(s.scan().unwrap().live.len(), 1, "新しい分は残る");
    slot.saved(Version::from_raw(7)).unwrap();
    assert!(s.scan().unwrap().live.is_empty());
    // 保存済みの版以前の遅れた書き出しは捨てる。新しい版は書ける。
    assert_eq!(
        slot.write(&snap(7, "late"), &ctx()).unwrap(),
        WriteOutcome::Stale
    );
    assert_eq!(
        slot.write(&snap(8, "more"), &ctx()).unwrap(),
        WriteOutcome::Written
    );
    assert_eq!(s.scan().unwrap().live.len(), 1);
}

#[test]
fn two_slots_for_the_same_document_do_not_overwrite_each_other() {
    let dir = TempDir::new("rec-two");
    let s = store(&dir);
    let a = s.open_slot().unwrap();
    let b = s.open_slot().unwrap();
    assert_ne!(a.id(), b.id());
    a.write(&snap(1, "from a"), &ctx()).unwrap();
    b.write(&snap(1, "from b"), &ctx()).unwrap();
    a.write(&snap(2, "from a 2"), &ctx()).unwrap();
    a.release_keeping_data();
    b.release_keeping_data();
    let mut texts: Vec<String> = s
        .scan()
        .unwrap()
        .candidates
        .iter()
        .map(|c| c.read_text().unwrap())
        .collect();
    texts.sort();
    assert_eq!(texts, ["from a 2", "from b"]);
}

#[test]
fn corrupt_and_unknown_version_are_quarantined_not_deleted() {
    let dir = TempDir::new("rec-quarantine");
    let s = store(&dir);
    let slot = s.open_slot().unwrap();
    slot.write(&snap(1, "good"), &ctx()).unwrap();
    slot.release_keeping_data();
    let good = s.scan().unwrap().candidates.pop().unwrap();
    let rec = data_path(s.dir(), &good.meta.id);
    let bytes = fs::read(&rec).unwrap();
    drop(good);

    // 途中で切れた（検査が合わない）。
    let bad = RecoveryId::parse("1-1-1").unwrap();
    fs::write(data_path(s.dir(), &bad), &bytes[..bytes.len() - 2]).unwrap();
    // 未知の版。
    let newer = RecoveryId::parse("1-1-2").unwrap();
    let text = String::from_utf8(bytes)
        .unwrap()
        .replacen("\"version\":1", "\"version\":9", 1);
    fs::write(data_path(s.dir(), &newer), text).unwrap();
    // 無関係なファイルは触らない。
    fs::write(s.dir().join("notes.txt"), "x").unwrap();

    let r = s.scan().unwrap();
    assert_eq!(r.candidates.len(), 1);
    assert_eq!(r.quarantined.len(), 2);
    let by_id = |id: &RecoveryId| r.quarantined.iter().find(|q| &q.id == id).unwrap();
    assert!(matches!(by_id(&bad).reason, QuarantineReason::Corrupt(_)));
    assert_eq!(by_id(&newer).reason, QuarantineReason::UnknownVersion(9));
    for q in &r.quarantined {
        assert!(q.path.exists());
        assert!(!data_path(s.dir(), &q.id).exists());
    }
    assert!(s.dir().join("notes.txt").exists());
    // 次の走査では報告されない（隔離済み）。
    drop(r);
    let r = s.scan().unwrap();
    assert!(r.quarantined.is_empty());
    assert_eq!(r.candidates.len(), 1);
}

#[test]
fn leftovers_of_a_crashed_write_are_ignored() {
    let dir = TempDir::new("rec-leftover");
    let s = store(&dir);
    let slot = s.open_slot().unwrap();
    slot.write(&snap(1, "complete"), &ctx()).unwrap();
    // 書き込み途中で落ちた一時ファイル（新しい）と、書き出し前に落ちたロックだけの残り。
    fs::write(s.dir().join(".~linote-1-2-3.tmp"), b"{\"half").unwrap();
    fs::write(s.dir().join("zz-1-0.lock"), b"").unwrap();
    fs::write(s.dir().join("zz-1-1.lock"), b"").unwrap();
    let old = SystemTime::now() - Duration::from_secs(2 * 3600);
    fs::File::options()
        .write(true)
        .open(s.dir().join("zz-1-0.lock"))
        .unwrap()
        .set_modified(old)
        .unwrap();
    slot.release_keeping_data();

    let r = s.scan().unwrap();
    assert_eq!(r.candidates.len(), 1);
    assert!(r.quarantined.is_empty());
    assert_eq!(r.candidates[0].read_text().unwrap(), "complete");
    assert!(
        !s.dir().join("zz-1-0.lock").exists(),
        "孤立したロックは掃除する"
    );
    assert!(
        s.dir().join("zz-1-1.lock").exists(),
        "新しいロックファイルは触らない"
    );
    assert!(
        s.dir().join(".~linote-1-2-3.tmp").exists(),
        "新しい一時ファイルは触らない"
    );
}

#[test]
fn discard_removes_data_and_lock() {
    let dir = TempDir::new("rec-discard");
    let s = store(&dir);
    let slot = s.open_slot().unwrap();
    slot.write(&snap(1, "x"), &ctx()).unwrap();
    slot.discard().unwrap();
    assert!(dir
        .path()
        .join("recovery")
        .read_dir()
        .unwrap()
        .next()
        .is_none());
}

#[test]
fn candidate_discard_removes_it() {
    let dir = TempDir::new("rec-cdiscard");
    let s = store(&dir);
    let slot = s.open_slot().unwrap();
    slot.write(&snap(1, "x"), &ctx()).unwrap();
    slot.release_keeping_data();
    s.scan()
        .unwrap()
        .candidates
        .pop()
        .unwrap()
        .discard()
        .unwrap();
    let r = s.scan().unwrap();
    assert!(r.candidates.is_empty() && r.live.is_empty());
}
