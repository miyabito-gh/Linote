//! ファイルの読み込み・保存の流れ、外部変更の検知、閉じる・終了の確認、設定ファイル。
//!
//! 中核は [`crate::file`]（読み書き、外部変更）と [`crate::text`]（文字コード）。ここは
//! 画面側のつなぎ込み:
//!
//! - 読み込みと保存はワーカーで行い、結果は [`UiMessage::Run`] で UI スレッドに戻す。
//!   戻ったら文書の存在と版を確かめてから反映する（`DocStamp`）。
//! - 保存は同じ文書について 1 つだけ進める。変換できない文字は確認のうえ `Replace` で
//!   やり直す。外部変更は別名での保存を案内する。失敗は [`crate::file::classify`] で
//!   原因別の文言にする。
//! - 外部変更の確認は、タブを表示したとき・ウィンドウが前面に来たとき・保存の直前。

use std::path::{Path, PathBuf};

use crate::appdata::{AppPaths, Settings};
use crate::bigfile::{large_mode_reason, FileProfile, LargeModeReason};
use crate::core::{DocStamp, DocumentId, Error, Limits, Result, TaskContext, WorkerPool};
use crate::document::{Capability, DocumentKind, DocumentState};
use crate::file::{
    self, check_external_change, ExternalChange, LoadedText, ReadOptions, SaveOutcome, SaveRequest,
};
use crate::text::{Confidence, Encoding, OnUnmappable};
use crate::ui::editor::{DocLoader, Language};
use crate::ui::statusbar::MessageKind;

use super::closing::{self, ChoiceAction, ClosePlan};
use super::{file_title, App, DocView, UiMessage};

// ------------------------------------------------------------------ 設定ファイル

/// 設定ファイルの読み込みの結果。書き戻してよいかを決める。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SettingsState {
    /// ファイルがない（既定値で動く。保存すれば作る）。
    Missing,
    /// 読めた。
    Loaded,
    /// 壊れている・読めない。**上書きしない**（利用者が直すまで既定値で動く）。
    Broken(String),
}

impl SettingsState {
    pub fn writable(&self) -> bool {
        !matches!(self, SettingsState::Broken(_))
    }
}

/// 設定ファイルを読む。起動の経路で呼ぶので、小さなファイルを 1 度読むだけ。
/// 壊れていても既定値を返し、状態で知らせる（ファイルには触れない）。
pub fn load_settings_from(path: &Path) -> (Settings, SettingsState) {
    let bytes = match std::fs::read(path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return (Settings::default(), SettingsState::Missing)
        }
        Err(e) => {
            return (
                Settings::default(),
                SettingsState::Broken(format!("読み込めません: {e}")),
            )
        }
    };
    let parsed = String::from_utf8(bytes)
        .map_err(|_| Error::corrupt("設定ファイル", "UTF-8 ではありません"))
        .and_then(|text| Settings::from_json(&text));
    match parsed {
        Ok(settings) => (settings, SettingsState::Loaded),
        Err(e) => (Settings::default(), SettingsState::Broken(e.to_string())),
    }
}

pub fn load_settings(paths: Option<&AppPaths>) -> (Settings, SettingsState) {
    match paths {
        Some(p) => load_settings_from(&p.settings_file()),
        None => (Settings::default(), SettingsState::Missing),
    }
}

// ------------------------------------------------------------------ 文言

/// 失敗の原因を添えた文。
pub fn describe_error(e: &Error) -> String {
    match file::classify(e) {
        Some(failure) => format!("{}（{e}）", failure.describe()),
        None => e.to_string(),
    }
}

fn unmappable_text(
    name: &str,
    format: crate::text::TextFormat,
    chars: &crate::core::Outcome<Vec<crate::text::Unmappable>>,
) -> String {
    let sample: Vec<String> = chars
        .value
        .iter()
        .take(5)
        .map(|u| format!("「{}」（{} 行 {} 桁）", u.ch, u.line + 1, u.column + 1))
        .collect();
    let more = if chars.completion.is_complete() && chars.value.len() <= 5 {
        String::new()
    } else if chars.completion.is_complete() {
        format!(" ほか {} 文字", chars.value.len() - 5)
    } else {
        " ほか".to_owned()
    };
    format!(
        "「{name}」には {} で表せない文字があります: {}{more}\n\n\
         このまま保存すると、これらの文字は「?」に置き換わります。保存しますか？",
        format.encoding.label(),
        sample.join("、")
    )
}

