//! ファイル操作の補助（U1）: 最近使ったファイル、すべて閉じる、新しいウィンドウ、
//! 保存時の文字コード・改行の変更、フルパスのコピー、エクスプローラーで表示、
//! ファイルのドラッグ＆ドロップ。
//!
//! 画面に依存しない判断（表示名の短縮、改行・文字コードの名前）は下の純粋な関数に置いて
//! 試験する。

use std::path::{Path, PathBuf};

use windows::Win32::UI::Shell::{DragFinish, DragQueryFileW, HDROP};

use crate::core::{Actor, DocumentId, Error, Result};
use crate::document::DocumentKind;
use crate::platform::clipboard;
use crate::text::{Encoding, Eol, TextFormat};
use crate::ui::dialogs::NoticeKind;
use crate::ui::statusbar::MessageKind;

use super::{App, DocView};

/// 最近使ったファイルのメニュー項目の表示名（`&` は `&&` に、長いパスは中ほどを省く）。
pub(super) fn recent_menu_label(number: usize, path: &str) -> String {
    let shown = shorten_path(path, 60).replace('&', "&&");
    if number <= 9 {
        format!("&{number} {shown}")
    } else {
        format!("{number} {shown}")
    }
}

/// パスが `max` 文字を超えるときは、先頭と末尾を残して中ほどを「...」にする。
pub(super) fn shorten_path(path: &str, max: usize) -> String {
    let chars: Vec<char> = path.chars().collect();
    if chars.len() <= max || max < 8 {
        return path.to_owned();
    }
    let tail = max * 2 / 3;
    let head = max - tail - 3;
    let head_s: String = chars[..head].iter().collect();
    let tail_s: String = chars[chars.len() - tail..].iter().collect();
    format!("{head_s}...{tail_s}")
}

/// コマンド引数の文字コード名から（文字コード, BOM）。
pub(super) fn parse_encoding(name: &str) -> Option<(Encoding, bool)> {
    Some(match name {
        "utf-8" => (Encoding::Utf8, false),
        "utf-8-bom" => (Encoding::Utf8, true),
        // UTF-16 は Windows の慣例どおり BOM を付ける。
        "utf-16le" => (Encoding::Utf16Le, true),
        "utf-16be" => (Encoding::Utf16Be, true),
        "cp932" => (Encoding::Cp932, false),
        _ => return None,
    })
}

/// コマンド引数の改行名から改行。
pub(super) fn parse_eol(name: &str) -> Option<Eol> {
    Some(match name {
        "crlf" => Eol::CrLf,
        "lf" => Eol::Lf,
        "cr" => Eol::Cr,
        _ => return None,
    })
}

impl App {
    // ---------------------------------------------------------------- 最近使ったファイル

    /// 最近使ったファイルの先頭に加え、設定に保存する（存在確認はしない）。
    pub(super) fn add_recent(&mut self, path: &Path) {
        let text = path.to_string_lossy().into_owned();
        if self.settings.recent_files.first() == Some(&text) {
            return;
        }
        self.settings.push_recent(text);
        // 壊れた設定ファイルは上書きしない（一覧は今回の起動の間だけ保つ）。
        if self.settings_state.writable() {
            self.persist_settings();
        }
    }

    /// 最近使ったファイルの `index` 番目（1 始まり）を開く。なければ案内して一覧から外す。
    pub(super) fn open_recent(&mut self, index: u64) -> Result<()> {
        let Some(path) = index
            .checked_sub(1)
            .and_then(|i| self.settings.recent_files.get(i as usize))
            .cloned()
        else {
            return Err(Error::invalid_argument(
                "index",
                "最近使ったファイルにその番号はありません",
            ));
        };
        let p = PathBuf::from(&path);
        if !p.is_file() {
            self.settings.remove_recent(&path);
            if self.settings_state.writable() {
                self.persist_settings();
            }
            self.notify(
                NoticeKind::Warning,
                &format!("ファイルが見つかりません。一覧から外しました:\n{path}"),
            );
            return Ok(());
        }
        self.open_path(&p);
        Ok(())
    }

    // ---------------------------------------------------------------- 閉じる

