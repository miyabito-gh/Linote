//! 検索・置換・Grep の統合（統合 I2 の K）。
//!
//! - 検索欄（[`FindPanel`]）は初めて使うときに作る。利用者の操作は [`FindEvent`] で届き、
//!   ここで現在の文書に対して [`Searcher`] を呼ぶ。
//! - 通常の文書: Scintilla を包んだ [`crate::ui::editor::Editor`]（`TextSource`／`EditTarget`
//!   の実装）に対して検索・置換する。置換は 1 つの Undo のまとまりにする。大きな文書
//!   （[`WORKER_SEARCH_BYTES`] 以上）は本文の写しをワーカーに渡し、結果を [`DocStamp`] で
//!   照合してから反映する（置換は、ワーカーが求めた置換の一覧を UI スレッドで 1 つの
//!   Undo のまとまりとして適用する）。
//! - 巨大モードの文書: [`crate::bigfile::BigFile`] の検索（[`BigView::find`]）。置換は使えない。
//! - Grep の結果（[`FindEvent::OpenHit`]）はファイルを開いて該当行へ移る。

use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;

use crate::bigfile::{self, SearchKind, SearchPattern};
use crate::core::{
    CancelToken, Completion, DocStamp, DocumentId, Error, Limits, Outcome, Result, TaskContext,
    WorkerPool,
};
use crate::document::{
    Capability, EditTarget, LineCount, MemoryText, Selection, TextRange, TextSource,
};
use crate::search::{Direction, Hit, ReplaceSummary, SearchOptions, Searcher};
use crate::ui::find::{FindEvent, FindPanel};
use crate::ui::statusbar::MessageKind;

use super::{App, UiMessage};

/// これ以上の大きさの文書は、本文の写しをワーカーで検索・置換する（UI を止めない）。
pub(super) const WORKER_SEARCH_BYTES: usize = 4 * 1024 * 1024;
/// 1 回の「すべて置換」で置換する件数の上限（超えたら何も変更しない）。
const MAX_REPLACEMENTS: u64 = 1_000_000;
/// 検索欄に入れる選択文字列の最大バイト数。
const MAX_PREFILL_BYTES: u64 = 256;

/// ワーカーで求めた置換の結果（終わり方と、置換の一覧）。
type RecordedReplace = (Outcome<ReplaceSummary>, Vec<(TextRange, String)>);

/// ワーカーで求めた置換の一覧を記録する対象（本文は変更しない）。置換は後ろから行われる
/// ので、記録した範囲は元の本文の位置のまま、一覧の順に適用できる。
struct Recorder {
    src: MemoryText,
    edits: Vec<(TextRange, String)>,
}

impl TextSource for Recorder {
    fn len(&self) -> u64 {
        self.src.len()
    }

    fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<usize> {
        self.src.read_at(offset, buf)
    }

    fn line_count(&self) -> LineCount {
        self.src.line_count()
    }

    fn line_start(&self, line: u64) -> Result<Option<u64>> {
        self.src.line_start(line)
    }
}

impl EditTarget for Recorder {
    fn selection(&self) -> Selection {
        Selection::default()
    }

    fn set_selection(&mut self, _selection: Selection) {}

    fn replace(&mut self, range: TextRange, text: &str) -> Result<()> {
        self.edits.push((range, text.to_owned()));
        Ok(())
    }

    fn begin_undo_group(&mut self) {}

    fn end_undo_group(&mut self) {}
}

/// 「選択範囲内」の状態（アクティブな文書について）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Scope {
    /// 範囲を限っていない（文書全体）。
    Off,
    /// この範囲（編集でずれた後の位置）に限る。
    Range(TextRange),
    /// 「選択範囲内」だが、範囲が消えた（範囲の本文をすべて削除した場合など）。
    Missing,
}

const SCOPE_MISSING: &str =
    "検索の範囲が失われました（「選択範囲内」を外すか、範囲を選び直してください）";

/// 範囲の先頭からの位置で求めた一致を、文書の位置に直す。
fn shift_hit(hit: Option<Hit>, base: u64) -> Option<Hit> {
    hit.map(|h| Hit {
        range: TextRange::new(h.range.start + base, h.range.end + base),
        wrapped: h.wrapped,
    })
}

