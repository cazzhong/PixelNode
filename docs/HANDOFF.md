# PixelNode 交接文件

> 最後更新：2026-10-17。給下一個接手的對話或開發者。專案規格與慣例見根目錄 `CLAUDE.md`，使用說明見 `README.md`。

## 1. 現況

- CLAUDE.md 的 M1–M4 功能都已實作，GitHub：https://github.com/cazzhong/PixelNode（公開，分支 `main`）。
- 最新 CI 全綠（Windows runner，沒有 GPU，用軟體 adapter 跑測試與 benchmark）。
- 共 37 個節點，詳細清單見 README。
- 效能（RTX 2070，100 節點、256×256）：融合後每格 1.32 ms，逐節點執行 3.38 ms，融合加速 2.55×。歷次數據在 `benches/RESULTS.md`。

## 2. 快速上手

```bash
cargo run -p pixelnode --release              # 開編輯器（要從專案根目錄執行，範例路徑是相對路徑）
bash scripts/check.sh                         # 提交前必跑：clippy -D warnings + 全部測試
cargo bench -p pixelnode-bench                # benchmark
cargo run -p pixelnode-bench --example fusion_stats   # 印出 benchmark 圖的融合統計
```

範例圖：`examples/planet.pixelnode.json`、`examples/loop_spiral.pixelnode.json`（`bundled_examples_compile` 測試會檢查它們能編譯）。

## 3. 程式碼地圖

| 路徑 | 職責 |
|---|---|
| `crates/graph/src/lib.rs` | Graph / Node / Link / Value（含 `Text`）、DataType 隱式轉換、JSON 存讀（`FORMAT_VERSION = 2`，v1 自動相容）、`subgraphs` |
| `crates/graph/src/anim.rs` | Timeline、Track / Key / Interp、`Value::lerp` |
| `crates/nodes/src/lib.rs` | `NodeDef`（kind / category / ports / params / per_pixel / time_invariant / cost / wgsl / eval）、`COMMON_WGSL`、參數 port 輔助函式、`convert` |
| `crates/nodes/src/library.rs` | 大部分 GPU 節點（含 particles、custom、image） |
| `crates/nodes/src/math.rs` | 數學節點（CPU 求值） |
| `crates/nodes/src/structure.rs` | subgraph / loop / group_in / group_out / loop_index |
| `crates/nodes/src/palette.wgsl` | 預設調色盤資料（以 include_str! 併入 Palette Map） |
| `crates/compiler/src/lib.rs` | `compile_with`、融合（`fuse` / `build_kernels`）、WGSL 生成（`kernel_wgsl` / `instantiate` / `custom_wgsl`） |
| `crates/compiler/src/schedule.rs` | pull-based 排程、循環偵測、數值節點求值、參數連線解析、step hash |
| `crates/compiler/src/flatten.rs` | Subgraph / Loop 展開 |
| `crates/runtime/src/lib.rs` | wgpu 執行器、texture pool、LRU 輸出快取、pipeline 快取（含 error scope）、讀回 / 預覽 texture |
| `crates/runtime/src/{worker,export,image}.rs` | 背景求值執行緒、GIF / PNG / Sprite Sheet 匯出、PNG 讀取與貼合 |
| `crates/app/src/main.rs` | 編輯器主體、SnarlViewer（參數 UI、keyframe 按鈕、參數 port）、預覽 |
| `crates/app/src/{history,timeline,subgraphs}.rs` | Undo（Command pattern）、時間軸面板、子圖管理 |
| `benches/` | 隨機圖產生器 + benchmark（CI 跑，融合版比未融合慢就判定失敗） |

### 核心機制
- **WGSL 片段契約**：每個節點定義 `fn node_fn(px: vec2<u32>, uv: vec2<f32>) -> vec4<f32>`，可用 `P[i]`、`in_at(k, px)`、`SIZE`、`FRAME`、`FPS`、`FRAMES`。`instantiate` 會幫頂層函式加上 `m{n}_` 前綴，並把 `P[` 改寫成 storage buffer 的對應位置。
- **融合規則**（`fuse`）：
  - 只有一個下游、且上下游都是逐像素節點 → 併入下游 kernel。
  - 被多個節點共用，但子樹成本 ≤ 12、下游 ≤ 4 個 → 在每個下游內重算。
  - 靜態且昂貴的子樹餵給會動的節點 → 先落地成 texture，讓它跨影格留在快取。
  - 單一 kernel 超過 8 個輸入 texture → 只拆最大的一個分支。