// ------------------------------------------------------------------ 開く・保存の指定

/// 読み込みの依頼。
struct OpenRequest {
    path: PathBuf,
    /// 開き直しなら、置き換える文書の印（読み込みの間に編集されたら中止する）。
    reopen_of: Option<DocStamp>,
    /// 「UTF-8 (BOM 付き)」で開き直した場合、保存時に BOM を付ける。
    force_bom: bool,
}

/// 保存先。
#[derive(Debug, Clone)]
pub(super) enum SaveDest {
    /// 文書の現在のパス（上書き保存）。
    Current,
    /// 指定のパス（名前を付けて保存）。
    As(PathBuf),
}

/// 保存の後にすること。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum After {
    Nothing,
    /// 閉じる（閉じる前の「保存」の選択）。
    Close,
    /// 終了を続ける（終了前の「保存」の選択）。
    ContinueExit,
}

#[derive(Debug, Clone, Copy)]
pub(super) struct SaveOpts {
    pub after: After,
    /// 変換できない文字を `?` に置き換えて保存してよいと確認済み。
    pub replace_unmappable: bool,
}

impl SaveOpts {
    pub fn new(after: After) -> Self {
        SaveOpts {
            after,
            replace_unmappable: false,
        }
    }
}

impl App {
    // ---------------------------------------------------------------- 設定の保存

    /// 設定を保存する（壊れた設定ファイルは上書きしない）。ワーカーで書く。
    pub(super) fn persist_settings(&mut self) {
        let Some(paths) = self.paths.clone() else {
            return;
        };
        if !self.settings_state.writable() {
            self.set_status(
                "設定ファイルが壊れているため、設定を保存しませんでした",
                MessageKind::Warning,
            );
            return;
        }
        let json = self.settings.to_json();
        let sender = self.sender();
        self.settings_state = SettingsState::Loaded;
        WorkerPool::global().spawn(move || {
            let result = paths.ensure_dir(paths.root()).and_then(|()| {
                file::write_atomic(
                    &paths.settings_file(),
                    json.as_bytes(),
                    &TaskContext::detached(),
                )
            });
            if let Err(e) = result {
                sender.send(UiMessage::Status(format!(
                    "設定を保存できませんでした: {}",
                    describe_error(&e)
                )));
            }
        });
    }

    // ---------------------------------------------------------------- 開く

    /// ファイルを開く（ワーカーで読む）。既に開いていればそのタブへ切り替える。
    /// 失敗は状態表示に出す。
    pub fn open_path(&mut self, path: &Path) {
        if let Err(e) = self.open_inner(path, None) {
            self.report_failure(&format!("{} を開けませんでした", path.display()), &e);
        }
    }

    fn open_inner(
        &mut self,
        path: &Path,
        reopen: Option<(DocStamp, Option<Encoding>, bool)>,
    ) -> Result<()> {
        let path = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
        if reopen.is_none() {
            if let Some(d) = self
                .docs
                .iter()
                .find(|d| d.state.path() == Some(path.as_path()))
            {
                let id = d.id();
                return self.activate(id);
            }
        }
        if !self.opening.insert(path.clone()) {
            return Ok(());
        }
        let result = self.start_read(&path, reopen);
        if result.is_err() {
            self.opening.remove(&path);
        }
        result
    }

