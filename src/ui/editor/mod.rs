//! Scintilla の包み。Scintilla への呼び出し（[`crate::sci_sys`]、`SendMessage`）は
//! このモジュールに集める。
//!
//! - コードページは UTF-8。位置は [`crate::document::TextRange`] と同じバイト位置。
//! - [`crate::document::EditTarget`] を実装する（`begin_undo_group` は
//!   `SCI_BEGINUNDOACTION`）。
//! - `SCN_MODIFIED`（`SC_MOD_INSERTTEXT`／`SC_MOD_DELETETEXT`）を受けるたびに
//!   文書の版を進める。`SCN_SAVEPOINTREACHED` では
//!   [`crate::document::DocumentState::record_unmodified`] を呼ぶ。
//!   （通知を [`EditorEvent`] に読み替えるのがここ、版を進めるのは [`crate::ui::app`]。）
//! - 全文の写しを常時持たない。保存・復元・変換のために写しを作るときは、
//!   その時点の [`crate::core::DocStamp`] と組にしてバックグラウンドへ渡す。
//! - 1 つの編集ウィンドウで複数の文書（タブ）を扱う。文書ごとに Scintilla の
//!   ドキュメント（[`SciDoc`]、`SCI_CREATEDOCUMENT`）を作り、`SCI_SETDOCPOINTER` で
//!   切り替える。Undo の履歴、保存位置、字句解析器、読み取り専用、改行形式は
//!   ドキュメント側に残る。選択とスクロール位置は [`ViewState`] で控えて戻す。
//!
//! 担当: タスク B。

mod bookmarks;
pub mod language;
pub mod printing;
pub mod sci;
mod textcmds;
pub mod transform;
mod viewopts;

use std::cell::RefCell;
use std::ffi::{c_char, CString};
use std::sync::OnceLock;

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, WPARAM};
use windows::Win32::Graphics::Gdi::{
    EnumFontFamiliesExW, GetDC, ReleaseDC, DEFAULT_CHARSET, LOGFONTW, TEXTMETRICW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DestroyWindow, SendMessageW, HMENU, WINDOW_EX_STYLE, WS_CHILD,
    WS_CLIPSIBLINGS, WS_HSCROLL, WS_TABSTOP, WS_VISIBLE, WS_VSCROLL,
};

use crate::appdata::settings::ViewSettings;
use crate::core::{Error, Result};
use crate::document::{EditTarget, LineCount, Selection, TextRange, TextSource};
use crate::text::Eol;
use crate::ui::theme::{Theme, Token};

pub use language::Language;
use sci::*;

/// 本文の書体の候補（先に見つかったものを使う）。日本語の字形は DirectWrite の
/// 代替書体で補われる。
const FONT_CANDIDATES: [&str; 3] = ["Cascadia Mono", "Consolas", "MS Gothic"];

/// Scintilla のウィンドウクラスを登録する（プロセスで 1 回。2 回目以降は何もしない）。
pub fn register_classes(instance: HINSTANCE) -> Result<()> {
    static REGISTERED: OnceLock<bool> = OnceLock::new();
    // SAFETY: インスタンスハンドルを渡すだけ。
    let ok = *REGISTERED
        .get_or_init(|| unsafe { crate::sci_sys::Scintilla_RegisterClasses(instance.0) != 0 });
    if ok {
        Ok(())
    } else {
        Err(Error::unavailable(
            "編集部品（Scintilla）",
            "ウィンドウクラスを登録できませんでした",
        ))
    }
}

/// Scintilla のドキュメント（参照カウント付きの本文）。
///
/// 所有者（`ui::app` の文書一覧）が参照を 1 つ持ち、閉じるときに
/// [`Editor::release_document`] で返す。`Drop` では返さない（編集ウィンドウが要るため）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SciDoc(isize);

impl SciDoc {
    pub fn as_raw(self) -> isize {
        self.0
    }
}

/// 選択とスクロール位置の控え（タブを切り替えるときに保存・復元する）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ViewState {
    pub selection: Selection,
    /// 矩形選択だったか（復元では矩形の両端だけを戻す）。
    pub rectangular: bool,
    pub first_visible_line: u64,
    pub x_offset: i32,
}

/// Scintilla の通知を読み替えたもの。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditorEvent {
    /// 本文が変わった（挿入または削除）。版を 1 つ進める。
    Modified {
        inserted: bool,
        position: u64,
        length: u64,
        lines_added: i64,
        /// Undo／Redo による変更。
        undo_redo: bool,
    },
    /// 保存位置に戻った（Undo で保存時点の内容になった、または保存位置を設定した）。
    SavePointReached,
    /// 保存位置から離れた。
    SavePointLeft,
    /// 表示の更新（選択の移動・内容の変更）。ステータスバーを更新する。
    UpdateUi {
        selection: bool,
        content: bool,
    },
    FocusIn,
    FocusOut,
    /// 文字が入力された（自動インデントに使う）。`ch` は入力された文字（改行は `\n`／`\r`）。
    CharAdded {
        ch: u32,
    },
    /// 拡大率が変わった（Ctrl+ホイールなど）。
    Zoom,
}

