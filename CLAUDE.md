# PixelNode — 高效能節點式像素藝術工具

類似 Pixel Composer 的獨立節點式程序化像素美術 / 動畫工具。**核心目標：效能遠優於 Pixel Composer。**
環境：Windows 11，Rust 1.99 / cargo 已安裝。回覆使用繁體中文。

## 技術棧
- Rust workspace
- `wgpu`（GPU，含 compute shader）
- `egui` / `eframe` + `egui-snarl`（或 `egui_node_graph`）作節點畫布
- 序列化：`serde` + JSON（含版本號以便 migration）

## 要避開的 Pixel Composer 效能問題
| 問題 | 對策 |
|---|---|
| GameMaker 單執行緒、無 compute | Rust + wgpu |
| 每節點一張 surface、一個 pass | **節點融合（kernel fusion）** |
| 粒子 / 物理跑 CPU | GPU compute |
| 失效範圍粗、大量重算 | hash 快取 + 精細 dirty 傳播 |
| UI 與運算同執行緒 | 背景求值執行緒，UI 永不阻塞 |
| 動畫逐格重算 | LRU 影格快取（有記憶體上限）+ time-invariant 標記 |

## 架構原則
1. **圖編譯管線**：節點圖 → 型別檢查 → 拓撲排序 → 融合逐像素節點 → 生成 WGSL → 資源配置 → 執行。
   - 逐像素節點（Color/Levels/Blend/Math/Gradient Map…）融合成單一 shader。
   - 需鄰域取樣（Blur/Outline）或全域資訊（Pixel Sort）的節點為融合邊界。
   - 融合後 shader 以「子圖結構 hash」快取；參數走 uniform buffer，改參數不重編。
2. **資源**：Texture pool 依（尺寸、格式）分桶重用，依生命週期分析即時回收。預設 `RGBA8`，必要時 `RGBA16F`。小圖可批次進 texture array。
3. **增量求值**：輸出 key = hash(參數 + 輸入 hash + 影格)；pull-based，只算可見預覽與輸出需要的分支。
4. **動畫**：與時間無關的節點標記 time-invariant，整段動畫只算一次。
5. **執行緒**：UI 執行緒只繪製與輸入；求值執行緒吃 immutable snapshot；拖 slider 時以 cancel token 取消過時工作。
6. **粒子**：compute shader、SoA buffer；固定 seed 與固定 timestep 以保證可重現。
7. **Undo**：Command pattern，只記 diff，不存整圖快照。從第一天就設計進去。
8. **節點宣告式定義**：每個節點 = port/參數描述 + 一段 WGSL 函式片段，供融合器拼接，也方便日後開放自訂節點。

## 專案結構
```
crates/
  graph/     # 節點圖資料模型、型別系統、序列化（無 GPU 依賴，可單元測試）
  compiler/  # 拓撲排序、循環偵測、融合、WGSL 生成
  runtime/   # wgpu 資源池、執行器、快取
  nodes/     # 宣告式節點定義
  app/       # eframe/egui 編輯器、節點畫布、預覽、時間軸
benches/     # 效能 benchmark
```

## 型別系統
Surface、Float、Int、Vec2/3/4、Color、Gradient、Curve、Path、Particle；支援隱式轉換（如 Float→Color）。

## 里程碑
- **M1 基礎（下一步）**：建 workspace；graph crate；5 個逐像素節點（Noise、Solid、Color Adjust、Blend、Output）；未融合執行器；egui 節點畫布可拖節點、連線、即時預覽 Noise → Color → Output；JSON 存讀檔。
- **M2 效能核心**：融合編譯器、texture pool、hash 快取、背景求值執行緒、benchmark（目標情境：100 節點、256×256、60 影格），benchmark 進 CI。
- **M3 動畫**：時間軸、keyframe + 曲線、影格快取、time-invariant、GIF / PNG 序列 / Sprite Sheet 輸出。
- **M4 擴充**：GPU 粒子、Subgraph、Loop、調色盤工具（量化、dither、palette map）、自訂 WGSL 節點。

## 第一批節點清單（約 30 個，M1 後陸續補）
- 生成：Solid、Noise（Perlin/Cellular/Simplex）、Gradient、Shape、Checker
- 變換：Transform、Tile、Mirror、Crop、Scale（nearest）
- 濾鏡：Blur、Outline、Dither、Posterize、Palette Map、Pixel Sort
- 合成：Blend（多種模式）、Mask、Composite
- 顏色：HSV、Levels、Color Replace、Gradient Map
- 數學：Math、Vector、Time、Random、Wave
- I/O：Image In、PNG/Sprite Sheet/GIF Out

## 慣例
- 像素處理一律 point filtering、整數縮放。
- 效能相關改動需附 benchmark 數據。