/// 検索のエラーを検索欄に出す文にする。
fn describe(e: &Error) -> String {
    match e {
        Error::InvalidArgument { reason, .. } => reason.clone(),
        other => other.to_string(),
    }
}

impl App {
    // ---------------------------------------------------------------- 検索欄

    /// 検索欄を出す（初めてなら作る）。`grep` なら Grep の起点のフォルダーも決める。
    pub fn show_find(&mut self, grep: bool) -> Result<()> {
        if self.find_panel.is_none() {
            let sender = self.sender();
            let on_event: Rc<dyn Fn(FindEvent)> = Rc::new(move |event| {
                sender.send(UiMessage::Run(Box::new(move |app| {
                    app.on_find_event(event)
                })));
            });
            let panel = FindPanel::create(self.hwnd, &self.theme, self.dpi, on_event)?;
            self.find_panel = Some(panel);
            self.sync_find_history();
            self.sync_grep_settings();
        }
        let Some(panel) = self.find_panel else {
            return Ok(());
        };
        let prefill = self.find_prefill();
        if grep {
            if let Some(dir) = self.grep_folder() {
                panel.set_grep_folder(&dir);
            }
        }
        self.update_find_context();
        panel.show(prefill.as_deref());
        Ok(())
    }

    /// 設定の再読み込み時だけ Grep 条件も差し替える。
    pub(super) fn sync_grep_settings(&self) {
        if let Some(panel) = self.find_panel {
            panel.set_grep_settings(&self.settings.grep);
        }
    }

    /// 設定の履歴を検索欄のドロップダウンに反映する。
    pub(super) fn sync_find_history(&self) {
        if let Some(p) = self.find_panel {
            let h = &self.settings.search_history;
            p.set_history(&h.find, &h.replace, &h.grep_dir);
        }
    }

    /// 実行した語を履歴の先頭に置く。変わったら設定に保存する（壊れた設定ファイルには書かない）。
    fn record_history(&mut self, find: &str, replace: Option<&str>, grep_dir: Option<&str>) {
        let h = &mut self.settings.search_history;
        let mut changed = h.push_find(find);
        if let Some(r) = replace {
            changed |= h.push_replace(r);
        }
        if let Some(d) = grep_dir {
            changed |= h.push_grep_dir(d);
        }
        if !changed {
            return;
        }
        self.sync_find_history();
        if self.settings_state.writable() {
            self.persist_settings();
        }
    }

    // ---------------------------------------------------------------- 選択範囲内

    /// `id` の検索の範囲。
    pub(super) fn scope_of(&mut self, id: DocumentId) -> Scope {
        if self.scope_doc != Some(id) {
            return Scope::Off;
        }
        match self.with_text(id, |ed| ed.search_scope()) {
            Ok(Some(r)) => Scope::Range(r),
            _ => Scope::Missing,
        }
    }

    /// 「選択範囲内」を解除する（印も消し、検索欄のチェックも外す）。
    pub(super) fn clear_scope(&mut self) {
        if let Some(id) = self.scope_doc.take() {
            let _ = self.with_text(id, |ed| ed.clear_search_scope());
        }
        if let Some(p) = self.find_panel {
            p.set_in_selection(false);
        }
    }

    /// 範囲の本文（文字として読めなければエラー）。
    fn scoped_text(&mut self, id: DocumentId, r: TextRange) -> Result<String> {
        let bytes = self.with_text(id, |ed| ed.text_range(r.start, r.end))?;
        String::from_utf8(bytes)
            .map_err(|_| Error::invalid_argument("選択範囲", "選択範囲を文字として読めません"))
    }

    /// 検索用の本文の写し（`scope` があればその範囲だけ）。
    pub(super) fn scope_snapshot(
        &mut self,
        id: DocumentId,
        scope: Option<TextRange>,
    ) -> Result<(DocStamp, String)> {
        let Some(r) = scope else {
            return self.text_snapshot(id);
        };
        self.sync();
        let stamp = self
            .state(id)
            .map(|s| s.stamp())
            .ok_or_else(|| Error::not_found(format!("文書 {id}")))?;
        Ok((stamp, self.scoped_text(id, r)?))
    }

