//! Linote の中核ライブラリ。
//!
//! 取り決めの全体は `docs/architecture.md`。依存の向きは一方向:
//!
//! ```text
//! core ← text, file ← document ← command ← appdata, mcp
//!      ← memo, recovery, search, bigfile, markdown      （ここまで GUI 非依存）
//!      ← platform ← ui                                   （Windows 専用）
//! ```
//!
//! GUI 非依存のモジュールは `windows` crate を使わず、Linux でもテストできる
//! （`cargo test --lib --target x86_64-unknown-linux-gnu`）。

// 共通の取り決め（タスク C0）。
pub mod appdata;
pub mod command;
pub mod core;
pub mod document;
pub mod mcp;

// GUI 非依存の中核（各タスクが実装する）。
pub mod bigfile;
pub mod file;
pub mod markdown;
pub mod memo;
pub mod recovery;
pub mod search;
pub mod text;

// Windows 専用。
#[cfg(windows)]
pub mod platform;
#[cfg(windows)]
pub mod ui;

/// Raw FFI to the statically linked Scintilla/Lexilla (see build.rs).
#[cfg(windows)]
pub mod sci_sys {
    extern "C" {
        pub fn Scintilla_RegisterClasses(h_instance: *mut core::ffi::c_void) -> i32;
        pub fn Scintilla_ReleaseResources() -> i32;
        /// Returns an `ILexer5*` for a bundled lexer name, or null.
        pub fn linote_create_lexer(name: *const core::ffi::c_char) -> *mut core::ffi::c_void;
    }
}