/// `WM_NOTIFY` の `lParam` を読み、Scintilla の通知なら（送り元、読み替えた通知）を返す。
///
/// # Safety
/// `lparam` は `WM_NOTIFY` で受け取った値（`NMHDR` へのポインタ）であること。
pub unsafe fn parse_notification(lparam: LPARAM) -> Option<(HWND, EditorEvent)> {
    if lparam.0 == 0 {
        return None;
    }
    // SAFETY: WM_NOTIFY の lParam は NMHDR を先頭に持つ構造体を指す。Scintilla の
    // 通知かどうかはコードで判断し、Scintilla のものだけ SCNotification として読む。
    let header = unsafe { &*(lparam.0 as *const NotifyHeader) };
    let from = HWND(header.hwnd_from);
    let event = match header.code {
        SCN_MODIFIED => {
            // SAFETY: 上の説明のとおり。
            let n = unsafe { &*(lparam.0 as *const ScNotification) };
            let t = n.modification_type;
            if t & (SC_MOD_INSERTTEXT | SC_MOD_DELETETEXT) == 0 {
                return None;
            }
            EditorEvent::Modified {
                inserted: t & SC_MOD_INSERTTEXT != 0,
                position: n.position.max(0) as u64,
                length: n.length.max(0) as u64,
                lines_added: n.lines_added as i64,
                undo_redo: t & (SC_PERFORMED_UNDO | SC_PERFORMED_REDO) != 0,
            }
        }
        SCN_SAVEPOINTREACHED => EditorEvent::SavePointReached,
        SCN_SAVEPOINTLEFT => EditorEvent::SavePointLeft,
        SCN_UPDATEUI => {
            // SAFETY: 上の説明のとおり。
            let n = unsafe { &*(lparam.0 as *const ScNotification) };
            EditorEvent::UpdateUi {
                selection: n.updated & SC_UPDATE_SELECTION != 0,
                content: n.updated & SC_UPDATE_CONTENT != 0,
            }
        }
        SCN_CHARADDED => {
            // SAFETY: 上の説明のとおり。
            let n = unsafe { &*(lparam.0 as *const ScNotification) };
            EditorEvent::CharAdded {
                ch: n.ch.max(0) as u32,
            }
        }
        SCN_ZOOM => EditorEvent::Zoom,
        SCN_FOCUSIN => EditorEvent::FocusIn,
        SCN_FOCUSOUT => EditorEvent::FocusOut,
        _ => return None,
    };
    Some((from, event))
}

/// Scintilla の編集ウィンドウ 1 つ。UI スレッド専用。
///
/// 操作の対象は、その時点で表示中のドキュメント（[`Editor::document`]）。
#[derive(Debug)]
pub struct Editor {
    hwnd: HWND,
    /// 適用中の表示設定（書体、折り返し桁、タブ幅など）。
    view: RefCell<ViewSettings>,
}

impl Editor {
    /// 子ウィンドウとして作り、共通の設定（UTF-8、IME、余白、DirectWrite など）をする。
    /// 先に [`register_classes`] を呼んでおくこと。
    pub fn create(parent: HWND, instance: HINSTANCE, id: isize, visible: bool) -> Result<Editor> {
        let mut style = WS_CHILD | WS_CLIPSIBLINGS | WS_TABSTOP | WS_VSCROLL | WS_HSCROLL;
        if visible {
            style |= WS_VISIBLE;
        }
        // SAFETY: 引数はすべて有効な値。失敗は Err で返る。
        let hwnd = unsafe {
            CreateWindowExW(
                WINDOW_EX_STYLE::default(),
                w!("Scintilla"),
                PCWSTR::null(),
                style,
                0,
                0,
                0,
                0,
                Some(parent),
                Some(HMENU(id as *mut _)),
                Some(instance),
                None,
            )
        }
        .map_err(|e| Error::unavailable("編集欄", format!("作れませんでした: {e}")))?;
        let editor = Editor {
            hwnd,
            view: RefCell::new(ViewSettings::default()),
        };
        editor.configure();
        Ok(editor)
    }

    pub fn hwnd(&self) -> HWND {
        self.hwnd
    }

    /// ウィンドウを破棄する。表示中のドキュメントの参照は Scintilla が返す。
    pub fn destroy(self) {
        // SAFETY: 自分が作ったウィンドウ。
        unsafe {
            let _ = DestroyWindow(self.hwnd);
        }
    }

    #[inline]
    fn send(&self, msg: u32, wparam: usize, lparam: isize) -> isize {
        // SAFETY: Scintilla のウィンドウへの同期メッセージ。ポインタを渡す呼び出しは
        // 各メソッドで有効な領域を渡している。
        unsafe { SendMessageW(self.hwnd, msg, Some(WPARAM(wparam)), Some(LPARAM(lparam))).0 }
    }