    /// 「選択範囲内」のオン・オフ。オンにした時点の選択範囲を検索の範囲にする。
    pub fn set_scope(&mut self, on: bool) {
        if !on {
            self.clear_scope();
            self.refresh_query_highlight();
            return;
        }
        let Some(id) = self.active_id() else {
            self.clear_scope();
            return;
        };
        if self.bigviews.contains_key(&id) {
            self.clear_scope();
            self.find_message("巨大ファイルでは「選択範囲内」は使えません", true);
            return;
        }
        let sel = self
            .with_text(id, |ed| ed.selection().range())
            .unwrap_or(TextRange::new(0, 0));
        if sel.is_empty() {
            self.clear_scope();
            self.find_message(
                "選択範囲がありません（範囲を選んでから「選択範囲内」をオンにしてください）",
                true,
            );
            return;
        }
        // 別の文書の範囲が残っていれば消す。
        if let Some(old) = self.scope_doc.filter(|d| *d != id) {
            let _ = self.with_text(old, |ed| ed.clear_search_scope());
        }
        let _ = self.with_text(id, |ed| ed.set_search_scope(sel));
        self.scope_doc = Some(id);
        self.refresh_query_highlight();
        self.find_message("選択範囲内を対象にしました", false);
    }

    /// 検索欄の現在の条件で強調を付け直す（範囲の切り替えのあと）。
    fn refresh_query_highlight(&mut self) {
        match self.find_panel.and_then(|p| p.options()) {
            Some(o) if !o.query.is_empty() => self.on_query_changed(o),
            _ => self.clear_highlight(),
        }
    }

    /// 検索欄が開いているか。
    pub fn find_visible(&self) -> bool {
        self.find_panel.is_some_and(|p| p.is_visible())
    }

    /// Grep の起点の既定（アクティブな文書のフォルダー、なければ現在のフォルダー）。
    fn grep_folder(&self) -> Option<PathBuf> {
        self.active_id()
            .and_then(|id| self.state(id))
            .and_then(|s| s.path())
            .and_then(Path::parent)
            .map(Path::to_path_buf)
            .or_else(|| std::env::current_dir().ok())
    }

    /// 検索欄の初期値にする選択文字列（1 行で短いものだけ）。
    fn find_prefill(&mut self) -> Option<String> {
        let id = self.active_id()?;
        if let Some(b) = self.bigviews.get(&id) {
            let r = b.view.selection()?;
            if r.is_empty() || r.len() > MAX_PREFILL_BYTES {
                return None;
            }
            return b.view.selected_text().ok();
        }
        self.with_text(id, |ed| {
            let r = ed.selection().range();
            if r.is_empty() || r.len() > MAX_PREFILL_BYTES {
                return None;
            }
            Some(String::from_utf8_lossy(&ed.text_range(r.start, r.end)).into_owned())
        })
        .ok()
        .flatten()
    }

    /// アクティブな文書に合わせて検索欄の置換の有効・無効を直す（タブの切り替えで呼ぶ）。
    pub(super) fn update_find_context(&self) {
        let Some(panel) = self.find_panel else { return };
        let Some(i) = self.active else { return };
        let d = &self.docs[i];
        if self.bigviews.contains_key(&d.id()) {
            panel.set_replace_enabled(
                false,
                Some(
                    "巨大ファイルは読み取り専用のため、置換は使えません（正規表現の検索は行単位です）",
                ),
            );
        } else {
            panel.set_replace_enabled(d.state.capabilities().contains(Capability::Edit), None);
        }
    }

    /// 検索の状態を知らせる。検索欄が開いていればそこへ、閉じていれば状態表示へ。
    pub(super) fn find_message(&mut self, text: &str, error: bool) {
        match self.find_panel {
            Some(p) if p.is_visible() => p.set_status(text, error),
            _ => {
                let kind = if error {
                    MessageKind::Warning
                } else {
                    MessageKind::Info
                };
                self.set_status(text, kind);
            }
        }
    }

