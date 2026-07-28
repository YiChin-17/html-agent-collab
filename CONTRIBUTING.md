# 貢獻指南

感謝你協助改善 html-agent-collab。專案只支援 macOS 15 以上版本，並以 Rust 1.97.0、Tauri 2 與系統 WKWebView 為基準環境。

## 開發環境

1. 安裝 Xcode Command Line Tools、rustup、`jq` 與 `curl`。
2. Fork repository，從最新 `main` 建立功能分支。
3. 在 repository root 執行 `cargo build` 確認環境可用。

若你的 clone 建立於 2026 年 7 月公開前的 history migration 之前，請重新 clone；不要將舊歷史 merge 回來。

## 修改與驗證

每個變更應保持單一 scope，並為行為變更先新增會失敗的測試。提交 pull request 前執行：

```bash
cargo fmt -- --check
cargo test
cargo clippy --all-targets --all-features -- -D warnings
cargo package
scripts/session-ux-acceptance.sh
```

若修改 dependencies、CI 或 policy，另執行 `cargo deny check` 與 `gitleaks detect --source .`。四小時 soak 只在資源生命週期或 queue/buffer 上限改變時執行。

## Commit 與 pull request

- Commit message 遵循 [Conventional Commits](https://www.conventionalcommits.org/)；例如 `fix: reject stale session identity`。
- 不要把重構與功能修改混在同一個 commit。
- Pull request 說明需包含問題、解法、驗證輸出及使用者可觀察到的影響。
- 不要提交 `.collab/`、control token、screenshot evidence 或其他 secrets。
- 所有貢獻均依本 repository 的 MIT OR Apache-2.0 條款提供，除非另有明確書面聲明。

一般 bug 與 feature request 請使用對應 issue form。安全漏洞不得建立公開 issue，請依 `SECURITY.md` 使用 GitHub private vulnerability reporting。