    fn start_read(
        &mut self,
        path: &Path,
        reopen: Option<(DocStamp, Option<Encoding>, bool)>,
    ) -> Result<()> {
        let meta = std::fs::metadata(path).map_err(|e| Error::io("読み込み", path, e))?;
        let thresholds = self.large_thresholds;
        let max = thresholds.max_bytes;
        let too_large = large_mode_reason(&FileProfile::of_size(meta.len()), &thresholds).is_some();
        if too_large {
            if reopen.is_some() {
                return Err(Error::LimitExceeded {
                    what: format!(
                        "文字コードを指定して開き直せるファイルの大きさ（{} MiB）",
                        max / (1024 * 1024)
                    ),
                    limit: max,
                });
            }
            // 巨大モード: 読み取り専用のビューで開く（本文は読み込まない）。
            self.opening.remove(path);
            return self.open_big(path).map(|_| ());
        }
        let loader = self.create_loader(meta.len())?;
        let (reopen_of, encoding, force_bom) = match reopen {
            Some((stamp, encoding, bom)) => (Some(stamp), encoding, bom),
            None => (None, None, false),
        };
        let request = OpenRequest {
            path: path.to_path_buf(),
            reopen_of,
            force_bom,
        };
        let reopen_of = request.reopen_of;
        let sender = self.sender();
        let read_path = path.to_path_buf();
        WorkerPool::global().spawn(move || {
            let ctx = TaskContext::detached().with_limits(Limits {
                max_items: None,
                max_bytes: Some(max),
            });
            let options = ReadOptions {
                encoding,
                ..ReadOptions::default()
            };
            let mut loader: DocLoader = loader;
            let allow_big = reopen_of.is_none();
            let result = file::read_text(&read_path, &options, &ctx).and_then(|mut loaded| {
                // 容量は小さくても、行数が多い・1 行が極端に長いファイルは巨大モードにする
                // （UTF-8 のときだけ。巨大ファイルのビューは UTF-8 を前提とする）。
                let gate = thresholds.max_lines.min(thresholds.max_line_bytes);
                let reason = if allow_big
                    && loaded.format.encoding == Encoding::Utf8
                    && loaded.text.len() as u64 > gate
                {
                    large_mode_reason(&FileProfile::of_text(loaded.text.as_bytes()), &thresholds)
                } else {
                    None
                };
                if reason.is_none() {
                    // 本文は読み込み口に流し込み、ここでは持たない。
                    let text = std::mem::take(&mut loaded.text);
                    loader.add(text.as_bytes())?;
                }
                Ok((loaded, reason))
            });
            let result = result.map(|(loaded, reason)| (loaded, loader, reason));
            sender.send(UiMessage::Run(Box::new(move |app| {
                app.finish_open(request, result)
            })));
        });
        Ok(())
    }

    fn finish_open(
        &mut self,
        request: OpenRequest,
        result: Result<(LoadedText, DocLoader, Option<LargeModeReason>)>,
    ) {
        self.opening.remove(&request.path);
        let (loaded, loader, big) = match result {
            Ok(r) => r,
            Err(e) => {
                self.report_failure(
                    &format!("{} を開けませんでした", request.path.display()),
                    &e,
                );
                self.ensure_a_document();
                return;
            }
        };
        if let Some(stamp) = request.reopen_of {
            if !self.is_current(stamp) {
                self.set_status(
                    "開き直している間に文書が変更されたため、開き直しを中止しました",
                    MessageKind::Warning,
                );
                return;
            }
        }
        if big.is_some() {
            // 行数が多い・長大な行がある: 読み込んだ分は捨て、読み取り専用のビューで開く。
            drop(loader);
            if let Err(e) = self.open_big(&request.path) {
                self.report_failure(
                    &format!("{} を開けませんでした", request.path.display()),
                    &e,
                );
                self.ensure_a_document();
            }
            return;
        }
        let doc = match loader.finish() {
            Ok(d) => d,
            Err(e) => {
                self.report_failure("編集欄に読み込めませんでした", &e);
                return;
            }
        };
        let mut format = loaded.format;
        if request.force_bom && format.encoding == Encoding::Utf8 {
            format.bom = true;
        }
        let title = file_title(&request.path);
        let language = Language::from_path(&request.path);
        let state = DocumentState::open_file(request.path.clone(), false, format, loaded.stamp);
        let id = match self.add_loaded(state, doc, title, language, true) {
            Ok(id) => id,
            Err(e) => {
                self.report_failure("文書を開けませんでした", &e);
                return;
            }
        };
        if let Some(old) = request.reopen_of {
            let _ = self.close_document(old.document, true);
        }
        let lossy = loaded.is_lossy();
        if let Ok(i) = self.index_of(id) {
            self.docs[i].lossy = lossy;
        }
        self.apply_pending_jump(id);
        if lossy {
            self.set_status(
                &format!(
                    "読み取れないバイトが {} 箇所あり、「\u{fffd}」に置き換えて表示しています。\
                     「ファイル > 文字コードを指定して開き直す」を試してください（このまま保存すると元に戻せません）",
                    loaded.invalid.count
                ),
                MessageKind::Warning,
            );
        } else if loaded.detection.confidence == Confidence::Low {
            self.set_status(
                &format!(
                    "文字コードを自動判定できませんでした（{} として表示）。違う場合は\
                     「ファイル > 文字コードを指定して開き直す」で指定してください",
                    loaded.format.encoding.label()
                ),
                MessageKind::Warning,
            );
        }
    }

