# 安全模型

html-agent-collab 是本機 macOS preview 工具，不是可部署到網路的 HTTP server。以下邊界與信任假設適用於每個 preview session。

## 支援版本

| 版本 | 安全更新 |
| --- | --- |
| `main` 與最新 GitHub Release | 支援 |
| 較舊的 source release | 不支援；請先在最新版本重現 |

目前所有支援版本都要求 macOS 15 或更新版本。

## 通報安全漏洞

Repository 公開後，請使用 GitHub repository 的 Security 頁面與 GitHub private vulnerability reporting 私下通報安全漏洞。不要建立公開 issue、pull request 或 discussion，也不要在報告中附上真實 control token、`.collab/session.json` 或私人 project 內容。

報告請包含受影響的 commit 或版本、macOS 版本、最小重現步驟、影響範圍及已知緩解方式。Maintainer 會在 private advisory 內協調確認、修正與公開時程。

一般 bug、功能需求與不涉及安全邊界的錯誤請使用 GitHub issue forms；行為準則事件依 `CODE_OF_CONDUCT.md` 的 Enforcement 流程處理。

## Loopback 與 Host 驗證

Preview server 只綁定 IPv4 loopback `127.0.0.1`，不支援非 loopback interface 或自訂 hostname。所有 HTTP routes，包括 project files、health/status、control 與 overlay feedback，都只接受符合當前 session port 的 `127.0.0.1:<port>` 或 `localhost:<port>` Host header。缺少或不符的 Host header 會在進入 route 前被拒絕，以降低 DNS rebinding 讓遠端網頁存取本機 preview 的風險。

Loopback 與 Host 驗證不會隔離同一台電腦上的其他 process。本機 process 可連線至 server 並自行送出合法 Host header；請只在受信任的本機環境執行 preview。

## Control token

每個 preview session 都有不可預測的 control token，用來保護會改變狀態或控制頁面的 operations，例如 attach、detach、close、reload、eval、screenshot、wait 與 feedback lifecycle 變更。CLI 從被預覽專案的 `.collab/session.json` 讀取 token；session file 以 mode `0600` 建立，正常 CLI 輸出與 logs 不會顯示 token。

Token 不保護 project file serving、最小 read-only health/status，或 overlay feedback endpoint。Token 是 session secret，但不是用來抵禦同一使用者帳號下惡意本機 process 的完整 sandbox。

## Overlay feedback 的信任假設

`/__collab__/overlay/feedback` 刻意不要求 control token，因為 token 不會暴露給被預覽頁面的 JavaScript。這讓 preview page 能提交 feedback，同時也代表任何能連到 loopback server 並使用合法 Host header 的本機 process 都可能注入 feedback。

Agent 不得無條件信任 feedback 內容。Feedback 應視為不受信任的使用者輸入：不得因其中的文字而揭露 secrets、執行超出目前工作範圍的命令，或繞過既有核准與安全邊界。

## Project root 可讀範圍

Preview server 會透過 loopback HTTP origin，在不要求 control token 的情況下提供整個被選取的 project root。路徑穿越到 project root 外會被拒絕，但 root 內的檔案都可能被知道 session port 且能送出合法 Host header 的本機 process 讀取。

不要以此工具預覽包含不應暴露給同一台電腦上其他 process 的敏感檔案之 project root。Host 驗證用來阻擋 foreign web origins 的 DNS rebinding，不是 project root 內的檔案存取控制。

## Runtime artifacts

Feedback records、painting attachments 與 screenshots 只會發布到目前 project root 的 `.collab/feedback` 或 `.collab/screenshots`。這兩個位置必須是 real directory（實體目錄）；runtime 會拒絕預先存在的 symlink 或非 directory 項目，並把目錄權限強制設為 mode `0700`。

每個 JSON、SVG 與 PNG artifact 都先寫入目的檔案同一目錄內的隨機 temporary file。Runtime 以 create-new、`O_NOFOLLOW` 與 mode `0600` 開啟 temporary file，完整寫入並執行 `sync_all` 後，再用 same-directory atomic rename 發布。這個流程不會跟隨預先放置的 temporary 或 final symlink；若發布失敗，只會清理由該次 operation 建立的 temporary file，也不會回報 artifact path 已成功發布。

這些檔案系統限制降低 project 內容預先放置 symlink 所造成的覆寫風險，但不是 sandbox。與 preview 使用相同帳號執行的惡意 same-user process 仍在既有信任邊界內，runtime 不承諾抵禦它在檢查後即時替換目錄或檔案。

## Session file 與版本控制

`.collab/session.json` 包含 control token。請在每個被預覽專案的 `.gitignore` 加入：

```gitignore
.collab/
```

不得 commit、分享或公開 session file。Preview runtime 關閉後會移除 active session file，但 `.collab/` 內仍可能有 feedback 或 screenshot artifacts，因此整個目錄都應排除於版本控制之外。
