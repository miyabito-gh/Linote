//! クリップボードのテキストの読み書き（`CF_UNICODETEXT`）。
//! 日本語・タブ・改行をそのまま保つ。テキストがなければ `None` を返し、呼び出し側が短い状態表示を出す。
//! 取り込んだ文書は [`crate::document::DocumentState::clipboard`]。
//!
//! 担当: タスク F。

use std::time::Duration;

use windows::Win32::Foundation::{HANDLE, HGLOBAL, HWND};
use windows::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, GetClipboardData, IsClipboardFormatAvailable, OpenClipboard,
    SetClipboardData,
};
use windows::Win32::System::Memory::{
    GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock, GMEM_MOVEABLE,
};

use crate::core::{Error, Result};

/// `CF_UNICODETEXT`。
const CF_UNICODETEXT: u32 = 13;

/// 他のプロセスが開いている間は少し待って数回やり直す。
fn open(owner: Option<HWND>) -> Result<()> {
    for _ in 0..10 {
        // SAFETY: owner は有効な HWND か None。
        if unsafe { OpenClipboard(owner) }.is_ok() {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    Err(Error::Busy {
        what: "クリップボード".to_owned(),
    })
}

struct Guard;
impl Drop for Guard {
    fn drop(&mut self) {
        // SAFETY: Guard は open 成功後にだけ作る。
        unsafe {
            let _ = CloseClipboard();
        }
    }
}

/// テキストがクリップボードにあるか（開かずに確認する）。
pub fn has_text() -> bool {
    // SAFETY: 引数は定数。
    unsafe { IsClipboardFormatAvailable(CF_UNICODETEXT).is_ok() }
}

/// `CF_UNICODETEXT` を取得する。テキストがなければ `Ok(None)`、空文字列も `None`。
/// 改行・タブ・日本語は変換せずそのまま返す（末尾の NUL だけ除く）。
pub fn read_text(owner: Option<HWND>) -> Result<Option<String>> {
    if !has_text() {
        return Ok(None);
    }
    open(owner)?;
    let _guard = Guard;
    // SAFETY: クリップボードを開いている間だけハンドルを使い、ロックして読む。
    unsafe {
        let Ok(h) = GetClipboardData(CF_UNICODETEXT) else {
            return Ok(None);
        };
        let hg = HGLOBAL(h.0);
        let size = GlobalSize(hg) / 2;
        let p = GlobalLock(hg) as *const u16;
        if p.is_null() {
            return Ok(None);
        }
        let all = std::slice::from_raw_parts(p, size);
        let len = all.iter().position(|&c| c == 0).unwrap_or(size);
        let text = String::from_utf16_lossy(&all[..len]);
        let _ = GlobalUnlock(hg);
        Ok((!text.is_empty()).then_some(text))
    }
}

/// `CF_UNICODETEXT` として書き込む。
pub fn write_text(owner: Option<HWND>, text: &str) -> Result<()> {
    let mut wide: Vec<u16> = text.encode_utf16().collect();
    wide.push(0);
    open(owner)?;
    let _guard = Guard;
    let io = |e: windows::core::Error| Error::Io {
        op: "クリップボードへの書き込み",
        path: None,
        source: std::io::Error::other(e.to_string()),
    };
    // SAFETY: 確保したメモリへ自分で書いてから所有権をクリップボードへ渡す。
    unsafe {
        EmptyClipboard().map_err(io)?;
        let hg = GlobalAlloc(GMEM_MOVEABLE, wide.len() * 2).map_err(io)?;
        let p = GlobalLock(hg) as *mut u16;
        if p.is_null() {
            return Err(Error::Busy {
                what: "クリップボード".to_owned(),
            });
        }
        std::ptr::copy_nonoverlapping(wide.as_ptr(), p, wide.len());
        let _ = GlobalUnlock(hg);
        SetClipboardData(CF_UNICODETEXT, Some(HANDLE(hg.0))).map_err(io)?;
    }
    Ok(())
}