    /// F3／Shift+F3 で使う検索条件（検索欄の現在の条件、なければ最後に使った条件）。
    pub(super) fn current_search_options(&self) -> Option<SearchOptions> {
        self.find_panel
            .and_then(|p| p.options())
            .filter(|o| !o.query.is_empty())
            .or_else(|| self.last_search.clone())
    }

    /// 検索語にする文字列: 選択中の文字列（1 行で短いもの）、なければカーソル位置の単語。
    /// 巨大モードでは選択だけ。
    fn query_from_caret(&mut self) -> Option<String> {
        if let Some(q) = self.find_prefill() {
            if !q.is_empty() && !q.contains(['\n', '\r']) {
                return Some(q);
            }
        }
        let id = self.active_id()?;
        if self.bigviews.contains_key(&id) {
            return None;
        }
        self.with_text(id, |ed| {
            let caret = ed.selection().caret;
            ed.word_at(caret)
        })
        .ok()
        .flatten()
    }

    /// F3／Shift+F3: 検索欄が閉じていても、直前の検索条件で次（前）へ。条件がなければ
    /// 選択中の文字列（なければカーソル位置の単語）で始め、それもなければ検索欄を開く。
    pub(super) fn find_again(&mut self, direction: Direction) -> Result<()> {
        if let Some(o) = self.current_search_options() {
            self.search_step(o, direction);
        } else if self.query_from_caret().is_some() {
            self.find_selection(direction)?;
        } else {
            self.show_find(false)?;
        }
        Ok(())
    }

    /// Ctrl+F3: 選択中の文字列（なければカーソル位置の単語）を検索語にして、次の一致へ。
    pub(super) fn find_selection(&mut self, direction: Direction) -> Result<()> {
        let Some(q) = self.query_from_caret() else {
            self.find_message("検索する文字列が選択されていません", true);
            return Ok(());
        };
        let mut options = self.current_search_options().unwrap_or_default();
        options.regex = false;
        options.query = q.clone();
        if let Some(p) = self.find_panel {
            // 検索欄の条件も更新する（検索欄が正規表現のときは、記号をそのまま探す）。
            if let Some(panel_options) = p.options() {
                options = SearchOptions {
                    query: q.clone(),
                    ..panel_options
                };
                if options.regex {
                    options.query = regex::escape(&q);
                }
            }
            p.set_query(&options.query);
        }
        self.search_step(options, direction);
        Ok(())
    }

    // ---------------------------------------------------------------- 検索欄の通知

    pub(super) fn on_find_event(&mut self, event: FindEvent) {
        match event {
            FindEvent::Next(o) => {
                self.last_search = Some(o.clone());
                self.search_step(o, Direction::Forward);
            }
            FindEvent::Prev(o) => {
                self.last_search = Some(o.clone());
                self.search_step(o, Direction::Backward);
            }
            FindEvent::Replace {
                options,
                replacement,
            } => {
                self.last_search = Some(options.clone());
                self.replace_one(options, &replacement);
            }
            FindEvent::ReplaceAll {
                options,
                replacement,
            } => {
                self.last_search = Some(options.clone());
                self.replace_all(options, replacement);
            }
            FindEvent::Scope(on) => self.set_scope(on),
            FindEvent::Ran {
                find,
                replace,
                grep_dir,
            } => self.record_history(&find, replace.as_deref(), grep_dir.as_deref()),
            FindEvent::GrepSettings(settings) => {
                self.reload_settings(super::settingsops::ReloadReason::BeforeApply);
                if self.settings.grep != settings {
                    self.settings.grep = settings;
                    self.sync_grep_settings();
                    if self.settings_state.writable() {
                        self.persist_settings();
                    }
                }
            }
            FindEvent::Changed(options) => self.on_query_changed(options),
            FindEvent::OpenHit { path, line, column } => self.open_hit(path, line, column),
            FindEvent::Closed => self.focus_active(),
        }
    }

    // ---------------------------------------------------------------- 検索

