# PixelNode

高效能的節點式程序化像素美術 / 動畫工具（類似 Pixel Composer），以 Rust + wgpu 實作。
節點圖會被**編譯並融合**成少量 GPU compute kernel，參數改動不重編 shader，結果以內容 hash 快取。

![CI](https://github.com/cazzhong/PixelNode/actions/workflows/ci.yml/badge.svg)

## 執行

```bash
cargo run -p pixelnode --release
```

需要支援 Vulkan / DX12 / Metal 的 GPU（沒有實體 GPU 時會退回軟體 adapter，但很慢）。

範例：在上方路徑欄輸入 `examples/planet.pixelnode.json` 或 `examples/loop_spiral.pixelnode.json` 後按「載入」。

## 操作

| 動作 | 方式 |
|---|---|
| 新增節點 | 畫布上按右鍵，依分類選擇 |
| 連線 | 從輸出拖到輸入；每個輸入只接一條，型別不符會拒絕 |
| 選取 | Shift+點選節點，或 Shift+拖曳框選 |
| 刪除 | Del，或節點右鍵 →「刪除」 |
| 復原 / 重做 | Ctrl+Z / Ctrl+Y（每張圖各自的歷史） |
| 播放 / 逐格 | 空白鍵 / ← → |
| Keyframe | 參數左側 ◇：左鍵新增或移除目前影格的 key，右鍵選插值方式 |
| 參數連線 | Float / Int / Color 參數左側有小圓點，可接數學節點的輸出 |
| 匯入圖片 | 把 PNG 拖進視窗，或用 Image In 節點 |
| 子圖 | 左側面板新增子圖，或選取節點後按「從選取建立子圖」 |
| 匯出 | 底部：GIF、PNG 序列、Sprite Sheet（+ JSON），整數倍放大 |

## 節點（37 個）

| 分類 | 節點 |
|---|---|
| 生成 | Solid、Noise（Value / Perlin / Cellular，可循環相位）、Gradient、Shape、Checker、Particles、Image In |
| 變換 | Transform、Tile、Mirror、Crop |
| 濾鏡 | Blur、Outline、Dither、Posterize、Pixel Sort、Custom WGSL |
| 顏色 | Color Adjust、HSV、Levels、Color Replace、Gradient Map、Palette Map（Game Boy / PICO-8 / Sweetie 16 / CGA / 1-bit / 自訂，可 dither） |
| 合成 | Blend（6 種模式）、Mask、Composite |
| 數學 | Time、Wave、Random、Math、Make Color |
| 結構 | Subgraph、Loop、Group In、Group Out、Loop Index |
| 輸出 | Output |

### Custom WGSL

只寫函式內容，必須 `return vec4<f32>`。可用變數：`a`、`b`（兩個輸入的顏色）、`uv`、`px`、`size`、`frame`、`p1`–`p4`、`c1`、`c2`。
寫錯時節點輸出透明，錯誤訊息顯示在預覽面板，不會讓程式崩潰。

## 架構

```
crates/
  graph/     節點圖資料模型、型別、keyframe、JSON（含版本 migration）
  nodes/     宣告式節點定義：port / 參數 + WGSL 片段（或 CPU 求值函式）
  compiler/  子圖展開 → 型別檢查 → pull-based 排程 → 融合 → WGSL 生成
  runtime/   wgpu 執行器、texture pool、LRU 輸出快取、匯出、背景求值執行緒
  app/       eframe + egui-snarl 編輯器
benches/     benchmark（CI 會跑）
```

- **融合**：逐像素節點併入下游 kernel；被共用但便宜的子樹在每個消費者內重算；Blur / Outline 等需要鄰域的節點是融合邊界；靜態昂貴子樹餵給會動的節點時落地，讓它跨影格留在快取。
- **快取**：kernel 輸出以「參數 + 上游 hash」為 key，存在有記憶體上限的 LRU；與時間無關的分支整段動畫只算一次，不需手動標記。
- **Shader**：只依子圖結構生成，參數走 storage buffer，改參數不重編。
- **執行緒**：UI 只送 graph snapshot 給背景執行緒，只算最新的一份；預覽與 eframe 共用 GPU device，不讀回 CPU。
- **數學節點**：在 CPU 於編譯期求值，不產生 kernel。
- **子圖 / Loop**：編譯前展開成單層圖，所以能跨子圖邊界融合；Loop 最多 64 次、巢狀最多 8 層。

## 效能

`cargo bench -p pixelnode-bench`（100 節點隨機圖、256×256、60 影格，RTX 2070）：

| 情境 | 每格 |
|---|---:|
| 逐節點執行（100 kernels） | 3.38 ms |
| 融合後（33 kernels） | 1.32 ms |
| 動畫，只有一個參數會動（第一輪） | 0.67 ms |
| 動畫第二輪（全部命中快取） | 0.15 ms |
| 2048 顆粒子 | 4.75 ms |

完整紀錄見 [benches/RESULTS.md](benches/RESULTS.md)。

## 已知限制

- 粒子是解析式運動（重力 + 線性阻力），沒有粒子間互動或亂流；渲染是逐像素收集，粒子數 × 像素數的成本。
- Gradient Map 最多 4 色；Palette Map 自訂最多 11 色。
- 子圖介面只支援 2 個 Surface 輸入、1 個輸出，參數不能跨子圖邊界連線。
- 建立 / 刪除 / 改名子圖不進 Undo；拖動節點位置不進 Undo。
- Image In 只讀 PNG。

## 開發

```bash
bash scripts/check.sh
```

會跑 clippy（警告視為錯誤）與全部測試；GPU 測試在沒有 adapter 的環境會自動略過。
