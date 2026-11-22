//! 未保存の変更の復元（タスク D の画面側のつなぎ込み）。
//!
//! - 文書ごとに復元データの枠（[`RecoverySlot`]）を持つ。初めて書くときに開く
//!   （起動の経路では何もしない）。
//! - 編集があったら「書き出していない編集」の時刻を控え、タイマー
//!   （[`super::TIMER_AUTOSAVE`]）が [`RecoveryPolicy::next_due`] に従ってワーカーに書かせる。
//!   古い版の書き出しが新しい版を上書きしないことは枠が保証する。
//! - 保存の成功で `saved(版)`、文書を閉じる・「保存しない」で破棄する。
//! - 起動後にワーカーで [`RecoveryStore::scan`] し、孤児を見つけたら件数を知らせる。
//!   利用者が復元を開いたときに改めて走査し、候補を選ぶ間だけ所有する。
//!   復元した文書は元ファイルに触れない（利用者が保存したときだけ書く）。復元時に元ファイルの
//!   状態を確かめ、変わっていれば上書き保存を止める。

use std::collections::HashMap;
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

#[derive(Default)]
pub(super) struct RecoveryUi {
    candidates: usize,
    scanning: bool,
    restoring: bool,
    failed: HashMap<DocumentId, String>,
}

struct RecoveryBatch {
    remaining: std::vec::IntoIter<Candidate>,
    first: Option<DocumentId>,
    active_before: Option<DocumentId>,
    failed: usize,
}

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

    pub(super) fn has_recovery_failures(&self) -> bool {
        !self.recovery_ui.failed.is_empty()
    }

    pub(super) fn has_recovery_candidates(&self) -> bool {
        self.recovery_ui.candidates != 0
    }

    pub(super) fn can_open_recovery_candidates(&self) -> bool {
        self.paths.is_some() && !self.recovery_ui.scanning && !self.recovery_ui.restoring
    }

    pub(super) fn persistent_recovery_status(&self) -> Option<(String, MessageKind)> {
        let failed = &self.recovery_ui.failed;
        if let Some((&id, reason)) = self
            .active_id()
            .and_then(|id| failed.get_key_value(&id))
            .or_else(|| failed.iter().next())
        {
            let title = self.document(id).map_or("文書", |doc| doc.title.as_str());
            let count = failed.len();
            Some((
                format!("復元データの保存に失敗（{count} 件）。「ファイル > 復元データの保存を再試行」。「{title}」: {reason}"),
                MessageKind::Warning,
            ))
        } else if self.has_recovery_candidates() {
            Some((
                format!(
                    "復元できる文書が {} 件あります。「ファイル > 復元できる文書…」",
                    self.recovery_ui.candidates
                ),
                MessageKind::Info,
            ))
        } else {
            None
        }
    }

    fn note_recovery_failure(&mut self, id: DocumentId, reason: String) {
        self.recovery_ui.failed.insert(id, reason);
        self.refresh_persistent_status();
    }

    pub(super) fn clear_recovery_failure(&mut self, id: DocumentId) {
        if self.recovery_ui.failed.remove(&id).is_some() {
            self.refresh_persistent_status();
        }
    }

    pub(super) fn retry_recovery_writes(&mut self) {
        let ids: Vec<_> = self
            .docs
            .iter()
            .filter(|d| d.state.is_dirty() && !d.write_in_flight)
            .map(|d| d.id())
            .filter(|id| self.recovery_ui.failed.contains_key(id))
            .collect();
        for id in ids {
            self.write_recovery(id);
        }
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
            self.clear_recovery_failure(id);
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
        if self.docs[i].write_in_flight {
            return;
        }
        if !self.docs[i].state.is_dirty() {
            self.docs[i].pending = None;
            return;
        }
        if self.docs[i].slot.is_none() {
            match store.open_slot() {
                Ok(slot) => self.docs[i].slot = Some(Arc::new(slot)),
                Err(e) => {
                    self.docs[i].pending = None;
                    self.note_recovery_failure(id, e.to_string());
                    return;
                }
            }
        }
        let (stamp, text) = match self.text_snapshot(id) {
            Ok(r) => r,
            Err(e) => {
                self.docs[i].pending = None;
                self.note_recovery_failure(id, e.to_string());
                return;
            }
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
        let Ok(i) = self.index_of(id) else { return };
        self.docs[i].write_in_flight = false;
        match result {
            Err(e) if self.docs[i].state.is_dirty() => {
                self.note_recovery_failure(id, e.to_string());
            }
            _ => self.clear_recovery_failure(id),
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
            self.clear_recovery_failure(id);
        }
    }

    // ---------------------------------------------------------------- 起動時の走査

    /// 起動時は候補の件数だけ知らせ、復元するかは利用者が開いたときに尋ねる。
    pub fn start_recovery_scan(&mut self) {
        if self.can_open_recovery_candidates() {
            self.scan_recovery(false);
        }
    }

    pub(super) fn open_recovery_candidates(&mut self) {
        if self.can_open_recovery_candidates() {
            self.scan_recovery(true);
        }
    }

    fn scan_recovery(&mut self, show_candidates: bool) {
        let Some(store) = self.recovery_store() else {
            return;
        };
        self.recovery_ui.scanning = true;
        let sender = self.sender();
        WorkerPool::global().spawn(move || {
            let result = store.scan();
            sender.send(UiMessage::Run(Box::new(move |app| {
                app.recovery_ui.scanning = false;
                match result {
                    Ok(report) => app.finish_recovery_scan(report, show_candidates),
                    Err(e) => app.set_status(
                        &format!("復元データを調べられませんでした: {e}"),
                        MessageKind::Warning,
                    ),
                }
            })));
        });
    }

    fn finish_recovery_scan(&mut self, report: ScanReport, show_candidates: bool) {
        self.recovery_ui.candidates = report.candidates.len();
        self.refresh_persistent_status();
        if !report.quarantined.is_empty() {
            self.set_status(
                &format!(
                    "壊れた復元データ {} 件を、読み込まずに別の場所へ移しました",
                    report.quarantined.len()
                ),
                MessageKind::Warning,
            );
        } else if !report.warnings.is_empty() {
            self.set_status(
                "一部の復元データを確認できませんでした",
                MessageKind::Warning,
            );
        } else if show_candidates && report.candidates.is_empty() {
            self.set_status("復元できる文書はありません", MessageKind::Info);
        }
        if show_candidates && !report.candidates.is_empty() {
            self.offer_restore(report.candidates);
        }
        // 起動時と閉じた後は所有を手放す。別のウィンドウからも復元できる。
    }

    fn offer_restore(&mut self, candidates: Vec<Candidate>) {
        let labels: Vec<_> = candidates
            .iter()
            .map(|c| match &c.meta.source_path {
                Some(p) => format!("{}（{}）", c.meta.title, p.display()),
                None => format!("{}（未保存の文書）", c.meta.title),
            })
            .collect();
        let owner = self.hwnd;
        let prompts = self.prompts.clone();
        let (choice, mut selected) = prompts.restore_selection(owner, &labels, &self.theme);
        if choice == RestoreChoice::Later {
            return;
        }
        selected.sort_unstable();
        let candidates: Vec<_> = candidates
            .into_iter()
            .enumerate()
            .filter(|(i, _)| selected.binary_search(i).is_ok())
            .map(|(_, c)| c)
            .collect();
        if candidates.is_empty() {
            return;
        }
        self.recovery_ui.restoring = true;
        self.recovery_ui.candidates -= candidates.len();
        self.refresh_persistent_status();
        match choice {
            RestoreChoice::Restore => {
                self.restore_next(RecoveryBatch {
                    remaining: candidates.into_iter(),
                    first: None,
                    active_before: self.active_id(),
                    failed: 0,
                });
            }
            RestoreChoice::Discard => {
                let sender = self.sender();
                WorkerPool::global().spawn(move || {
                    let failed = candidates
                        .into_iter()
                        .map(Candidate::discard)
                        .filter(Result::is_err)
                        .count();
                    sender.send(UiMessage::Run(Box::new(move |app| {
                        app.recovery_ui.restoring = false;
                        app.recovery_ui.candidates += failed;
                        app.refresh_persistent_status();
                        if failed != 0 {
                            app.set_status(
                                &format!("復元データを破棄できませんでした（{failed} 件）"),
                                MessageKind::Warning,
                            );
                        }
                    })));
                });
            }
            RestoreChoice::Later => {}
        }
    }

    fn restore_next(&mut self, mut batch: RecoveryBatch) {
        let Some(candidate) = batch.remaining.next() else {
            self.recovery_ui.restoring = false;
            self.recovery_ui.candidates += batch.failed;
            self.refresh_persistent_status();
            if self.active_id() == batch.active_before {
                if let Some(id) = batch.first {
                    let _ = self.activate(id);
                }
            }
            if batch.failed != 0 {
                self.set_status(
                    &format!("復元できなかった文書が {} 件あります", batch.failed),
                    MessageKind::Warning,
                );
            }
            return;
        };
        let sender = self.sender();
        WorkerPool::global().spawn(move || {
            let prepared = prepare_candidate(&candidate);
            sender.send(UiMessage::Run(Box::new(move |app| {
                match prepared
                    .and_then(|(state, text)| app.restore_candidate(candidate, state, &text))
                {
                    Ok(id) => {
                        batch.first.get_or_insert(id);
                    }
                    Err(_) => batch.failed += 1,
                }
                app.restore_next(batch);
            })));
        });
    }

    /// 復元データ 1 件を、未保存の文書として開く。本文とファイルの状態はワーカーで取得済み。
    fn restore_candidate(
        &mut self,
        candidate: Candidate,
        mut state: DocumentState,
        text: &str,
    ) -> Result<DocumentId> {
        let meta = &candidate.meta;
        let already_open = state.path().is_some_and(|path| {
            self.opening.contains(path) || self.docs.iter().any(|d| d.state.path() == Some(path))
        });
        let language = meta
            .source_path
            .as_deref()
            .map(Language::from_path)
            .unwrap_or(Language::PlainText);
        let mut title = meta
            .source_path
            .as_deref()
            .map(file_title)
            .unwrap_or_else(|| meta.title.clone());
        let changed = state.has_external_change();
        if already_open {
            state = DocumentState::untitled();
            state.format = meta.format;
            state.mark_unsaved();
            title.push_str("（復元）");
        }
        let lossy = meta.lossy;
        let id = self.open_text(state, text, title, language, false)?;
        let i = self.index_of(id)?;
        self.docs[i].slot = Some(Arc::new(candidate.into_slot()));
        self.docs[i].lossy = lossy;
        if already_open {
            self.set_status(
                "元のファイルを開いているため、復元した内容を別のタブに開きました。「名前を付けて保存」で保存できます",
                MessageKind::Info,
            );
        } else if changed {
            self.set_status(
                &format!(
                    "「{}」の元のファイルは、復元データを作った後に変更されています。「名前を付けて保存」で別に保存してください",
                    self.docs[i].title
                ),
                MessageKind::Warning,
            );
        }
        Ok(id)
    }
}

fn prepare_candidate(candidate: &Candidate) -> Result<(DocumentState, String)> {
    let text = candidate.read_text()?;
    let meta = &candidate.meta;
    let mut state = match (&meta.source_path, meta.kind) {
        (Some(path), _) => {
            // 通常のファイルを開く操作と同じ形式で比較する。
            let path = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
            let known = meta.file_stamp;
            let disk = known
                .or_else(|| FileStamp::read(&path).ok().flatten())
                .unwrap_or(FileStamp {
                    len: 0,
                    modified: None,
                    id: None,
                });
            let changed = check_external_change(&path, known).is_ok_and(|c| c.is_changed());
            let mut state = DocumentState::open_file(path, false, meta.format, disk);
            // 復元の前に、元ファイルが変わっていないか確かめる。
            if changed {
                state.record_external_change();
            }
            state
        }
        (None, RecoveryKind::Clipboard) => DocumentState::clipboard(),
        (None, _) => DocumentState::untitled(),
    };
    state.format = meta.format;
    state.mark_unsaved();
    Ok((state, text))
}