    fn configure(&self) {
        self.configure_match_indicators();
        self.configure_bookmarks();
        // DirectWrite: DPI に沿った描画と、日本語の字形の代替書体。Wine の DirectWrite では
        // 何も描かれないので、Wine では GDI にする（試験環境のため）。
        let technology = if crate::platform::system::is_wine() {
            SC_TECHNOLOGY_DEFAULT
        } else {
            SC_TECHNOLOGY_DIRECTWRITE
        };
        self.send(SCI_SETTECHNOLOGY, technology, 0);
        self.send(SCI_SETCODEPAGE, SC_CP_UTF8, 0);
        // 日本語 IME: 変換中の文字列を本文の位置にそのまま表示する。
        self.send(SCI_SETIMEINTERACTION, SC_IME_INLINE, 0);
        // 既定の右クリックメニュー（英語）は使わない。WM_CONTEXTMENU が親に届き、
        // アプリが日本語のメニューを出す。
        self.send(SCI_USEPOPUP, 0 /* SC_POPUP_NEVER */, 0);
        self.send(
            SCI_SETMODEVENTMASK,
            (SC_MOD_INSERTTEXT | SC_MOD_DELETETEXT) as usize,
            0,
        );
        // 矩形選択（Alt＋ドラッグ、Alt＋Shift＋矢印）と、その上での一括入力。
        self.send(SCI_SETMULTIPLESELECTION, 1, 0);
        self.send(SCI_SETADDITIONALSELECTIONTYPING, 1, 0);
        self.send(SCI_SETRECTANGULARSELECTIONMODIFIER, SCMOD_ALT, 0);
        self.send(SCI_SETMOUSESELECTIONRECTANGULARSWITCH, 1, 0);
        self.send(SCI_SETVIRTUALSPACEOPTIONS, SCVS_RECTANGULARSELECTION, 0);
        // 余白: 0 番を行番号、1 番をブックマークに使う。
        self.send(SCI_SETMARGINTYPEN, 0, SC_MARGIN_NUMBER);
        self.send(SCI_SETMARGINLEFT, 0, 4);
        self.send(SCI_SETTABWIDTH, 4, 0);
        // 横スクロールバーは、表示した行が幅を超えたときだけ出す（既定の 2000 px のままだと、
        // 短い文書でも常に出て、Windows 11 では左下に細い線として見える）。
        self.send(SCI_SETSCROLLWIDTH, 1, 0);
        self.send(SCI_SETSCROLLWIDTHTRACKING, 1, 0);
        self.send(SCI_SETLAYOUTCACHE, SC_CACHE_PAGE, 0);
        // 着色は見えている範囲を優先し、残りは空き時間に行う。
        self.send(SCI_SETIDLESTYLING, SC_IDLESTYLING_TOVISIBLE, 0);
        self.send(SCI_SETCARETSTYLE, 1, 0);
        self.send(SCI_SETENDATLASTLINE, 1, 0);
    }

    // ---------------------------------------------------------------- 文書の切り替え

    /// 空のドキュメントを作る（参照 1 つを呼び出し側が持つ）。`bytes_hint` は
    /// 予想される大きさ（確保の目安）。
    ///
    /// **注意**: Scintilla 5.4.1 の `SCI_CREATEDOCUMENT`／`SCI_CREATELOADER` は、呼んだ
    /// 編集ウィンドウの表示中の文書の折り畳み状態を作り直してしまう（行が表示されなくなる）。
    /// 表示用の編集欄では呼ばず、文書を作るための隠れた編集欄で呼ぶこと
    /// （`ui::app` はそうしている）。
    pub fn create_document(&self, bytes_hint: u64) -> Result<SciDoc> {
        let options = if bytes_hint > i32::MAX as u64 {
            SC_DOCUMENTOPTION_TEXT_LARGE
        } else {
            SC_DOCUMENTOPTION_DEFAULT
        };
        let doc = self.send(
            SCI_CREATEDOCUMENT,
            bytes_hint.min(isize::MAX as u64) as usize,
            options,
        );
        if doc == 0 {
            Err(Error::Internal("Scintilla の文書を作れませんでした".into()))
        } else {
            Ok(SciDoc(doc))
        }
    }

    /// 本文をまとめて流し込むための読み込み口を作る（[`DocLoader`]）。
    pub fn create_loader(&self, bytes_hint: u64) -> Result<DocLoader> {
        let options = if bytes_hint > i32::MAX as u64 {
            SC_DOCUMENTOPTION_TEXT_LARGE
        } else {
            SC_DOCUMENTOPTION_DEFAULT
        };
        let p = self.send(
            SCI_CREATELOADER,
            bytes_hint.min(isize::MAX as u64) as usize,
            options,
        );
        if p == 0 {
            Err(Error::Internal(
                "Scintilla の読み込み口を作れませんでした".into(),
            ))
        } else {
            Ok(DocLoader {
                loader: p as *mut ILoader,
            })
        }
    }

    /// 表示中のドキュメント。
    pub fn document(&self) -> SciDoc {
        SciDoc(self.send(SCI_GETDOCPOINTER, 0, 0))
    }

    /// 表示するドキュメントを切り替える。Scintilla が参照を 1 つ足し、前のドキュメントの
    /// 参照を 1 つ返す。切り替えると選択とスクロールは先頭に戻る（[`ViewState`] で戻す）。
    pub fn set_document(&self, doc: SciDoc) {
        self.send(SCI_SETDOCPOINTER, 0, doc.0);
        // 前の文書の長い行で広がった横スクロールの幅を持ち越さない（表示に合わせて広がる）。
        self.send(SCI_SETSCROLLWIDTH, 1, 0);
        // コードページはドキュメントごとの設定。読み込み口で作った文書にも必ず設定する。
        self.send(SCI_SETCODEPAGE, SC_CP_UTF8, 0);
        // タブ幅などはドキュメント側の設定なので、切り替えのたびに合わせる。
        self.apply_document_options();
    }

