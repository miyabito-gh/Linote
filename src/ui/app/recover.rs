//! 未保存の変更の復元（タスク D の画面側のつなぎ込み）。
//!
//! - 文書ごとに復元データの枠（[`RecoverySlot`]）を持つ。初めて書くときに開く
//!   （起動の経路では何もしない）。
//! - 編集があったら「書き出していない編集」の時刻を控え、タイマー
//!   （[`super::TIMER_AUTOSAVE`]）が [`RecoveryPolicy::next_due`] に従ってワーカーに書かせる。
//!   古い版の書き出しが新しい版を上書きしないことは枠が保証する。
//! - 保存の成功で `saved(版)`、文書を閉じる・「保存しない」で破棄する。
//! - 起動後にワーカーで [`RecoveryStore::scan`] し、孤児を見つけたら復元するか尋ねる。
//!   復元した文書は元ファイルに触れない（利用者が保存したときだけ書く）。復元時に元ファイルの
//!   状態を確かめ、変わっていれば上書き保存を止める。

use std::sync::Arc;
use std::time::{Duration, Instant};

use windows::Win32::UI::WindowsAndMessaging::{KillTimer, SetTimer};

use crate::core::{DocumentId, Result, TaskContext, Version, WorkerPool};
use crate::document::{Capability, DocumentKind, DocumentState};
use crate::file::{check_external_change, FileStamp};
use crate::recovery::{
    Candidate, RecoveryKind, RecoveryPolicy, RecoverySlot, RecoveryStore, ScanReport, Snapshot,
};
use crate::ui::dialogs::RestoreChoice;
use crate::ui::editor::Language;
use crate::ui::statusbar::MessageKind;

use super::{file_title, App, UiMessage, AUTOSAVE_TICK_MS, TIMER_AUTOSAVE};

/// メモの自動保存の間隔の方針（復元データより短く）。
pub(super) const MEMO_POLICY: RecoveryPolicy = RecoveryPolicy {
    idle: Duration::from_millis(1500),
    max_interval: Duration::from_secs(10),
};

/// 復元データの枠を手放し、ファイルを消す（ワーカーで。書き出しが進行中なら終わるのを待つ）。
pub(super) fn discard_slot(slot: Arc<RecoverySlot>) {
    WorkerPool::global().spawn(move || {
        let mut slot = slot;
        for _ in 0..200 {
            match Arc::try_unwrap(slot) {
                Ok(s) => {
                    let _ = s.discard();
                    return;
                }
                Err(shared) => {
                    slot = shared;
                    std::thread::sleep(Duration::from_millis(20));
                }
            }
        }
    });
}

impl App {
    fn recovery_store(&self) -> Option<RecoveryStore> {
        self.paths.as_ref().map(RecoveryStore::new)
    }

    /// 文書の本文が変わった。書き出す必要があれば、書き出しの時刻を見るタイマーを動かす。
    pub(super) fn note_change(&mut self, id: DocumentId) {
        let available = self.paths.is_some();
        let Ok(i) = self.index_of(id) else { return };
        let d = &mut self.docs[i];
        let relevant = match d.state.kind() {
            DocumentKind::Memo(_) => d.memo.is_some(),
            _ => available && d.state.capabilities().contains(Capability::Recover),
        };
        if !relevant || !d.state.is_dirty() {
            return;
        }
        let now = Instant::now();
        d.pending = Some(match d.pending {
            Some((first, _)) => (first, now),
            None => (now, now),
        });
        self.arm_autosave_timer();
    }

    fn arm_autosave_timer(&mut self) {
        if !self.autosave_timer {
            self.autosave_timer = true;
            // SAFETY: 自分のウィンドウのタイマー。
            unsafe {
                SetTimer(Some(self.hwnd), TIMER_AUTOSAVE, AUTOSAVE_TICK_MS, None);
            }
        }
    }

