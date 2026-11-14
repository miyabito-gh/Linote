//! 文字コードと改行（タスク A の担当。`src/text/` 配下）。
//!
//! 責務: 文字コードの判定、ファイルのバイト列と編集用 UTF-8 の相互変換、BOM の扱い、
//! 改行形式の判定、変換できない文字の検出。GUI に依存せず、`windows` crate を使わない。
//!
//! 取り決め:
//! - **編集中の本文は常に UTF-8**（Scintilla のコードページは UTF-8）。位置はすべて
//!   UTF-8 のバイト位置（[`crate::document::TextRange`]）。
//! - ファイルの形式は [`TextFormat`] で表し、文書の状態
//!   （[`crate::document::DocumentState::format`]）に持たせる。保存では開いたときの
//!   形式をそのまま使う（利用者が変更した場合を除く）。
//! - 改行は変換しない。本文中の改行はバイト列のまま保持し、[`TextFormat::eol`] は
//!   「新しく入力する改行」の形式（判定では最も多い改行）を表す。
//! - 長い処理は [`crate::core::TaskContext`] を受け取り、中止に応じる。
//! - エラーは [`crate::core::Error`]（文字コードの問題は `Error::Encoding`）。
//!   変換できない文字の位置など詳しい結果は、このモジュールで専用の型を定義して返す。
//!
//! ここに定義済みの型（[`Encoding`]、[`Eol`]、[`TextFormat`]）は他のモジュールと
//! 共有する取り決めなので、既存の公開項目を変える場合は報告すること。追加は自由。

//!
//! ## 提供する処理（タスク A）
//!
//! - [`detect`] — 文字コードの判定（根拠 [`DetectReason`] と信頼度 [`Confidence`] つき）。
//! - [`decode`] — バイト列 → UTF-8。不正なバイトは U+FFFD に置き換えて
//!   [`InvalidBytes`] で報告する。往復でバイト列が一致するかも返す。
//! - [`EolStats`] — 改行の数。最も多い形式を [`TextFormat::eol`] にする。
//! - [`encode_to`]／[`encode`] — UTF-8 → 元の文字コード・BOM。改行は変換しない。
//! - [`find_unmappable`]／[`can_encode`] — 変換できない文字の位置（行・列）と一覧。
//!
//! CP932 の機種依存文字・外字・重複文字の扱いは [`encode`](mod@encode) の説明にまとめた。

mod decode;
mod detect;
pub mod encode;
mod eol;
pub mod tidy;

pub use decode::{decode, Decoded, InvalidBytes};
pub use detect::{detect, has_bom, Confidence, DetectReason, Detection};
pub use encode::{
    can_encode, encode, encode_to, find_unmappable, OnUnmappable, Unmappable, REPLACEMENT_BYTE,
};
pub use eol::{normalize_eol, EolStats};

/// 通常編集で扱う文字コード。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Encoding {
    Utf8,
    Utf16Le,
    Utf16Be,
    /// CP932（Windows-31J。`encoding_rs::SHIFT_JIS` 相当）。
    Cp932,
}

impl Encoding {
    pub const ALL: [Encoding; 4] = [
        Encoding::Utf8,
        Encoding::Utf16Le,
        Encoding::Utf16Be,
        Encoding::Cp932,
    ];

    /// 設定・記録・中継で使う安定した名前。
    pub const fn name(self) -> &'static str {
        match self {
            Encoding::Utf8 => "utf-8",
            Encoding::Utf16Le => "utf-16le",
            Encoding::Utf16Be => "utf-16be",
            Encoding::Cp932 => "cp932",
        }
    }

    /// [`name`](Self::name) の逆。大文字小文字は区別しない。
    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|e| e.name().eq_ignore_ascii_case(name))
    }

    /// ステータスバーに出す表示名（BOM の有無は [`TextFormat::label`] が付ける）。
    pub const fn label(self) -> &'static str {
        match self {
            Encoding::Utf8 => "UTF-8",
            Encoding::Utf16Le => "UTF-16 LE",
            Encoding::Utf16Be => "UTF-16 BE",
            Encoding::Cp932 => "Shift_JIS (CP932)",
        }
    }

    /// BOM のバイト列。BOM という概念がない文字コードでは空。
    pub const fn bom(self) -> &'static [u8] {
        match self {
            Encoding::Utf8 => &[0xEF, 0xBB, 0xBF],
            Encoding::Utf16Le => &[0xFF, 0xFE],
            Encoding::Utf16Be => &[0xFE, 0xFF],
            Encoding::Cp932 => &[],
        }
    }
}

/// 改行形式。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Eol {
    Lf,
    CrLf,
    Cr,
}

impl Eol {
    pub const fn as_str(self) -> &'static str {
        match self {
            Eol::Lf => "\n",
            Eol::CrLf => "\r\n",
            Eol::Cr => "\r",
        }
    }

    /// ステータスバーに出す表示名。設定・中継の名前としても使う。
    pub const fn label(self) -> &'static str {
        match self {
            Eol::Lf => "LF",
            Eol::CrLf => "CRLF",
            Eol::Cr => "CR",
        }
    }

    /// [`label`](Self::label) の逆。大文字小文字は区別しない。
    pub fn from_label(label: &str) -> Option<Self> {
        [Eol::Lf, Eol::CrLf, Eol::Cr]
            .into_iter()
            .find(|e| e.label().eq_ignore_ascii_case(label))
    }
}

/// ファイルの形式（文字コード、BOM、改行）。保存時に保持する対象。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TextFormat {
    pub encoding: Encoding,
    /// ファイルの先頭に BOM を書くか。CP932 では常に `false`。
    pub bom: bool,
    /// 新しく入力する改行の形式。既存の改行は書き換えない。
    pub eol: Eol,
}

impl TextFormat {
    /// 新規文書・メモ・クリップボード由来の文書の形式（UTF-8、BOM なし、CRLF）。
    pub const DEFAULT: TextFormat = TextFormat {
        encoding: Encoding::Utf8,
        bom: false,
        eol: Eol::CrLf,
    };

    /// ステータスバーに出す文字コードの表示（例: `UTF-8 (BOM)`）。
    pub fn label(&self) -> String {
        if self.bom {
            format!("{} (BOM)", self.encoding.label())
        } else {
            self.encoding.label().to_owned()
        }
    }
}

impl Default for TextFormat {
    fn default() -> Self {
        Self::DEFAULT
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_round_trip() {
        for e in Encoding::ALL {
            assert_eq!(Encoding::from_name(e.name()), Some(e));
            assert_eq!(Encoding::from_name(&e.name().to_uppercase()), Some(e));
        }
        assert_eq!(Encoding::from_name("latin1"), None);
        for e in [Eol::Lf, Eol::CrLf, Eol::Cr] {
            assert_eq!(Eol::from_label(e.label()), Some(e));
        }
    }

    #[test]
    fn boms_and_labels() {
        assert_eq!(Encoding::Utf8.bom(), b"\xEF\xBB\xBF");
        assert!(Encoding::Cp932.bom().is_empty());
        assert_eq!(TextFormat::default().label(), "UTF-8");
        let f = TextFormat {
            encoding: Encoding::Utf16Le,
            bom: true,
            eol: Eol::Lf,
        };
        assert_eq!(f.label(), "UTF-16 LE (BOM)");
        assert_eq!(Eol::CrLf.as_str(), "\r\n");
    }
}