    pub fn add_ref_document(&self, doc: SciDoc) {
        self.send(SCI_ADDREFDOCUMENT, 0, doc.0);
    }

    /// 参照を 1 つ返す。表示中のドキュメントの参照を返す場合は、先に別のドキュメントに
    /// 切り替えておくこと（表示中の参照は Scintilla が持っているので壊れはしない）。
    pub fn release_document(&self, doc: SciDoc) {
        self.send(SCI_RELEASEDOCUMENT, 0, doc.0);
    }

    /// 表示中のドキュメントを手放し、新しい空のドキュメントにする
    /// （隠れた編集欄が、閉じる文書の参照を持ち続けないようにするため）。
    pub fn detach_document(&self) {
        self.send(SCI_SETDOCPOINTER, 0, 0);
        self.send(SCI_SETCODEPAGE, SC_CP_UTF8, 0);
    }

    pub fn view_state(&self) -> ViewState {
        let rectangular = self.send(SCI_GETSELECTIONMODE, 0, 0) as usize == SC_SEL_RECTANGLE;
        let selection = if rectangular {
            Selection {
                anchor: self.send(SCI_GETRECTANGULARSELECTIONANCHOR, 0, 0) as u64,
                caret: self.send(SCI_GETRECTANGULARSELECTIONCARET, 0, 0) as u64,
            }
        } else {
            self.selection()
        };
        ViewState {
            selection,
            rectangular,
            first_visible_line: self.send(SCI_GETFIRSTVISIBLELINE, 0, 0) as u64,
            x_offset: self.send(SCI_GETXOFFSET, 0, 0) as i32,
        }
    }

    pub fn restore_view_state(&self, state: &ViewState) {
        if state.rectangular {
            self.set_rectangular_selection(state.selection.anchor, state.selection.caret);
        } else {
            self.set_selection(state.selection);
        }
        self.send(
            SCI_SETFIRSTVISIBLELINE,
            state.first_visible_line as usize,
            0,
        );
        self.send(SCI_SETXOFFSET, state.x_offset.max(0) as usize, 0);
    }

    // ---------------------------------------------------------------- 本文

    /// 本文の長さ（バイト）。
    pub fn length(&self) -> u64 {
        self.send(SCI_GETLENGTH, 0, 0).max(0) as u64
    }

    /// 範囲のバイト列の写し。範囲は本文の内側に切り詰める。
    pub fn text_range(&self, start: u64, end: u64) -> Vec<u8> {
        let len = self.length();
        let (start, end) = (start.min(len), end.min(len));
        if end <= start {
            return Vec::new();
        }
        let n = (end - start) as usize;
        let mut buf = vec![0u8; n + 1];
        let mut range = TextRangeFull {
            cp_min: start as isize,
            cp_max: end as isize,
            text: buf.as_mut_ptr() as *mut c_char,
        };
        self.send(
            SCI_GETTEXTRANGEFULL,
            0,
            &mut range as *mut TextRangeFull as isize,
        );
        buf.truncate(n);
        buf
    }

    /// 全文の写し（UTF-8）。保存・復元などで一時的に使う。常時持たないこと。
    pub fn text(&self) -> String {
        let bytes = self.text_range(0, self.length());
        String::from_utf8(bytes)
            .unwrap_or_else(|e| String::from_utf8_lossy(e.as_bytes()).into_owned())
    }

    /// 全文を置き換える（Undo できる 1 回の変更）。NUL を含む本文も扱える。
    pub fn set_text(&self, text: &str) {
        self.replace_raw(0, self.length(), text.as_bytes());
    }

    fn replace_raw(&self, start: u64, end: u64, bytes: &[u8]) {
        self.send(SCI_SETTARGETRANGE, start as usize, end as isize);
        self.send(SCI_REPLACETARGET, bytes.len(), bytes.as_ptr() as isize);
    }

    fn byte_at(&self, pos: u64) -> u8 {
        self.send(SCI_GETCHARAT, pos as usize, 0) as u8
    }

    /// `pos` が UTF-8 の文字の境界か（末尾も境界）。
    pub fn is_char_boundary(&self, pos: u64) -> bool {
        pos == 0 || pos >= self.length() || (self.byte_at(pos) & 0xC0) != 0x80
    }

    // ---------------------------------------------------------------- 行

    pub fn line_count(&self) -> u64 {
        self.send(SCI_GETLINECOUNT, 0, 0).max(1) as u64
    }

    /// 位置を含む行（0 始まり）。
    pub fn line_from_position(&self, pos: u64) -> u64 {
        self.send(SCI_LINEFROMPOSITION, pos as usize, 0).max(0) as u64
    }

    /// 行（0 始まり）の先頭の位置。行がなければ `None`。
    pub fn line_start(&self, line: u64) -> Option<u64> {
        if line >= self.line_count() {
            return None;
        }
        let p = self.send(SCI_POSITIONFROMLINE, line as usize, 0);
        (p >= 0).then_some(p as u64)
    }

    /// 行の終わり（改行の手前）の位置。
    pub fn line_end(&self, line: u64) -> u64 {
        self.send(SCI_GETLINEENDPOSITION, line as usize, 0).max(0) as u64
    }