    /// 次（前）の一致を探して選択する。端で反対側へ回り込む。
    pub fn search_step(&mut self, options: SearchOptions, direction: Direction) {
        let Some(id) = self.active_id() else { return };
        self.last_search = Some(options.clone());
        if self.bigviews.contains_key(&id) {
            self.big_search(id, &options, direction);
            return;
        }
        let searcher = match options.compile() {
            Ok(s) => s,
            Err(e) => {
                self.find_message(&describe(&e), true);
                return;
            }
        };
        let scope = match self.scope_of(id) {
            Scope::Off => None,
            Scope::Range(r) => Some(r),
            Scope::Missing => {
                self.find_message(SCOPE_MISSING, true);
                return;
            }
        };
        let Ok((len, sel)) = self.with_text(id, |ed| (ed.length(), ed.selection())) else {
            return;
        };
        let range = sel.range();
        let mut from = match direction {
            Direction::Forward => range.end,
            Direction::Backward => range.start,
        };
        let base = scope.map_or(0, |r| r.start);
        let size = scope.map_or(len, |r| r.len());
        if let Some(r) = scope {
            // 範囲の外から始めるときは、範囲の端から。
            from = from.clamp(r.start, r.end) - r.start;
        }
        if size as usize >= self.worker_search_bytes {
            self.search_on_worker(id, searcher, from, direction, scope);
            return;
        }
        self.cancel_search();
        let result = match scope {
            None => self.with_text(id, |ed| {
                searcher.find(ed, from, direction, true, &TaskContext::detached())
            }),
            Some(r) => self.scoped_text(id, r).map(|text| {
                searcher.find(
                    &MemoryText::new(text),
                    from,
                    direction,
                    true,
                    &TaskContext::detached(),
                )
            }),
        };
        match result {
            Ok(Ok(outcome)) => self.apply_hit(id, shift_hit(outcome.value, base), direction),
            Ok(Err(e)) | Err(e) => self.find_message(&describe(&e), true),
        }
    }

    pub(super) fn cancel_search(&mut self) {
        if let Some(token) = self.search_cancel.take() {
            token.cancel();
        }
    }

    fn search_on_worker(
        &mut self,
        id: DocumentId,
        searcher: Searcher,
        from: u64,
        direction: Direction,
        scope: Option<TextRange>,
    ) {
        let base = scope.map_or(0, |r| r.start);
        let (stamp, text) = match self.scope_snapshot(id, scope) {
            Ok(r) => r,
            Err(e) => {
                self.find_message(&describe(&e), true);
                return;
            }
        };
        self.cancel_search();
        let token = CancelToken::new();
        self.search_cancel = Some(token.clone());
        let sender = self.sender();
        self.find_message("検索中...", false);
        WorkerPool::global().spawn(move || {
            let mut ctx = TaskContext::detached();
            ctx.cancel = token;
            let src = MemoryText::new(text);
            let result = searcher.find(&src, from, direction, true, &ctx);
            sender.send(UiMessage::Run(Box::new(move |app| {
                app.finish_search(stamp, direction, base, result)
            })));
        });
    }

    fn finish_search(
        &mut self,
        stamp: DocStamp,
        direction: Direction,
        base: u64,
        result: Result<Outcome<Option<Hit>>>,
    ) {
        // 検索している間に編集された・タブが替わったときは、結果を使わない。
        if !self.is_current(stamp) || self.active_id() != Some(stamp.document) {
            return;
        }
        match result {
            Ok(outcome) if outcome.completion == Completion::Cancelled => {}
            Ok(outcome) => {
                self.apply_hit(stamp.document, shift_hit(outcome.value, base), direction)
            }
            Err(e) if e.is_cancelled() => {}
            Err(e) => self.find_message(&describe(&e), true),
        }
    }

    /// 検索の結果を選択に反映して知らせる。
    fn apply_hit(&mut self, id: DocumentId, hit: Option<Hit>, direction: Direction) {
        let Some(hit) = hit else {
            self.find_message("見つかりません", true);
            return;
        };
        let r = hit.range;
        let selection = match direction {
            Direction::Forward => Selection {
                anchor: r.start,
                caret: r.end,
            },
            Direction::Backward => Selection {
                anchor: r.end,
                caret: r.start,
            },
        };
        let line = self.with_text(id, |ed| {
            EditTarget::set_selection(ed, selection);
            ed.line_from_position(r.start) + 1
        });
        let Ok(line) = line else { return };
        self.refresh_status();
        // 全一致の強調（検索欄が閉じていても付ける）と「n / 全 m 件」。
        if let Some(options) = self.last_search.clone() {
            self.ensure_highlight(&options);
        }
        if self.report_highlight(hit.wrapped) {
            return;
        }
        let text = if hit.wrapped {
            format!("{line} 行目（端で折り返しました）")
        } else {
            format!("{line} 行目")
        };
        self.find_message(&text, false);
    }