- **快取 key**：step hash = 種類 + 參數 + 上游 hash + 尺寸（與時間相關才加影格）。runtime 再串上游 key（讓 Image In 的檔案修改時間能往下傳），存在 LRU（預設 512 MB）。
- **參數 port**：Float / Int / Color 參數依序排在 Surface 輸入之後，成為輸入 port（`NodeDef::param_port_of` / `input_port`）。

## 4. 待辦（依建議優先順序）

1. **好用度**：
   - 檔案對話框（可考慮 `rfd`）
   - 每個節點的縮圖預覽
   - 複製 / 貼上節點
   - 縮放到全部節點
   - 自動存檔，關閉時提醒存檔
2. **從圖片自動抽調色盤**（median cut / k-means）：規格的「量化」只做了預設調色盤與手動顏色。
3. **真正的 GPU 粒子模擬**：規格寫的是 SoA buffer + 固定 timestep 的 compute 模擬，目前是封閉公式、逐像素收集（2048 顆每格約 4.7 ms）。可改成先散佈到 texture 再繪製，並加入亂流 / 碰撞。
4. **曲線編輯器**：目前只有常數 / 線性 / 緩入緩出三種插值。
5. **子圖擴充**：目前最多 2 個 Surface 輸入、1 個輸出，參數不能跨子圖邊界接線。
6. **規格中尚未做的項目**：
   - 拖 slider 時以 cancel token 中途取消（目前只有「只算最新的一份」）
   - RGBA16F
   - 小圖批次放進 texture array
   - Gradient / Curve / Path / Vec 型別的實際 port
   - Vector 節點
7. **Undo 缺口**：拖動節點位置，以及子圖的建立、刪除、改名，都不會進 Undo。
8. **其他**：Image In 只讀 PNG；CI 的 `actions/checkout@v4` 有 Node 20 棄用警告（可升到 v5）。

## 5. 踩過的坑

- **版本**：egui-snarl 0.12 綁 egui 0.36，所以 eframe 用 0.36（不是 0.37），wgpu 30。
- **egui 0.36 API**：
  - `App::ui(&mut self, ui, frame)`
  - `egui::Panel::top/left/right/bottom`
  - `ctx.text_edit_focused()`
  - `DroppedFile` 是 trait，要用 `f.path()`
- **egui-snarl 選取**：要用 Shift+點選或 Shift+拖曳框選；一般點擊不會選取，而且點在標題列上的點擊會被標題元件吃掉。`get_selected_nodes(graph_id(), ctx)` 只在 `SnarlWidget::id()` 有設定時才有效。
- **wgpu 30**：
  - `push_error_scope` 回傳 guard，要呼叫 `.pop()`
  - `get_mapped_range()` 回傳 Result
  - `PollType::wait_indefinitely()`
  - bind group layout 要手動建立（auto layout 會把沒用到的 binding 拿掉）
- **WGSL**：`vec2<u32>(f32, f32)` 不合法，要逐一轉型；要動態索引常數陣列，得先放進函式內的 `var` 再索引。
- **clippy 1.99**：要用 `as_chunks::<N>()`，不能寫 `chunks_exact(N)`；單一字元要用 `push('\n')`。
- **.gitignore**：曾經寫 `*.pixelnode.json`，把範例檔也排除了，導致 CI 連續失敗 10 次。現在是 `/*.pixelnode.json`。推送後記得用 `gh run list` 確認 CI 結果。
- **gh CLI**：裝在 `C:\Program Files\GitHub CLI\gh.exe`（不在 bash 的 PATH 裡），已登入帳號 cazzhong。
- **自動化測 UI**：用 PowerShell 截圖 + SetCursorPos/mouse_event。中文輸入法會吃掉 SendKeys 的英文字，要先 `Set-Clipboard` 再送 `^a^v`。視窗座標約 1.06 倍縮放；`cargo clippy` 不會重新產生 exe，要先 `cargo build -p pixelnode`。
- **Windows 換行**：`.gitattributes` 設了 `* text=auto eol=lf`，commit 時出現的 CRLF 警告可以忽略。

## 6. 提交慣例

- 每完成一個功能就跑 `bash scripts/check.sh`，全過才 commit 並推送。
- commit 訊息用繁體中文，結尾加上 `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`。
- 效能相關改動要更新 `benches/RESULTS.md`。
- 新節點加在 `crates/nodes`。`every_node_fused_matches_unfused` 會自動測試它能編譯、融合結果一致，而且輸出不是全黑；結構節點與 Image In 被排除在外。
