# Dormant 存储与 CU 预算分析（Wave 3）

> 目标：把 `clearing-core::DormantStore` 的两种实现（**Eager** 与 **Lazy**）对应到 Solana 实际 CU（Compute Unit）成本，给出"在什么活跃 bucket 数下哪种模式可行"的量化结论，避免在 1 亿用户量级才发现链上预算撞墙。

## 1. Solana 单笔交易硬约束（2026 主网现行）

| 维度 | 限制 | 引用 |
| --- | --- | --- |
| 单条 CPU 限额 | 1 400 000 CU | `compute_budget::MAX_COMPUTE_UNIT_LIMIT` |
| 单笔 tx 默认 CU | 200 000 CU（用户可自行 `set_compute_unit_limit` 提升至 1 400 000） | runtime |
| 单笔 tx 加载账户上限 | 64（`MAX_TX_ACCOUNT_LOCKS`） | runtime |
| 单 slot 时长 | ~400 ms（理论），实际中位 ~410 ms | RPC |
| BPF 单条 u64 算术 | ~1 CU | `solana-program` syscall table |
| BPF 单条 u128 算术 | ~3-5 CU（u128 加减乘）；u128/u128 div ≈ 6-10 CU | bench (informal) |
| `mul_div_floor`（u128 mul + u128 div + 边界判断） | ≈ 25-40 CU | this crate static analysis |
| Borsh 反序列化（每字节） | ~2 CU | anchor docs |
| Anchor 账户写回（每字节） | ~4-6 CU + rent re-rent check | runtime |

我们用 `mul_div_floor ≈ 30 CU` 作为算术热点单价，`account_write_overhead ≈ 1 500 CU` 作为单账户写入成本（含序列化 + rent + signer 检查）。

## 2. `clearing-core` 各 entrypoint 的算术+I/O 静态分解

`AB(K)` 表示 K 个被激活 bucket 的迭代成本，`AP` 表示一次 active 池转移的固定算术。

### 2.1 `sync_pool` —— Eager 模式

```
sync_pool(eager) =
    Pyth_validate                            ≈ 4 000 CU   // pyth-adapter parse + checks
  + price_envelope_check                     ≈    50 CU
  + pool_demand_calc                         ≈   200 CU   // ~6 mul_div_floor
  + (optional) active_pool_transfer          ≈   400 CU   // ~12 mul_div_floor
  + dormant_distribute_eager(K)              = K × 250 CU // 8 mul_div_floor + 2 写 per bucket
  + bucket_account_writes                    = K × 1 500 CU
  + rotate_check (low-equity)                ≈   150 CU
  + sub_pool_account_write                   ≈ 3 000 CU   // 大账户
  + emit_PoolSyncEvent                       ≈   400 CU
                                             —————————————
  ≈ 8 200 + K × 1 750  CU
```

K=0 时 ≈ 8 200 CU；K=80 时 ≈ 8 200 + 140 000 ≈ 148 200 CU。这是 **eager 模式的 K 上限**：

> **结论**：在默认 200 000 CU 预算下，eager 模式可承载约 **K ≤ 80** 个被激活 bucket。提升到 1 400 000 CU 后约 **K ≤ 700**。

注意：实际链上 K 受限于"该次 sync 对应的价格区间内有多少 bucket 落入激活集合"，并非 dormant_count 总数。在合理的 `max_price_move_bps_per_sync = 200`（2%）下，K 通常在 0-20 之间。

### 2.2 `sync_pool` —— Lazy 模式

```
sync_pool(lazy) =
    Pyth_validate                            ≈ 4 000 CU
  + price_envelope_check                     ≈    50 CU
  + pool_demand_calc                         ≈   200 CU
  + (optional) active_pool_transfer          ≈   400 CU
  + dormant_distribute_lazy(K_outstanding)
      // 仅用于计算 total_outstanding_at_event
      // 仍要遍历 K_outstanding 个 bucket 计算"快照"，
      // 但**只更新 ledger 一个账户**，不写每个 bucket
      = K × 100 CU            // 仅读 + 算 outstanding
  + ledger_account_write                     ≈ 2 000 CU
  + rotate_check                             ≈   150 CU
  + sub_pool_account_write                   ≈ 3 000 CU
  + emit_PoolSyncEvent                       ≈   400 CU
                                             —————————————
  ≈ 10 200 + K × 100 CU
```