    /// 文書が 1 つもない状態を避ける（開くのに失敗した起動時など）。
    fn ensure_a_document(&mut self) {
        if self.docs.is_empty() && self.opening.is_empty() {
            let _ = self.new_untitled();
        }
    }

    /// 文字コードを指定して開き直す。未保存の変更があれば確認する（破棄される）。
    /// `bom` は UTF-8 のとき、保存で BOM を付けるか。
    pub fn reopen_with_encoding(
        &mut self,
        id: DocumentId,
        encoding: Encoding,
        bom: bool,
    ) -> Result<()> {
        let i = self.index_of(id)?;
        let (path, dirty, title) = {
            let d = &self.docs[i];
            let Some(path) = d.state.path().map(Path::to_path_buf) else {
                return Err(Error::unsupported(
                    "まだファイルになっていない文書は開き直せません",
                ));
            };
            (path, d.state.is_dirty(), d.title.clone())
        };
        if !matches!(self.docs[i].state.kind(), DocumentKind::File) {
            return Err(Error::unsupported("ファイルではない文書は開き直せません"));
        }
        if dirty {
            let owner = self.hwnd;
            let prompts = self.prompts.clone();
            let ok = prompts.confirm(
                owner,
                "Linote",
                &format!("「{title}」の未保存の変更は、開き直すと失われます。\n開き直しますか？"),
            );
            if !ok {
                return Ok(());
            }
        }
        let stamp = self.docs[i].state.stamp();
        // 開き直しは同じパスを読むので、重複の判定を通らないようにする。
        self.open_inner(&path, Some((stamp, Some(encoding), bom)))
    }

    // ---------------------------------------------------------------- 外部変更

    /// ディスク上のファイルが控えから変わっていないか確かめる。変わっていれば文書に
    /// 記録し（上書き保存を止める）、利用者に知らせる。変化があれば `true`。
    pub fn check_external_change_of(&mut self, id: DocumentId) -> bool {
        let Ok(i) = self.index_of(id) else {
            return false;
        };
        let d = &self.docs[i];
        if d.state.has_external_change() {
            return true;
        }
        if !matches!(d.state.kind(), DocumentKind::File) {
            return false;
        }
        let Some(path) = d.state.path().map(Path::to_path_buf) else {
            return false;
        };
        let known = d.state.disk_stamp();
        let title = d.title.clone();
        match check_external_change(&path, known) {
            Ok(change) if change.is_changed() => {
                let _ = self.update_state(id, |s| s.record_external_change());
                let what = match change {
                    ExternalChange::Removed => "削除または移動されました",
                    _ => "ほかのプログラムで変更されました",
                };
                self.set_status(
                    &format!(
                        "「{title}」が{what}。上書きせず、「名前を付けて保存」で別に保存してください\
                         （「文字コードを指定して開き直す」で読み直すこともできます）"
                    ),
                    MessageKind::Warning,
                );
                true
            }
            _ => false,
        }
    }

