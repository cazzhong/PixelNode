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