    /// 巨大ファイルの検索（ワーカーで探し、結果は [`BigViewEvent::Found`] で届く）。
    ///
    /// [`BigViewEvent::Found`]: crate::ui::bigview::BigViewEvent::Found
    fn big_search(&mut self, id: DocumentId, options: &SearchOptions, direction: Direction) {
        let (text, kind) = if options.whole_word {
            let inner = if options.regex {
                options.query.clone()
            } else {
                regex::escape(&options.query)
            };
            (format!(r"\b(?:{inner})\b"), SearchKind::Regex)
        } else if options.regex {
            (options.query.clone(), SearchKind::Regex)
        } else {
            (options.query.clone(), SearchKind::Literal)
        };
        let pattern = match SearchPattern::new(&text, kind, options.case_sensitive) {
            Ok(p) => p,
            Err(e) => {
                self.find_message(&describe(&e), true);
                return;
            }
        };
        let Some(b) = self.bigviews.get(&id) else {
            return;
        };
        let notes = pattern.notes(b.file.config());
        let dir = match direction {
            Direction::Forward => bigfile::Direction::Forward,
            Direction::Backward => bigfile::Direction::Backward,
        };
        b.view.find(Arc::new(pattern), dir);
        if notes.line_based {
            self.find_message(
                "巨大ファイルの正規表現検索は行単位です（行をまたぐ一致は探せません）",
                false,
            );
        }
    }

    // ---------------------------------------------------------------- 置換

    /// 選択が一致なら置換して、次の一致を選ぶ。
    pub fn replace_one(&mut self, options: SearchOptions, replacement: &str) {
        let Some(id) = self.active_id() else { return };
        if !self.can_replace(id) {
            return;
        }
        let searcher = match options.compile() {
            Ok(s) => s,
            Err(e) => {
                self.find_message(&describe(&e), true);
                return;
            }
        };
        let scope = match self.scope_of(id) {
            Scope::Off => None,
            Scope::Range(r) => Some(r),
            Scope::Missing => {
                self.find_message(SCOPE_MISSING, true);
                return;
            }
        };
        let result = self.with_text(id, |ed| -> Result<bool> {
            let range = ed.selection().range();
            // 範囲の外の一致は置換しない（範囲内の次の一致へ移るだけ）。
            if scope.is_some_and(|r| range.start < r.start || range.end > r.end) {
                return Ok(false);
            }
            match searcher.replacement_for(ed, range, replacement)? {
                Some(text) => {
                    EditTarget::replace(ed, range, &text)?;
                    if let Some(r) = scope {
                        // 範囲の端の置換でも印が縮まないよう、付け直す。
                        let end = (r.end + text.len() as u64).saturating_sub(range.len());
                        ed.set_search_scope(TextRange::new(r.start, end.max(r.start)));
                    }
                    Ok(true)
                }
                None => Ok(false),
            }
        });
        match result {
            Ok(Ok(_)) => self.search_step(options, Direction::Forward),
            Ok(Err(e)) | Err(e) => self.find_message(&describe(&e), true),
        }
    }

    /// 置換できる文書か（巨大ファイル・読み取り専用では、理由を知らせて `false`）。
    fn can_replace(&mut self, id: DocumentId) -> bool {
        if self.bigviews.contains_key(&id) {
            self.find_message("巨大ファイルは読み取り専用のため、置換できません", true);
            return false;
        }
        let editable = self
            .state(id)
            .is_some_and(|s| s.capabilities().contains(Capability::Edit));
        if !editable {
            self.find_message("この文書は読み取り専用のため、置換できません", true);
        }
        editable
    }