    /// 外部変更を知らせ、別名での保存を勧める。
    fn external_change_guidance(&mut self, id: DocumentId, after: After) {
        let Ok(i) = self.index_of(id) else { return };
        let title = self.docs[i].title.clone();
        let owner = self.hwnd;
        let prompts = self.prompts.clone();
        let ok = prompts.confirm(
            owner,
            "Linote - ファイルが変更されています",
            &format!(
                "「{title}」は、開いた後にほかのプログラムで変更（または削除）されました。\n\
                 上書きすると、その変更を失うおそれがあります。\n\n別の名前で保存しますか？"
            ),
        );
        if ok {
            if let Err(e) = self.save_as_flow(id, after) {
                self.report_failure("保存できませんでした", &e);
            }
        }
    }

    // ---------------------------------------------------------------- 保存

    /// 上書き保存、またはまだファイルでない文書なら名前を付けて保存する。
    pub(super) fn save_then(&mut self, id: DocumentId, after: After) {
        let result = (|| {
            let i = self.index_of(id)?;
            let d = &self.docs[i];
            if matches!(d.view, DocView::External(_)) {
                return Err(Error::unsupported(
                    "巨大ファイルは読み取り専用のため保存できません",
                ));
            }
            if matches!(d.state.kind(), DocumentKind::Memo(_)) {
                return if self.save_memo_now(id) {
                    Ok(())
                } else {
                    Err(Error::Busy {
                        what: "メモの保存".into(),
                    })
                };
            }
            if closing::needs_save_as(d.state.path().is_some()) {
                self.save_as_flow(id, after)
            } else {
                self.save_document(id, SaveDest::Current, SaveOpts::new(after))
            }
        })();
        if let Err(e) = result {
            self.report_failure("保存できませんでした", &e);
        }
    }

    /// 保存先を尋ねて名前を付けて保存する（取り消されたら何もしない）。
    pub(super) fn save_as_flow(&mut self, id: DocumentId, after: After) -> Result<()> {
        let i = self.index_of(id)?;
        let suggested = match self.docs[i].state.path() {
            Some(p) => file_title(p),
            None => {
                let t = &self.docs[i].title;
                if t.contains('.') {
                    t.clone()
                } else {
                    format!("{t}.txt")
                }
            }
        };
        let dir = self.docs[i]
            .state
            .path()
            .and_then(Path::parent)
            .map(Path::to_path_buf);
        let owner = self.hwnd;
        let prompts = self.prompts.clone();
        match prompts.save_file(owner, &suggested, dir.as_deref()) {
            Some(path) => self.save_document(id, SaveDest::As(path), SaveOpts::new(after)),
            None => Ok(()),
        }
    }