    /// タイマーの刻み: 時刻が来た文書を書き出す。待つものがなくなったらタイマーを止める。
    pub(super) fn autosave_tick(&mut self, force: bool) {
        let now = Instant::now();
        let mut due: Vec<(DocumentId, bool)> = Vec::new();
        let mut waiting = false;
        for d in &self.docs {
            let Some((first, last)) = d.pending else {
                continue;
            };
            let is_memo = matches!(d.state.kind(), DocumentKind::Memo(_));
            let policy = if is_memo {
                MEMO_POLICY
            } else {
                RecoveryPolicy::default()
            };
            if (force || now >= policy.next_due(first, last)) && !d.write_in_flight {
                due.push((d.id(), is_memo));
            } else {
                waiting = true;
            }
        }
        for (id, is_memo) in due {
            if is_memo {
                self.autosave_memo(id);
            } else {
                self.write_recovery(id);
            }
        }
        if !waiting && self.autosave_timer {
            self.autosave_timer = false;
            // SAFETY: 自分のタイマーを止める。
            unsafe {
                let _ = KillTimer(Some(self.hwnd), TIMER_AUTOSAVE);
            }
        }
    }

    /// 書き出していない編集をすぐ書き出す（試験・終了前の確認用）。
    pub fn flush_pending_writes(&mut self) {
        self.autosave_tick(true);
    }

    /// 復元データを書き出す（本文の写しを作り、ワーカーで書く）。
    pub(super) fn write_recovery(&mut self, id: DocumentId) {
        let Some(store) = self.recovery_store() else {
            return;
        };
        let Ok(i) = self.index_of(id) else { return };
        if self.docs[i].slot.is_none() {
            match store.open_slot() {
                Ok(slot) => self.docs[i].slot = Some(Arc::new(slot)),
                Err(e) => {
                    self.docs[i].pending = None;
                    self.set_status(
                        &format!("復元データを用意できません: {e}"),
                        MessageKind::Warning,
                    );
                    return;
                }
            }
        }
        let (stamp, text) = match self.text_snapshot(id) {
            Ok(r) => r,
            Err(_) => return,
        };
        let d = &mut self.docs[i];
        let kind = match d.state.kind() {
            DocumentKind::Untitled => RecoveryKind::Untitled,
            DocumentKind::Clipboard => RecoveryKind::Clipboard,
            _ => RecoveryKind::File,
        };
        let snapshot = Snapshot {
            kind,
            source_path: d.state.path().map(std::path::Path::to_path_buf),
            format: d.state.format,
            file_stamp: d.state.disk_stamp(),
            lossy: d.lossy,
            title: d.title.clone(),
            version: stamp.version,
            text,
        };
        let slot = Arc::clone(d.slot.as_ref().expect("開いた"));
        d.pending = None;
        d.write_in_flight = true;
        let sender = self.sender();
        WorkerPool::global().spawn(move || {
            let result = slot.write(&snapshot, &TaskContext::detached());
            sender.send(UiMessage::Run(Box::new(move |app| {
                app.finish_recovery_write(id, result.map(|_| ()))
            })));
        });
    }

    fn finish_recovery_write(&mut self, id: DocumentId, result: Result<()>) {
        if let Ok(i) = self.index_of(id) {
            self.docs[i].write_in_flight = false;
        }
        if let Err(e) = result {
            self.set_status(
                &format!("復元データを書けませんでした: {e}"),
                MessageKind::Warning,
            );
        }
    }

    /// 元ファイルへの保存が成功した。保存した版までの復元データを消す。
    pub(super) fn recovery_saved(&mut self, id: DocumentId, version: Version) {
        let Ok(i) = self.index_of(id) else { return };
        let current = self.docs[i].state.version();
        if let Some(slot) = self.docs[i].slot.clone() {
            WorkerPool::global().spawn(move || {
                let _ = slot.saved(version);
            });
        }
        if version == current && !matches!(self.docs[i].state.kind(), DocumentKind::Memo(_)) {
            self.docs[i].pending = None;
        }
    }

    // ---------------------------------------------------------------- 起動時の走査

    /// ワーカーで復元データを探す。見つかったら（UI スレッドで）復元するか尋ねる。
    pub fn start_recovery_scan(&mut self) {
        let Some(store) = self.recovery_store() else {
            return;
        };
        let sender = self.sender();
        WorkerPool::global().spawn(move || {
            let report = store.scan();
            sender.send(UiMessage::Run(Box::new(move |app| match report {
                Ok(report) => app.offer_restore(report),
                Err(e) => app.set_status(
                    &format!("復元データを調べられませんでした: {e}"),
                    MessageKind::Warning,
                ),
            })));
        });
    }