    /// すべて置換する。Undo は 1 回で元に戻る。
    pub fn replace_all(&mut self, options: SearchOptions, replacement: String) {
        let Some(id) = self.active_id() else { return };
        if !self.can_replace(id) {
            return;
        }
        let searcher = match options.compile() {
            Ok(s) => s,
            Err(e) => {
                self.find_message(&describe(&e), true);
                return;
            }
        };
        match self.scope_of(id) {
            Scope::Off => {}
            Scope::Range(r) => {
                self.replace_in_scope(id, searcher, replacement, r);
                return;
            }
            Scope::Missing => {
                self.find_message(SCOPE_MISSING, true);
                return;
            }
        }
        let Ok(len) = self.with_text(id, |ed| ed.length()) else {
            return;
        };
        if len as usize >= self.worker_search_bytes {
            self.replace_all_on_worker(id, searcher, replacement);
            return;
        }
        let ctx = TaskContext::detached().with_limits(Limits::items(MAX_REPLACEMENTS));
        let result = self.with_text(id, |ed| searcher.replace_all(ed, &replacement, &ctx));
        match result {
            Ok(Ok(outcome)) => self.report_replaced(outcome),
            Ok(Err(e)) | Err(e) => self.find_message(&describe(&e), true),
        }
        self.refresh_status();
    }

    fn report_replaced(&mut self, outcome: Outcome<ReplaceSummary>) {
        match outcome.completion {
            Completion::Complete if outcome.value.replaced == 0 => {
                self.find_message("見つかりません", true)
            }
            Completion::Complete => self.find_message(
                &format!(
                    "{} 件を置換しました（「元に戻す」1 回で元に戻ります）",
                    outcome.value.replaced
                ),
                false,
            ),
            _ => self.find_message(
                "一致が多すぎる（または中止された）ため、何も置換していません",
                true,
            ),
        }
    }

    /// 範囲の中だけをすべて置換する。Undo は 1 回で元に戻る。範囲の本文だけを検索の
    /// 対象にするので、`^`・`\b` などは範囲の端を文書の端として扱う。
    fn replace_in_scope(
        &mut self,
        id: DocumentId,
        searcher: Searcher,
        replacement: String,
        sel: TextRange,
    ) {
        let Ok(text) = self.scoped_text(id, sel) else {
            self.find_message("選択範囲を文字として読めませんでした", true);
            return;
        };
        self.sync();
        let Some(stamp) = self.state(id).map(|s| s.stamp()) else {
            return;
        };
        let run = move |text: String| {
            let ctx = TaskContext::detached().with_limits(Limits::items(MAX_REPLACEMENTS));
            let mut rec = Recorder {
                src: MemoryText::new(text),
                edits: Vec::new(),
            };
            searcher
                .replace_all(&mut rec, &replacement, &ctx)
                .map(|outcome| (outcome, rec.edits))
        };
        if sel.len() as usize >= self.worker_search_bytes {
            let sender = self.sender();
            self.find_message("置換中...", false);
            WorkerPool::global().spawn(move || {
                let result = run(text);
                sender.send(UiMessage::Run(Box::new(move |app| {
                    app.finish_replace_all(stamp, Some(sel), result)
                })));
            });
        } else {
            let result = run(text);
            self.finish_replace_all(stamp, Some(sel), result);
        }
    }

    fn replace_all_on_worker(&mut self, id: DocumentId, searcher: Searcher, replacement: String) {
        let (stamp, text) = match self.text_snapshot(id) {
            Ok(r) => r,
            Err(e) => {
                self.find_message(&describe(&e), true);
                return;
            }
        };
        let sender = self.sender();
        self.find_message("置換中...", false);
        WorkerPool::global().spawn(move || {
            let ctx = TaskContext::detached().with_limits(Limits::items(MAX_REPLACEMENTS));
            let mut rec = Recorder {
                src: MemoryText::new(text),
                edits: Vec::new(),
            };
            let result = searcher
                .replace_all(&mut rec, &replacement, &ctx)
                .map(|outcome| (outcome, rec.edits));
            sender.send(UiMessage::Run(Box::new(move |app| {
                app.finish_replace_all(stamp, None, result)
            })));
        });
    }

