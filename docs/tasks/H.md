# H: Markdown プレビュー表示（WebView2 の遅延初期化と自前ローダー）

担当: Opus（自前ローダー部分は Fable のレビュー対象）。共通指示は `_common.md`。

## 担当パス
`src/platform/webview2_loader.rs`、`src/ui/preview.rs`（右ペインと別ウィンドウ）。

## 要件
1. **自前ローダー**（`architecture.md` の WebView2 の節）: `webview2-com` の COM インターフェース定義だけを使い、`WebView2Loader.dll` に依存しない。インストール済みランタイムをレジストリ（`SOFTWARE\Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}` の `pv`、HKLM／HKCU、`WOW6432Node`）から探し、ランタイムフォルダー（`location` 値、または既定のインストール先 `%ProgramFiles(x86)%\Microsoft\EdgeWebView\Application\<pv>`）配下の `EBWebView\x64\EmbeddedBrowserWebView.dll` を `LoadLibraryW` し、`CreateWebViewEnvironmentWithOptionsInternal(checkRunningInstance: BOOL, runtimeType: i32 /*0=installed*/, userDataFolder: PCWSTR, options: ICoreWebView2EnvironmentOptions*, handler)` を呼ぶ。ユーザーデータフォルダーは `%LOCALAPPDATA%\Linote\WebView2`。
2. **遅延初期化**: プレビューを初めて開く操作まで、レジストリ確認・DLL 読み込み・環境生成・関連プロセス起動を一切しない。ランタイムがなければその時点で案内し、通常編集は続けられる。
3. 右ペインと別ウィンドウの両方。どちらも `markdown` モジュールの変換結果を `NavigateToString` で表示。スクリプト無効（`ICoreWebView2Settings::SetIsScriptEnabled(false)`）、外部ナビゲーションの拒否（`NavigationStarting` でキャンセル）、右クリックメニュー・開発者ツールの無効化、外部リソース要求の拒否（`WebResourceRequested` フィルター）。
4. テーマを反映。
5. EXE の DLL 依存が増えないこと（`scripts/check-imports.sh`）。`webview2-com` を `Cargo.toml` に追加する必要がある（`windows` 0.62 と整合する版。0.39 系を想定）。

## テスト
Wine には WebView2 がないため、ランタイムがない場合の案内経路と、レジストリ探索・パス組み立ての単体テストを自動化する。表示そのものは Windows 実機確認項目として報告に列挙する。