    /// 保存を始める（ワーカーで書く）。同じ文書の保存が進行中なら始めない。
    pub(super) fn save_document(
        &mut self,
        id: DocumentId,
        dest: SaveDest,
        opts: SaveOpts,
    ) -> Result<()> {
        self.sync();
        let i = self.index_of(id)?;
        if self.docs[i].saving {
            self.set_status("保存中です", MessageKind::Info);
            return Ok(());
        }
        let is_memo = matches!(self.docs[i].state.kind(), DocumentKind::Memo(_));
        let (path, expect) = match &dest {
            SaveDest::Current => {
                if self.docs[i].state.has_external_change() {
                    self.external_change_guidance(id, opts.after);
                    return Ok(());
                }
                self.docs[i].state.require(Capability::Save)?;
                let path = self.docs[i]
                    .state
                    .path()
                    .map(Path::to_path_buf)
                    .ok_or_else(|| Error::unsupported("save"))?;
                // 保存の直前に、ディスク上の状態を確かめる。
                if self.check_external_change_of(id) {
                    self.external_change_guidance(id, opts.after);
                    return Ok(());
                }
                let expect = file::DiskExpectation::from_stamp(self.docs[i].state.disk_stamp());
                (path, expect)
            }
            SaveDest::As(path) => {
                self.docs[i].state.require(Capability::SaveAs)?;
                // 保存先の選択（上書きの確認を済ませたもの）なので、状態は問わない。
                (path.clone(), file::DiskExpectation::Any)
            }
        };
        if self.docs[i].lossy {
            let owner = self.hwnd;
            let prompts = self.prompts.clone();
            let title = self.docs[i].title.clone();
            let ok = prompts.confirm(
                owner,
                "Linote - 保存の確認",
                &format!(
                    "「{title}」には、読み取れなかったバイトを「\u{fffd}」に置き換えた箇所があります。\n\
                     保存すると、その部分は元に戻せません。\n\n保存しますか？"
                ),
            );
            if !ok {
                self.set_status("保存を中止しました", MessageKind::Info);
                return Ok(());
            }
            self.docs[i].lossy = false;
        }
        let (stamp, text) = self.text_snapshot(id)?;
        let format = self.docs[i].state.format;
        self.docs[i].saving = true;
        let export = is_memo && matches!(dest, SaveDest::As(_));
        let sender = self.sender();
        let dest_again = dest.clone();
        let on_unmappable = if opts.replace_unmappable {
            OnUnmappable::Replace
        } else {
            OnUnmappable::Fail
        };
        WorkerPool::global().spawn(move || {
            let ctx = TaskContext::detached();
            let result = file::save_text(
                &SaveRequest {
                    path: &path,
                    text: &text,
                    format,
                    doc: stamp,
                    expect,
                    on_unmappable,
                },
                &ctx,
            );
            sender.send(UiMessage::Run(Box::new(move |app| {
                app.finish_save(id, path, dest_again, export, opts, format, result)
            })));
        });
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn finish_save(
        &mut self,
        id: DocumentId,
        path: PathBuf,
        dest: SaveDest,
        export: bool,
        opts: SaveOpts,
        format: crate::text::TextFormat,
        result: Result<SaveOutcome>,
    ) {
        let exists = match self.index_of(id) {
            Ok(i) => {
                self.docs[i].saving = false;
                true
            }
            Err(_) => false,
        };
        let name = file_title(&path);
        match result {
            Ok(SaveOutcome::Saved(saved)) => {
                if export {
                    self.set_status(&format!("書き出しました: {name}"), MessageKind::Info);
                    return;
                }
                if !exists {
                    self.set_status(&format!("保存しました: {name}"), MessageKind::Info);
                    return;
                }
                if let Err(e) =
                    self.record_saved(id, saved.doc.version, Some(path.clone()), Some(saved.disk))
                {
                    self.report_failure("保存の記録に失敗しました", &e);
                    return;
                }
                self.recovery_saved(id, saved.doc.version);
                self.set_status(&format!("保存しました: {name}"), MessageKind::Info);
                match opts.after {
                    After::Nothing => {}
                    After::Close => {
                        let _ = self.close_document(id, false);
                    }
                    After::ContinueExit => self.request_exit(),
                }
            }
            Ok(SaveOutcome::Unmappable { chars, .. }) => {
                if !exists {
                    return;
                }
                let owner = self.hwnd;
                let prompts = self.prompts.clone();
                let ok = prompts.confirm(
                    owner,
                    "Linote - 変換できない文字",
                    &unmappable_text(&name, format, &chars),
                );
                if ok {
                    let opts = SaveOpts {
                        replace_unmappable: true,
                        ..opts
                    };
                    if let Err(e) = self.save_document(id, dest, opts) {
                        self.report_failure("保存できませんでした", &e);
                    }
                } else {
                    self.set_status(
                        "保存を中止しました（変換できない文字があります。UTF-8 などで保存し直すこともできます）",
                        MessageKind::Warning,
                    );
                }
            }
            Err(Error::ExternalChange { .. }) => {
                if exists {
                    let _ = self.update_state(id, |s| s.record_external_change());
                    self.external_change_guidance(id, opts.after);
                }
            }
            Err(e) => {
                self.report_failure(&format!("「{name}」を保存できませんでした"), &e);
            }
        }
    }

    /// 失敗を状態表示に出し、原因別の文言をダイアログでも知らせる（重大な失敗）。
    pub(super) fn report_failure(&mut self, what: &str, e: &Error) {
        if matches!(e, Error::Cancelled) {
            return;
        }
        let text = format!("{what}: {}", describe_error(e));
        self.set_status(&text, MessageKind::Warning);
    }
}

// ------------------------------------------------------------------ 閉じる・終了

/// 未保存の文書を閉じてよいか（[`super::Hooks::confirm_close`] の既定）。
/// `false` を返したときは閉じない（「保存」を選んだ場合は、保存が終わってから閉じる）。
pub(super) fn confirm_close(app: &mut App, id: DocumentId) -> bool {
    let Some(d) = app.document(id) else {
        return true;
    };
    match closing::plan(d.state.kind(), d.state.is_dirty()) {
        ClosePlan::Close => true,
        ClosePlan::SaveMemoThenClose => app.save_memo_now(id),
        ClosePlan::Ask => {
            let title = d.title.clone();
            let owner = app.hwnd;
            let prompts = app.prompts.clone();
            match closing::act_on(prompts.close_choice(owner, &title)) {
                ChoiceAction::Keep => false,
                ChoiceAction::DiscardAndClose => true,
                ChoiceAction::SaveThenClose => {
                    app.save_then(id, After::Close);
                    false
                }
            }
        }
    }
}

/// 未保存の文書があるときに終了してよいか。文書ごとに確認する。「保存」を選んだら保存を
/// 始めて `false` を返し、保存が終わったら終了を続ける。
pub(super) fn confirm_exit(app: &mut App) -> bool {
    loop {
        let list: Vec<_> = app
            .docs
            .iter()
            .map(|d| {
                (
                    d.id(),
                    d.state.is_dirty(),
                    d.state.kind().clone(),
                    d.state.version(),
                )
            })
            .collect();
        let Some(id) = closing::next_to_ask(&list, &app.exit_discard) else {
            return true;
        };
        let _ = app.activate(id);
        let (title, version) = match app.document(id) {
            Some(d) => (d.title.clone(), d.state.version()),
            None => continue,
        };
        let owner = app.hwnd;
        let prompts = app.prompts.clone();
        match closing::act_on(prompts.close_choice(owner, &title)) {
            ChoiceAction::Keep => {
                app.exit_discard.clear();
                return false;
            }
            ChoiceAction::DiscardAndClose => {
                app.exit_discard.insert(id, version);
            }
            ChoiceAction::SaveThenClose => {
                app.save_then(id, After::ContinueExit);
                return false;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "linote-fileio-{tag}-{}-{}",
            std::process::id(),
            crate::core::MemoId::generate()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn missing_and_valid_settings_are_writable() {
        let dir = temp_dir("settings-ok");
        let path = dir.join("settings.json");
        let (s, state) = load_settings_from(&path);
        assert_eq!(state, SettingsState::Missing);
        assert!(state.writable());
        assert_eq!(s, Settings::default());
        std::fs::write(&path, r#"{"theme":"dark"}"#).unwrap();
        let (s, state) = load_settings_from(&path);
        assert_eq!(state, SettingsState::Loaded);
        assert_eq!(s.theme, crate::appdata::ThemeMode::Dark);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn corrupt_settings_are_reported_and_left_alone() {
        let dir = temp_dir("settings-bad");
        let path = dir.join("settings.json");
        for bad in [&b"{ not json"[..], &b"\xff\xfe"[..], &b"[]"[..]] {
            std::fs::write(&path, bad).unwrap();
            let (s, state) = load_settings_from(&path);
            assert!(matches!(state, SettingsState::Broken(_)));
            assert!(!state.writable());
            assert_eq!(s, Settings::default());
            // 読むだけではファイルに触れない。
            assert_eq!(std::fs::read(&path).unwrap(), bad);
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn errors_are_described_by_cause() {
        let e = Error::io(
            "読み込み",
            "x.txt",
            std::io::Error::from(std::io::ErrorKind::NotFound),
        );
        assert!(describe_error(&e).starts_with("ファイルまたはフォルダーが見つかりません"));
        assert_eq!(
            describe_error(&Error::Cancelled),
            Error::Cancelled.to_string()
        );
    }
}
