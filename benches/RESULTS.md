# Benchmark 紀錄

`cargo bench -p pixelnode-bench` — 100 節點（亂數 DAG，seed 42）、256×256、60 影格。

## 2026-10-17 · M2 · RTX 2070

| 情境 | 總計 (ms) | 每格 (ms) | 備註 |
|---|---:|---:|---|
| compile+wgsl ×60 | 45.55 | 0.759 | |
| cold unfused ×60 | 224.34 | 3.739 | 100 kernels |
| cold fused ×60 | 146.71 | 2.445 | 70 kernels |
| cached fused ×60 | 3.86 | 0.064 | 70 dispatches（僅第一格） |
| slider drag ×60 | 115.58 | 1.926 | 2280 dispatches，0 次 shader 重編 |

融合加速 1.53×。單一 compute pass 改動前：cold unfused 357.6 ms / cold fused 257.8 ms。

## 2026-10-17 · M3 · RTX 2070

新增動畫情境（只有一個 Noise 的 phase 有 keyframe）：

| 情境 | 總計 (ms) | 每格 (ms) | 備註 |
|---|---:|---:|---|
| cold fused ×60 | 147.94 | 2.466 | 70 kernels |
| animated 1 track, 1st loop ×60 | 93.46 | 1.558 | 1368 dispatches（首格 70，之後每格 22） |
| animated 1 track, 2nd loop ×60 | 7.85 | 0.131 | 0 dispatches（整段命中 LRU 影格快取） |

與時間無關的分支以內容 hash 自動跨影格共用，不需手動標記。

## 2026-10-17 · 融合策略 v2 · RTX 2070

- 便宜的共用子樹（成本 ≤ 12、≤ 4 個消費者）在每個消費者內重算，不落地。
- texture 槽位超限時只拆最大的一個分支（原本整棵拆開）。
- 靜態且昂貴的子樹餵給會動的節點時強制落地，讓它跨影格留在快取。

| 情境 | 總計 (ms) | 每格 (ms) | 備註 |
|---|---:|---:|---|
| cold unfused ×60 | 214.33 | 3.572 | 100 kernels |
| cold fused ×60 | 86.75 | 1.446 | **33 kernels**（原 70） |
| cached fused ×60 | 2.30 | 0.038 | |
| animated 1 track, 1st loop ×60 | 50.14 | 0.836 | 254 dispatches（原 1368） |
| animated 1 track, 2nd loop ×60 | 7.24 | 0.121 | 0 dispatches |
| slider drag ×60 | 128.79 | 2.146 | 1320 dispatches |

融合加速 1.44× → **2.47×**。
