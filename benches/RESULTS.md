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