    /// 文書全体の文字数（コードポイント。改行の CR・LF は 1 文字ずつ）。`max_bytes` を
    /// 超える文書は数えない（`None`）。初回に文字の索引（行ごとの文字位置）を作り、以後は
    /// Scintilla が編集に合わせて保つので、毎回は数え直さない。
    pub fn total_characters(&self, max_bytes: u64) -> Option<u64> {
        let len = self.send(SCI_GETLENGTH, 0, 0).max(0) as u64;
        if len > max_bytes {
            return None;
        }
        if self.send(SCI_GETLINECHARACTERINDEX, 0, 0) & SC_LINECHARACTERINDEX_UTF32 as isize == 0 {
            self.send(
                SCI_ALLOCATELINECHARACTERINDEX,
                SC_LINECHARACTERINDEX_UTF32,
                0,
            );
        }
        let last = self.send(SCI_GETLINECOUNT, 0, 0).max(1) as usize - 1;
        let before = self
            .send(
                SCI_INDEXPOSITIONFROMLINE,
                last,
                SC_LINECHARACTERINDEX_UTF32 as isize,
            )
            .max(0) as u64;
        let start = self.send(SCI_POSITIONFROMLINE, last, 0).max(0) as u64;
        Some(before + self.count_characters(start, len))
    }

    /// 範囲の文字数（写しを作らずに数える）。
    pub fn count_characters(&self, start: u64, end: u64) -> u64 {
        self.send(SCI_COUNTCHARACTERS, start as usize, end as isize)
            .max(0) as u64
    }

    /// 位置の行内の文字数（0 始まり。画面の「列」は +1 する）。
    pub fn column_chars(&self, pos: u64) -> u64 {
        let line = self.line_from_position(pos);
        let start = self.line_start(line).unwrap_or(0);
        self.send(SCI_COUNTCHARACTERS, start as usize, pos as isize)
            .max(0) as u64
    }

    /// 行（0 始まり）へ移動し、カーソルを行頭に置いて見える位置へスクロールする。
    /// 範囲外は最終行に切り詰める。
    pub fn goto_line(&self, line: u64) {
        let line = line.min(self.line_count() - 1);
        self.send(SCI_ENSUREVISIBLE, line as usize, 0);
        self.send(SCI_GOTOLINE, line as usize, 0);
    }

    // ---------------------------------------------------------------- 選択

    pub fn selection(&self) -> Selection {
        Selection {
            anchor: self.send(SCI_GETANCHOR, 0, 0).max(0) as u64,
            caret: self.send(SCI_GETCURRENTPOS, 0, 0).max(0) as u64,
        }
    }

    /// 通常の選択にし、カーソルが見えるようにスクロールする。
    pub fn set_selection(&self, selection: Selection) {
        let len = self.length();
        // SCI_SETSEL は通常の選択に戻す（SCI_SETSELECTIONMODE は以後のカーソル移動で
        // 選択を伸ばす状態にしてしまうので使わない）。
        self.send(
            SCI_SETSEL,
            selection.anchor.min(len) as usize,
            selection.caret.min(len) as isize,
        );
    }

    /// 矩形選択にする（`anchor` と `caret` が対角の 2 点）。
    pub fn set_rectangular_selection(&self, anchor: u64, caret: u64) {
        let len = self.length();
        // この 2 つのメッセージが選択を矩形にする（以後のカーソル移動では伸びない）。
        self.send(
            SCI_SETRECTANGULARSELECTIONANCHOR,
            anchor.min(len) as usize,
            0,
        );
        self.send(SCI_SETRECTANGULARSELECTIONCARET, caret.min(len) as usize, 0);
    }

    pub fn is_rectangular_selection(&self) -> bool {
        self.send(SCI_GETSELECTIONMODE, 0, 0) as usize == SC_SEL_RECTANGLE
    }

    /// 「矩形選択」の切り替えが入っているか（以後のマウス選択・矢印キーによる選択が
    /// 矩形になる状態）。
    pub fn rectangular_select_active(&self) -> bool {
        self.is_rectangular_selection() && self.send(SCI_GETMOVEEXTENDSSELECTION, 0, 0) != 0
    }

    /// 次のマウス選択・矢印キーによる選択を矩形にする状態を切り替える（V1）。
    /// `SCI_SETSELECTIONMODE` は同じ値を 2 回送ると解除になる（Scintilla の仕様）。
    pub fn toggle_rectangular_select(&self) {
        if self.rectangular_select_active() {
            // 矩形から通常へは 2 回（1 回目で種類だけ戻り、2 回目で「伸ばす」状態が切れる）。
            for _ in 0..2 {
                if self.send(SCI_GETMOVEEXTENDSSELECTION, 0, 0) == 0
                    && !self.is_rectangular_selection()
                {
                    break;
                }
                self.send(SCI_SETSELECTIONMODE, SC_SEL_STREAM, 0);
            }
        } else {
            self.send(SCI_SETSELECTIONMODE, SC_SEL_RECTANGLE, 0);
        }
    }

    pub fn select_all(&self) {
        self.send(SCI_SELECTALL, 0, 0);
    }

    pub fn cut(&self) {
        self.send(SCI_CUT, 0, 0);
    }

    pub fn copy(&self) {
        self.send(SCI_COPY, 0, 0);
    }

    pub fn paste(&self) {
        self.send(SCI_PASTE, 0, 0);
    }

    pub fn can_paste(&self) -> bool {
        self.send(SCI_CANPASTE, 0, 0) != 0
    }