K=80 时 ≈ 10 200 + 8 000 ≈ 18 200 CU；K=1 000 时 ≈ 110 200 CU；K=10 000 时 ≈ 1.01M CU（仍可放进 1 400 000 CU 预算内）。

> **结论**：lazy 模式将 sync 的有效上限从 **K ≤ 80** 扩展到 **K ≤ 13 000**（默认预算）/ **K ≤ 92 000**（最大预算）。代价是把每个 bucket 的累计应用工作转嫁给后续的 `pre_sync_dormant_bucket` 调用。

### 2.3 `pre_sync_dormant_bucket` —— Lazy 配套

```
pre_sync_dormant_bucket(N) =
    bucket_account_load                      ≈ 1 200 CU
  + ledger_account_load                      ≈ 1 500 CU
  + replay(N events)                         = N × 60 CU // ~2 mul_div_floor / event
  + bucket_account_write                     ≈ 1 500 CU
                                             —————————————
  ≈ 4 200 + N × 60 CU
```

`MarketParams::max_pending_apply_per_tx`（默认 4 096）卡住单笔上限：实际 N=4 096 时 ≈ 4 200 + 245 760 ≈ 250 000 CU（用户须自行 `set_compute_unit_limit(300_000)`）。

> **结论**：单 bucket 长时间未触碰可累积 ≤ 数千个 ledger 事件，**任何用户在第一次平仓 / claim 该 bucket 时**都需要先付一笔 `pre_sync_dormant_bucket`。Keeper 网络应在事件累积到 ~2 048 时主动批量推平，避免用户体验放大。

### 2.4 `open_position` / `close_position`

均为 O(1)，不依赖 K。

```
open_position  ≈ 2 500 CU + position_account_init_overhead 5 000 CU
                ≈ 7 500 CU
close_position ≈ 4 200 CU + lazy_migrate(M_rotates_to_apply) (每条 ≈ 50 CU)
```

`M_rotates_to_apply` 受 `RotateLog::RING_CAPACITY = 64` 卡住，最坏 ≈ 64 × 50 = 3 200 CU。

> **结论**：open/close 在任何场景下都远低于预算，**不是扩容瓶颈**。

### 2.5 `claim_dormant_recovery`

```
claim = pre_sync_dormant_bucket(N) + redeem_math + token_transfer
      ≈ 4 200 + N×60  + 800 + 4 000
      ≈ 9 000 + N × 60 CU
```

N 受 `max_pending_apply_per_tx` 限制；如果 keeper 已推平 ledger 到当前，N 接近 0，此时 claim ≈ 9 000 CU。

## 3. 模式选择矩阵（运营建议）

| 场景 | 单 sub_pool 总 dormant bucket 数 | 单次 sync 平均激活 K | 推荐模式 |
| --- | --- | --- | --- |
| 测试网 / MVP | < 50 | < 5 | **Eager** |
| 正式上线，BTC/USDC 单市场 | 50 - 1 000 | 5 - 50 | **Eager**（默认 CU 预算够用，简单） |
| 正式上线，价格大幅震荡阶段 | 1 000 - 10 000 | 50 - 500 | **Lazy** + Keeper 主动 `pre_sync_dormant_bucket` |
| 极端事件（黑天鹅，价格 ±50%） | > 10 000 | > 500 | **Lazy 必选**；同时通过治理把 `max_pending_apply_per_tx` 调到 8 192，并临时上调 CU 预算 |

切换模式由 `Market::dormant_distribute_mode`（在 `programs/mole-option/src/state.rs` 中已支持，0 = Eager / 1 = Lazy）控制。治理多签调整。

## 4. 治理可调参数全景

`Market` 账户已暴露的、可治理调整的与 dormant 相关的参数：

| 参数 | 含义 | 默认 | 紧急上限 |
| --- | --- | --- | --- |
| `dormant_distribute_mode` | 0=Eager / 1=Lazy | 0 | 1 |
| `max_dormant_bucket_count_per_direction` | 单方向 dormant bucket 数硬上限 | 1 024 | 16 384 |
| `max_pending_apply_per_tx` | 单次 `pre_sync_dormant_bucket` 可消化的 ledger 事件数 | 4 096 | 8 192 |
| `tick_aggregation_factor` | bucket tick 聚合因子（影响 bucket 总量） | 1 | 100（粗化） |
| `max_price_move_bps_per_sync` | 单步价格移动上限（影响单次 K） | 200 | 500（极端事件） |