    /// 見つかった復元データについて、復元するか利用者に尋ねる。
    pub fn offer_restore(&mut self, report: ScanReport) {
        if !report.quarantined.is_empty() {
            self.set_status(
                &format!(
                    "壊れた復元データ {} 件を、読み込まずに別の場所へ移しました",
                    report.quarantined.len()
                ),
                MessageKind::Warning,
            );
        }
        let candidates = report.candidates;
        if candidates.is_empty() {
            return;
        }
        let mut lines: Vec<String> = candidates
            .iter()
            .take(8)
            .map(|c| match &c.meta.source_path {
                Some(p) => format!("・{}（{}）", c.meta.title, p.display()),
                None => format!("・{}（未保存の文書）", c.meta.title),
            })
            .collect();
        if candidates.len() > 8 {
            lines.push(format!("・ほか {} 件", candidates.len() - 8));
        }
        let text = format!(
            "前回の終了時に保存されていない内容が {} 件残っています。\n\n{}",
            candidates.len(),
            lines.join("\n")
        );
        let owner = self.hwnd;
        let prompts = self.prompts.clone();
        match prompts.restore_choice(owner, &text) {
            RestoreChoice::Restore => {
                let mut first = None;
                for c in candidates {
                    match self.restore_candidate(c) {
                        Ok(id) => {
                            first.get_or_insert(id);
                        }
                        Err(e) => self.set_status(
                            &format!("復元できませんでした: {e}"),
                            MessageKind::Warning,
                        ),
                    }
                }
                if let Some(id) = first {
                    let _ = self.activate(id);
                }
            }
            RestoreChoice::Discard => {
                for c in candidates {
                    if let Err(e) = c.discard() {
                        self.set_status(
                            &format!("復元データを破棄できませんでした: {e}"),
                            MessageKind::Warning,
                        );
                    }
                }
            }
            // 捨てると所有を手放す（孤児のまま残り、次の起動でまた尋ねる）。
            RestoreChoice::Later => drop(candidates),
        }
    }

    /// 復元データ 1 件を、未保存の文書として開く。
    fn restore_candidate(&mut self, candidate: Candidate) -> Result<DocumentId> {
        let text = candidate.read_text()?;
        let meta = candidate.meta.clone();
        let mut note = None;
        let mut state = match (&meta.source_path, meta.kind) {
            (Some(path), _) => {
                let known = meta.file_stamp;
                let disk = known
                    .or_else(|| FileStamp::read(path).ok().flatten())
                    .unwrap_or(FileStamp {
                        len: 0,
                        modified: None,
                        id: None,
                    });
                let mut state = DocumentState::open_file(path.clone(), false, meta.format, disk);
                // 復元の前に、元ファイルが変わっていないか確かめる。
                if check_external_change(path, known).is_ok_and(|c| c.is_changed()) {
                    state.record_external_change();
                    note = Some(format!(
                        "「{}」の元のファイルは、復元データを作った後に変更されています。\
                         上書きせず、「名前を付けて保存」で別に保存してください",
                        meta.title
                    ));
                }
                state
            }
            (None, RecoveryKind::Clipboard) => DocumentState::clipboard(),
            (None, _) => DocumentState::untitled(),
        };
        state.format = meta.format;
        state.mark_unsaved();
        let language = meta
            .source_path
            .as_deref()
            .map(Language::from_path)
            .unwrap_or(Language::PlainText);
        let title = meta
            .source_path
            .as_deref()
            .map(file_title)
            .unwrap_or_else(|| meta.title.clone());
        let slot = candidate.into_slot();
        let id = self.open_text(state, &text, title, language, false)?;
        let i = self.index_of(id)?;
        self.docs[i].slot = Some(Arc::new(slot));
        self.docs[i].lossy = meta.lossy;
        if let Some(note) = note {
            self.set_status(&note, MessageKind::Warning);
        }
        Ok(id)
    }
}