    pub fn scroll_caret(&self) {
        self.send(SCI_SCROLLCARET, 0, 0);
    }

    // ---------------------------------------------------------------- Undo・保存位置

    pub fn begin_undo_action(&self) {
        self.send(SCI_BEGINUNDOACTION, 0, 0);
    }

    pub fn end_undo_action(&self) {
        self.send(SCI_ENDUNDOACTION, 0, 0);
    }

    pub fn undo(&self) {
        self.send(SCI_UNDO, 0, 0);
    }

    pub fn redo(&self) {
        self.send(SCI_REDO, 0, 0);
    }

    pub fn can_undo(&self) -> bool {
        self.send(SCI_CANUNDO, 0, 0) != 0
    }

    pub fn can_redo(&self) -> bool {
        self.send(SCI_CANREDO, 0, 0) != 0
    }

    /// Undo の履歴を消す（ファイルを読み込んだ直後など）。
    pub fn empty_undo_buffer(&self) {
        self.send(SCI_EMPTYUNDOBUFFER, 0, 0);
    }

    /// Undo の記録を有効・無効にする（ドキュメントごとの設定）。
    pub fn set_undo_collection(&self, collect: bool) {
        self.send(SCI_SETUNDOCOLLECTION, collect as usize, 0);
    }

    /// [`DocLoader`] で作ったドキュメントを表示した直後に呼ぶ: 読み込み口は Undo の記録を
    /// 切ったままにするので有効に戻し、履歴を空にして、現在の内容を保存位置にする。
    pub fn finish_loaded_document(&self) {
        self.set_undo_collection(true);
        self.empty_undo_buffer();
        self.set_save_point();
    }

    /// 現在の内容を保存位置にする（`SCN_SAVEPOINTREACHED` が届く）。
    pub fn set_save_point(&self) {
        self.send(SCI_SETSAVEPOINT, 0, 0);
    }

    /// Scintilla から見て保存位置から変わっているか。
    pub fn is_modified(&self) -> bool {
        self.send(SCI_GETMODIFY, 0, 0) != 0
    }

    // ---------------------------------------------------------------- 文書ごとの設定

    pub fn set_read_only(&self, read_only: bool) {
        self.send(SCI_SETREADONLY, read_only as usize, 0);
    }

    pub fn is_read_only(&self) -> bool {
        self.send(SCI_GETREADONLY, 0, 0) != 0
    }

    /// 新しく入力する改行の形式（既存の改行は変えない）。
    pub fn set_eol(&self, eol: Eol) {
        let mode = match eol {
            Eol::CrLf => SC_EOL_CRLF,
            Eol::Cr => SC_EOL_CR,
            Eol::Lf => SC_EOL_LF,
        };
        self.send(SCI_SETEOLMODE, mode, 0);
    }

    /// 本文のすべての改行を指定の形式に変換する（編集として Undo できる）。
    pub fn convert_eols(&self, eol: Eol) {
        let mode = match eol {
            Eol::CrLf => SC_EOL_CRLF,
            Eol::Cr => SC_EOL_CR,
            Eol::Lf => SC_EOL_LF,
        };
        self.send(SCI_CONVERTEOLS, mode, 0);
    }

    /// 表示中のドキュメントに字句解析器を設定する（字句解析器はドキュメント側に残る）。
    /// 着色の配色は [`apply_theme`](Self::apply_theme) で別に設定する。
    pub fn set_language(&self, language: Language) {
        let lexer = match language.lexer_name() {
            Some(name) => {
                // lexer_name は英小文字だけなので NUL を含まない。
                let name = CString::new(name).unwrap_or_default();
                // SAFETY: NUL 終端の名前を渡す。戻り値は ILexer5*（または null）で、
                // 所有権は SCI_SETILEXER で Scintilla に移る。
                unsafe { crate::sci_sys::linote_create_lexer(name.as_ptr()) }
            }
            None => std::ptr::null_mut(),
        };
        self.send(SCI_SETILEXER, 0, lexer as isize);
        if lexer.is_null() {
            return;
        }
        for (key, value) in language.properties() {
            let (k, v) = (cstr(key), cstr(value));
            self.send(SCI_SETPROPERTY, k.as_ptr() as usize, v.as_ptr() as isize);
        }
        for (set, words) in language.keywords() {
            let words = cstr(words);
            self.send(SCI_SETKEYWORDS, *set, words.as_ptr() as isize);
        }
        // 着色は表示する範囲から空き時間に行われる（SCI_SETIDLESTYLING）。ここで全体を
        // 着色しない。
    }

    // ---------------------------------------------------------------- 表示