**治理动作示例**：当观测到单 sub_pool 的 dormant bucket 数突破 1 000 时：
1. 多签提议：`dormant_distribute_mode := 1`（切 Lazy）。
2. Keeper 配置：把 `pre_sync_dormant_bucket` 触发阈值降到 ledger 累积 1 024 事件即推平。
3. 监控：`indexer::IndexerState::sub_pool_stats` 暴露的 `dormant_bucket_count` 与 `dormant_recovery_shares` 给 Telegram / Slack 告警源。

## 5. 已实现的 host 侧实现 vs on-chain 实现差距

| 能力 | host 实现 (`crates/clearing-core`) | on-chain (`programs/mole-option`) |
| --- | --- | --- |
| Eager 分发 | ✅ 已实现 + 60+ 测试 | ⏳ skeleton（账户级桥接未完成，见 `apply_clearing_view` 中的 TODO） |
| Lazy 分发（ledger） | ✅ 已实现 + 等价性 prop 测试 | ⏳ skeleton（缺少 `pre_sync_dormant_bucket` 指令 handler 与 ledger 账户布局） |
| 模式开关字段 | ✅ `MarketParams::dormant_distribute_mode` | ✅ Wave 3 已添加：`Market::dormant_distribute_mode` + `max_pending_apply_per_tx` |
| Compaction | ✅ `DormantStore::compact_ledger` | ⏳ 需要新指令 handler |
| 等价性证明 | ✅ `tests/dormant_lazy_equivalence.rs` 4 个 prop 测试，400+ 步随机游走 | N/A |

## 6. Wave 4+ 应做事项

1. **on-chain ledger 账户布局**：`SubPool` 内嵌 ledger ring buffer（容量 ≤ 256 events，超出则强制 keeper compact）或单独 `DistributionLedger` 账户；账户 reallocation 走治理流程。
2. **`pre_sync_dormant_bucket` 指令**：参数为 `bucket_pda` + `max_events_to_apply`；调用 `clearing_core::pre_sync_dormant_bucket`；写回。
3. **Keeper bot**：监听 `DistEntry` 增长，自动推平接近上限的 bucket。
4. **`solana-program-test` 集成测试**：在真实 BPF runtime 下测量 sync_pool/pre_sync 的实际 CU；与本文静态分析对比，校准 30 CU/op 的乘子。
5. **Fenwick / 段树（O(log N)）**：仅在观测到单 sub_pool 实际 K > 5 000 时考虑；当前 lazy + ledger 已足够覆盖白皮书定义的所有合理使用场景。

## 7. 安全/审计要点

- **Lazy 模式新增账户写入路径**：ledger 账户必须为 `mut`，权限固定为 SubPool 的 PDA seed；任何外部账户冒充 ledger 就能伪造 distribute 事件，导致 bucket replay 出错。审计必须验证 ledger PDA seed 不可被外部构造。
- **ledger compaction race**：keeper 在 compact 时如果 bucket 仍在 in-flight tx 引用旧 ledger 偏移，会触发 `Invariant("bucket.last_applied_index points into a GC'd ledger window")`。on-chain 必须用单调递增的 `event_index`（已实现），并在 compact 时校验所有 bucket 的 `last_applied_index >= compact_watermark`。
- **`max_pending_apply_per_tx` 治理边界**：调高至上限 8 192 时，单次 `pre_sync_dormant_bucket` 仍需 ≈ 500 000 CU；用户必须显式 `set_compute_unit_limit(600_000)` 否则交易会因 OOC 失败。前端 SDK 应根据 `bucket.last_applied_index` 与 `ledger.next_event_index` 之差自动设置 CU 预算。
- **lazy 模式下 indexer 不变**：`EngineEvent::PoolSyncEvent` 已携带 `before` 快照（`*_active_shares_before` 等），indexer 重放完全不依赖底层是 eager 还是 lazy。这是 wave 2 设计这些字段时的前瞻性收益。

---

**所有的 CU 数字都是静态估算，需要 `solana-program-test` 校准。** Wave 4 第一项任务即是在真实 BPF 运行时下跑基准并填回本表。