    /// すべてのタブを閉じる。未保存の文書は 1 つずつ確認し、取り消し（または保存の開始）で
    /// そこで止める。閉じた数を返す。
    pub(super) fn close_all_documents(&mut self) -> usize {
        let ids: Vec<DocumentId> = self.docs.iter().map(|d| d.id()).collect();
        self.close_each(ids)
    }

    /// 指定した文書以外を閉じる。
    pub(super) fn close_other_documents(&mut self, keep: DocumentId) -> usize {
        let ids: Vec<DocumentId> = self
            .docs
            .iter()
            .map(|d| d.id())
            .filter(|&id| id != keep)
            .collect();
        let n = self.close_each(ids);
        let _ = self.activate(keep);
        n
    }

    pub(super) fn close_each(&mut self, ids: Vec<DocumentId>) -> usize {
        let mut closed = 0;
        for id in ids {
            match self.close_document(id, false) {
                Ok(true) => closed += 1,
                Ok(false) => break,
                // 既に閉じている文書は飛ばす。
                Err(_) => continue,
            }
        }
        closed
    }

    // ---------------------------------------------------------------- 新しいウィンドウ

    /// 新しいプロセスで Linote を起動する（ウィンドウは 1 プロセスに 1 つ）。
    pub(super) fn spawn_new_window(&mut self) -> Result<()> {
        let exe = std::env::current_exe().map_err(|e| Error::io("起動", Path::new("linote"), e))?;
        std::process::Command::new(&exe)
            .spawn()
            .map_err(|e| Error::io("新しいウィンドウの起動", &exe, e))?;
        Ok(())
    }

    // ---------------------------------------------------------------- 保存時の形式

    fn require_format_target(&self, id: DocumentId) -> Result<()> {
        let i = self.index_of(id)?;
        let d = &self.docs[i];
        let ok = matches!(
            d.state.kind(),
            DocumentKind::File | DocumentKind::Untitled | DocumentKind::Clipboard
        ) && matches!(d.view, DocView::Scintilla(_));
        if ok {
            Ok(())
        } else {
            Err(Error::unsupported(
                "この文書では保存時の文字コード・改行を変更できません（メモは UTF-8、巨大ファイルは読み取り専用）",
            ))
        }
    }

    /// 保存時の文字コードを変える。本文は変わらないが、保存するまで未保存の変更として扱う。
    pub(super) fn set_save_encoding(
        &mut self,
        id: DocumentId,
        encoding: Encoding,
        bom: bool,
    ) -> Result<()> {
        self.require_format_target(id)?;
        let before = self.state(id).map_or(TextFormat::DEFAULT, |s| s.format);
        let changed = before.encoding != encoding || before.bom != bom;
        self.update_state(id, |s| {
            s.format.encoding = encoding;
            s.format.bom = bom;
            if changed {
                s.record_edit(Actor::User);
            }
        })?;
        let label = self.state(id).map(|s| s.format.label()).unwrap_or_default();
        self.set_status(
            &format!("保存時の文字コード: {label}（保存すると反映されます）"),
            MessageKind::Info,
        );
        Ok(())
    }

    /// 保存時の改行を変える。本文の改行をすべてその形式に変換する（Undo できる）。
    pub(super) fn set_save_eol(&mut self, id: DocumentId, eol: Eol) -> Result<()> {
        self.require_format_target(id)?;
        let differs = self.state(id).is_some_and(|s| s.format.eol != eol);
        self.with_text(id, |ed| {
            ed.convert_eols(eol);
            ed.set_eol(eol);
        })?;
        self.update_state(id, |s| {
            s.format.eol = eol;
            if differs {
                s.record_edit(Actor::User);
            }
        })?;
        self.set_status(
            &format!("保存時の改行: {}（保存すると反映されます）", eol.label()),
            MessageKind::Info,
        );
        Ok(())
    }

    // ---------------------------------------------------------------- パス

    pub(super) fn path_of(&self, id: DocumentId) -> Result<PathBuf> {
        let i = self.index_of(id)?;
        self.docs[i]
            .state
            .path()
            .map(Path::to_path_buf)
            .ok_or_else(|| Error::unsupported("まだファイルになっていない文書です"))
    }

    /// フルパスをクリップボードへ。
    pub(super) fn copy_full_path(&mut self, id: DocumentId) -> Result<()> {
        let path = self.path_of(id)?;
        let text = path.to_string_lossy().into_owned();
        clipboard::write_text(Some(self.hwnd), &text)?;
        self.set_status("フルパスをコピーしました", MessageKind::Info);
        Ok(())
    }