    /// 配色と書体を設定する。スタイル番号の意味は字句解析器ごとに違うので、
    /// 文書を切り替えたら、その文書の言語でもう一度呼ぶ。
    pub fn apply_theme(&self, theme: &Theme, language: Language) {
        let e = &theme.editor;
        let (font_name, font_size) = {
            let v = self.view.borrow();
            (v.font.clone(), v.font_size)
        };
        let font = cstr(if font_name.is_empty() {
            editor_font()
        } else {
            &font_name
        });
        self.send(SCI_STYLESETFONT, STYLE_DEFAULT, font.as_ptr() as isize);
        self.send(
            SCI_STYLESETSIZEFRACTIONAL,
            STYLE_DEFAULT,
            (font_size.max(1) as isize) * 100,
        );
        self.send(SCI_STYLESETFORE, STYLE_DEFAULT, e.fg.colorref() as isize);
        self.send(SCI_STYLESETBACK, STYLE_DEFAULT, e.bg.colorref() as isize);
        self.send(SCI_STYLESETBOLD, STYLE_DEFAULT, 0);
        self.send(SCI_STYLESETITALIC, STYLE_DEFAULT, 0);
        self.send(SCI_STYLECLEARALL, 0, 0);

        self.send(
            SCI_STYLESETFORE,
            STYLE_LINENUMBER,
            e.line_number_fg.colorref() as isize,
        );
        self.send(
            SCI_STYLESETBACK,
            STYLE_LINENUMBER,
            e.line_number_bg.colorref() as isize,
        );
        self.send(
            SCI_STYLESETFORE,
            STYLE_CONTROLCHAR,
            e.whitespace.colorref() as isize,
        );
        self.send(
            SCI_STYLESETFORE,
            STYLE_INDENTGUIDE,
            e.whitespace.colorref() as isize,
        );
        let brace = theme.token(Token::Keyword);
        self.send(
            SCI_STYLESETFORE,
            STYLE_BRACELIGHT,
            brace.fore.colorref() as isize,
        );
        self.send(SCI_STYLESETBOLD, STYLE_BRACELIGHT, 1);
        let bad = theme.token(Token::Error);
        self.send(
            SCI_STYLESETFORE,
            STYLE_BRACEBAD,
            bad.fore.colorref() as isize,
        );

        for &(style, token) in language.styles() {
            let s = theme.token(token);
            let style = style as usize;
            self.send(SCI_STYLESETFORE, style, s.fore.colorref() as isize);
            self.send(SCI_STYLESETBOLD, style, s.bold as isize);
            self.send(SCI_STYLESETITALIC, style, s.italic as isize);
        }

        let element = |el: usize, c: crate::ui::theme::Rgb| {
            self.send(SCI_SETELEMENTCOLOUR, el, c.colour_alpha() as isize);
        };
        element(SC_ELEMENT_SELECTION_BACK, e.selection_bg);
        element(SC_ELEMENT_SELECTION_ADDITIONAL_BACK, e.selection_bg);
        element(SC_ELEMENT_SELECTION_SECONDARY_BACK, e.inactive_selection_bg);
        element(SC_ELEMENT_SELECTION_INACTIVE_BACK, e.inactive_selection_bg);
        element(SC_ELEMENT_CARET, e.caret);
        element(SC_ELEMENT_CARET_ADDITIONAL, e.caret);
        // Scintilla 5.4 は、現在行の背景色が設定されていれば強調する。切るときは設定を外す。
        if self.view.borrow().highlight_current_line {
            element(SC_ELEMENT_CARET_LINE_BACK, e.caret_line_bg);
        } else {
            self.send(SCI_SETCARETLINEVISIBLE, 0, 0);
        }
        element(SC_ELEMENT_WHITE_SPACE, e.whitespace);
        self.send(SCI_SETEDGECOLOUR, e.whitespace.colorref() as usize, 0);
        self.send(SCI_SETSELECTIONLAYER, SC_LAYER_BASE, 0);
        self.send(SCI_SETFOLDMARGINCOLOUR, 1, e.bg.colorref() as isize);
        self.send(SCI_SETFOLDMARGINHICOLOUR, 1, e.bg.colorref() as isize);
        self.apply_bookmark_theme(theme);
        self.update_line_number_margin();
    }

    /// 行番号の余白の幅を、行数の桁に合わせる（最低 3 桁。DPI は Scintilla が考慮する）。
    pub fn update_line_number_margin(&self) {
        if self.view.borrow().line_numbers {
            let digits = line_number_digits(self.line_count());
            let sample = cstr(&"9".repeat(digits + 1));
            let width = self.send(SCI_TEXTWIDTH, STYLE_LINENUMBER, sample.as_ptr() as isize);
            self.send(SCI_SETMARGINWIDTHN, 0, width);
        } else {
            self.send(SCI_SETMARGINWIDTHN, 0, 0);
        }
        // 余白の幅が変わると、桁指定の折り返しの右余白も合わせ直す。
        self.update_wrap_margin();
    }

    /// 編集欄にフォーカスを移す。
    pub fn focus(&self) {
        self.send(SCI_GRABFOCUS, 0, 0);
    }
}

/// 行番号に必要な桁数（最低 3）。
pub fn line_number_digits(lines: u64) -> usize {
    let mut digits = 1;
    let mut n = lines;
    while n >= 10 {
        n /= 10;
        digits += 1;
    }
    digits.max(3)
}

fn cstr(s: &str) -> CString {
    // 定数とテーマの名前だけを渡すので NUL を含まない。含んでいたら空にする。
    CString::new(s).unwrap_or_default()
}

/// 既定の本文の書体の名前（候補のうち最初にインストールされているもの）。
pub fn default_font_name() -> &'static str {
    editor_font()
}

/// 本文の書体（候補のうち最初にインストールされているもの）。
fn editor_font() -> &'static str {
    static FONT: OnceLock<&'static str> = OnceLock::new();
    FONT.get_or_init(|| {
        FONT_CANDIDATES
            .into_iter()
            .find(|name| font_exists(name))
            .unwrap_or(FONT_CANDIDATES[0])
    })
}