    fn finish_replace_all(
        &mut self,
        stamp: DocStamp,
        scope: Option<TextRange>,
        result: Result<RecordedReplace>,
    ) {
        let (outcome, edits) = match result {
            Ok(r) => r,
            Err(e) => {
                self.find_message(&describe(&e), true);
                return;
            }
        };
        if !self.is_current(stamp) {
            self.find_message(
                "置換している間に文書が変更されたため、何も置換していません",
                true,
            );
            return;
        }
        if !outcome.completion.is_complete() || edits.is_empty() {
            self.report_replaced(outcome);
            return;
        }
        let id = stamp.document;
        let base = scope.map_or(0, |r| r.start);
        let applied = self.with_text(id, |ed| -> Result<()> {
            ed.begin_undo_action();
            let mut result = Ok(());
            for (range, text) in &edits {
                let range = TextRange::new(range.start + base, range.end + base);
                result = EditTarget::replace(ed, range, text);
                if result.is_err() {
                    break;
                }
            }
            ed.end_undo_action();
            // 選択範囲内の置換は、置換後の範囲を選んだままにする。
            if let (Some(scope), Ok(())) = (scope, &result) {
                let delta: i64 = edits
                    .iter()
                    .map(|(r, t)| t.len() as i64 - r.len() as i64)
                    .sum();
                let end = (scope.end as i64 + delta).max(scope.start as i64) as u64;
                EditTarget::set_selection(
                    ed,
                    Selection {
                        anchor: scope.start,
                        caret: end,
                    },
                );
                // 範囲の印も、置換後の範囲に付け直す。
                ed.set_search_scope(TextRange::new(scope.start, end));
            }
            result
        });
        match applied {
            Ok(Ok(())) => self.report_replaced(outcome),
            Ok(Err(e)) | Err(e) => self.find_message(&describe(&e), true),
        }
        self.refresh_status();
    }

    // ---------------------------------------------------------------- Grep の結果

    /// Grep の結果を開いて、該当行へ移る（開いていなければ開いてから移る）。
    pub fn open_hit(&mut self, path: PathBuf, line: u64, column: u64) {
        let path = std::path::absolute(&path).unwrap_or(path);
        let open = self
            .docs
            .iter()
            .find(|d| d.state.path() == Some(path.as_path()))
            .map(|d| d.id());
        match open {
            Some(id) => {
                if self.activate(id).is_ok() {
                    self.jump_to(id, line, column);
                }
            }
            None => {
                self.pending_jumps.insert(path.clone(), (line, column));
                self.open_path(&path);
            }
        }
    }

    /// 開き終えた文書に、待たせていた移動を適用する。
    pub(super) fn apply_pending_jump(&mut self, id: DocumentId) {
        self.apply_reopen_view(id);
        let Some(path) = self.state(id).and_then(|s| s.path()).map(Path::to_path_buf) else {
            return;
        };
        if let Some((line, column)) = self.pending_jumps.remove(&path) {
            self.jump_to(id, line, column);
        }
    }

    /// `line`・`column`（1 始まり。`column` は文字数）へカーソルを移す。
    fn jump_to(&mut self, id: DocumentId, line: u64, column: u64) {
        if self.bigviews.contains_key(&id) {
            let _ = self.big_goto(id, Some(line.max(1)));
            return;
        }
        let moved = self.with_text(id, |ed| {
            let l = line
                .saturating_sub(1)
                .min(ed.line_count().saturating_sub(1));
            let start = ed.line_start(l).unwrap_or(0);
            let end = ed.line_end(l).min(start + 64 * 1024);
            let bytes = ed.text_range(start, end);
            let chars = column.saturating_sub(1) as usize;
            let offset: usize = String::from_utf8_lossy(&bytes)
                .chars()
                .take(chars)
                .map(char::len_utf8)
                .sum();
            let pos = (start + offset.min(bytes.len()) as u64).min(ed.length());
            let pos = if ed.is_char_boundary(pos) { pos } else { start };
            EditTarget::set_selection(ed, Selection::caret_at(pos));
        });
        if moved.is_ok() {
            self.refresh_status();
            self.focus_active();
        }
    }
}