    /// エクスプローラーでファイルを選択した状態で開く。
    pub(super) fn reveal_in_explorer(&mut self, id: DocumentId) -> Result<()> {
        use std::os::windows::process::CommandExt;
        let path = self.path_of(id)?;
        std::process::Command::new("explorer.exe")
            .raw_arg(format!("/select,\"{}\"", path.display()))
            .spawn()
            .map_err(|e| Error::io("エクスプローラーの起動", &path, e))?;
        Ok(())
    }

    // ---------------------------------------------------------------- ドラッグ＆ドロップ

    /// `WM_DROPFILES`: 落とされたファイルを開く。フォルダーは開かず状態表示で知らせる。
    pub(super) fn on_drop_files(&mut self, hdrop: HDROP) {
        let paths = dropped_paths(hdrop);
        // SAFETY: WM_DROPFILES で受け取ったハンドルを、読み終えたので解放する。
        unsafe { DragFinish(hdrop) };
        self.open_dropped(&paths);
    }

    /// 落とされたパスを開く（開いているファイルはそのタブへ移る）。
    pub fn open_dropped(&mut self, paths: &[PathBuf]) {
        let mut skipped = Vec::new();
        for p in paths {
            if p.is_dir() {
                skipped.push(
                    p.file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_else(|| p.display().to_string()),
                );
                continue;
            }
            self.open_path(p);
        }
        if !skipped.is_empty() {
            self.set_status(
                &format!(
                    "フォルダーは開けません（無視しました）: {}",
                    skipped.join("、")
                ),
                MessageKind::Warning,
            );
        }
    }
}

/// `HDROP` に入っているパスをすべて取り出す。
fn dropped_paths(hdrop: HDROP) -> Vec<PathBuf> {
    // SAFETY: 有効な HDROP。0xFFFFFFFF は個数の問い合わせ。
    let count = unsafe { DragQueryFileW(hdrop, u32::MAX, None) };
    let mut out = Vec::new();
    for i in 0..count {
        // SAFETY: 長さの問い合わせ（バッファーなし）。
        let len = unsafe { DragQueryFileW(hdrop, i, None) } as usize;
        let mut buf = vec![0u16; len + 1];
        // SAFETY: len + 1 要素のバッファーに書き込ませる。
        let n = unsafe { DragQueryFileW(hdrop, i, Some(&mut buf)) } as usize;
        out.push(PathBuf::from(crate::platform::system::os_from_wide(
            &buf[..n.min(buf.len())],
        )));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recent_labels_have_mnemonics_and_escape_ampersands() {
        assert_eq!(recent_menu_label(1, "C:\\a\\b.txt"), "&1 C:\\a\\b.txt");
        assert_eq!(recent_menu_label(9, "C:\\a&b.txt"), "&9 C:\\a&&b.txt");
        assert_eq!(recent_menu_label(10, "C:\\x.txt"), "10 C:\\x.txt");
    }

    #[test]
    fn long_paths_are_shortened_in_the_middle() {
        let long = format!("C:\\{}\\file.txt", "d".repeat(100));
        let s = shorten_path(&long, 60);
        assert_eq!(s.chars().count(), 60);
        assert!(s.contains("..."));
        assert!(s.starts_with("C:\\"));
        assert!(s.ends_with("file.txt"));
        assert_eq!(shorten_path("C:\\short.txt", 60), "C:\\short.txt");
    }

    #[test]
    fn format_names_parse() {
        assert_eq!(parse_encoding("utf-8"), Some((Encoding::Utf8, false)));
        assert_eq!(parse_encoding("utf-8-bom"), Some((Encoding::Utf8, true)));
        assert_eq!(parse_encoding("utf-16le"), Some((Encoding::Utf16Le, true)));
        assert_eq!(parse_encoding("cp932"), Some((Encoding::Cp932, false)));
        assert_eq!(parse_encoding("x"), None);
        assert_eq!(parse_eol("crlf"), Some(Eol::CrLf));
        assert_eq!(parse_eol("cr"), Some(Eol::Cr));
        assert_eq!(parse_eol("LF"), None);
    }
}