fn font_exists(name: &str) -> bool {
    unsafe extern "system" fn found(
        _: *const LOGFONTW,
        _: *const TEXTMETRICW,
        _: u32,
        lparam: LPARAM,
    ) -> i32 {
        // SAFETY: lparam は呼び出し側の bool を指す。
        unsafe { *(lparam.0 as *mut bool) = true };
        0
    }
    let mut lf = LOGFONTW {
        lfCharSet: DEFAULT_CHARSET,
        ..Default::default()
    };
    let wide: Vec<u16> = name.encode_utf16().collect();
    if wide.len() >= lf.lfFaceName.len() {
        return false;
    }
    lf.lfFaceName[..wide.len()].copy_from_slice(&wide);
    let mut exists = false;
    // SAFETY: 画面の DC を借りて返す。コールバックは exists だけを書き換える。
    unsafe {
        let dc = GetDC(None);
        EnumFontFamiliesExW(
            dc,
            &lf,
            Some(found),
            LPARAM(&mut exists as *mut bool as isize),
            0,
        );
        ReleaseDC(None, dc);
    }
    exists
}

impl TextSource for Editor {
    fn len(&self) -> u64 {
        self.length()
    }

    fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<usize> {
        let bytes = self.text_range(offset, offset.saturating_add(buf.len() as u64));
        buf[..bytes.len()].copy_from_slice(&bytes);
        Ok(bytes.len())
    }

    fn line_count(&self) -> LineCount {
        LineCount::Exact(Editor::line_count(self))
    }

    fn line_start(&self, line: u64) -> Result<Option<u64>> {
        Ok(Editor::line_start(self, line))
    }
}

impl EditTarget for Editor {
    fn selection(&self) -> Selection {
        Editor::selection(self)
    }

    fn set_selection(&mut self, selection: Selection) {
        Editor::set_selection(self, selection);
        self.scroll_caret();
    }

    fn replace(&mut self, range: TextRange, text: &str) -> Result<()> {
        if self.is_read_only() {
            return Err(Error::unsupported("edit"));
        }
        range.check_within(self.length())?;
        if !self.is_char_boundary(range.start) || !self.is_char_boundary(range.end) {
            return Err(Error::invalid_argument("range", "文字の途中を指しています"));
        }
        self.replace_raw(range.start, range.end, text.as_bytes());
        Editor::set_selection(self, Selection::caret_at(range.start + text.len() as u64));
        Ok(())
    }

    fn begin_undo_group(&mut self) {
        self.begin_undo_action();
    }

    fn end_undo_group(&mut self) {
        self.end_undo_action();
    }
}

/// 本文を Scintilla のドキュメントにまとめて流し込む読み込み口（`ILoader`）。
///
/// 表示中でないドキュメントを作るので、流し込みの間は変更通知も Undo の記録も起きない。
/// Scintilla の取り決めにより、作った後は**別のスレッドで** [`add`](Self::add) してよい
/// （ファイルの読み込みをワーカーで行う I1 向け）。[`finish`](Self::finish) で
/// [`SciDoc`]（参照 1 つ）になる。途中で捨てると `Release` される。
pub struct DocLoader {
    loader: *mut ILoader,
}

// SAFETY: ILoader は表示に結びつかない文書を作るためのもので、Scintilla は別スレッドでの
// AddData を想定している（Scintilla のドキュメント「Loading in the background」）。
// 同時に 2 つのスレッドから使わないことは &mut self で保証する。
unsafe impl Send for DocLoader {}

impl DocLoader {
    /// 本文の続きを足す（UTF-8 のバイト列）。メモリ不足などで失敗すると Err。
    pub fn add(&mut self, bytes: &[u8]) -> Result<()> {
        // SAFETY: loader は有効な ILoader*。データは bytes の範囲。
        let status = unsafe {
            ((*(*self.loader).vtbl).add_data)(
                self.loader,
                bytes.as_ptr() as *const c_char,
                bytes.len() as isize,
            )
        };
        if status == SC_STATUS_OK {
            Ok(())
        } else {
            Err(Error::Internal(format!(
                "本文を編集欄に読み込めませんでした（{status}）"
            )))
        }
    }

    /// ドキュメントにする（参照 1 つを呼び出し側が持つ）。
    pub fn finish(self) -> Result<SciDoc> {
        let loader = self.loader;
        std::mem::forget(self);
        // SAFETY: loader は有効で、ConvertToDocument の後は使わない（解放も不要）。
        let doc = unsafe { ((*(*loader).vtbl).convert_to_document)(loader) };
        if doc.is_null() {
            Err(Error::Internal("Scintilla の文書を作れませんでした".into()))
        } else {
            Ok(SciDoc(doc as isize))
        }
    }
}

impl Drop for DocLoader {
    fn drop(&mut self) {
        // SAFETY: finish していない有効な loader を 1 回だけ解放する。
        unsafe {
            ((*(*self.loader).vtbl).release)(self.loader);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_number_digits_grow_with_the_line_count() {
        assert_eq!(line_number_digits(1), 3);
        assert_eq!(line_number_digits(999), 3);
        assert_eq!(line_number_digits(1000), 4);
        assert_eq!(line_number_digits(123_456), 6);
    }
}
