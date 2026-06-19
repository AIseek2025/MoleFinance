# Wave 3：协议级端到端仿真器（protocol-harness）

> 本文件记录第三波硬核开发的交付内容、关键设计决定、所揭示的真实漏洞，
> 以及由此推导出的下一波优先级。承接 `20-攻坚开发进度与里程碑.md`、
> `21-Dormant存储与CU预算.md`。

## 一、为什么需要 protocol-harness

前两波我们已经分别得到：

- `clearing-core`：方向权益池 + shares + dormant bucket 的纯逻辑实现。
- `indexer`：消费 `EngineEvent`，重建 per-position 的"白皮书账本"。
- `pyth-adapter`：Pythnet v2 价格账户的健壮性校验。

但这些组件是**互不连通**的：

- 资金流：`clearing-core` 只更新 pool/dormant/dust 的内部数字，
  并没有人验证"链上 vault 的 SPL 余额是否始终等于这些内部数字之和"。
- 账本流：`indexer` 单独跑了等价测试，但**只在没有 rotation 的窗口**
  里被验证（`crates/indexer/tests/equivalence.rs` 的随机走子在
  `pool_equity == 0` 时显式 `break`）。
- 协议流：开仓 → sync → 平仓 → harvest 的全链路、跨仓位、跨 sub_pool
  组合从未被一起执行过，更不必说**每一步都跑一遍守恒检查**。

`protocol-harness` 把以上三块缝合成一个**可在主机端自由仿真的协议状态机**，
并把每一次操作都喂给一个不变量检查器。它是"上线前的实验台"，
也是寻找未知 bug 的猎犬。

## 二、设计要点

### 2.1 模型

```
Harness {
    market: MarketParams
    sub_pools: { sub_pool_id -> SubPool }     // 来自 clearing-core
    positions: { position_id -> Position }    // 来自 clearing-core
    vault_balance:     u128   // 用户可索取资金（pool/dormant/dust 之和）
    fee_vault_balance: u128   // 协议费用金库（open_fee + 已 sweep 的 dust）
    indexer: IndexerState
    total_deposits, total_withdrawals: u128
}
```

### 2.2 资金流账本（核心不变量）

每一笔状态变化必须同时维护以下两条等式：

1. **总账守恒**
   `total_deposits == total_withdrawals + vault_balance + fee_vault_balance`
2. **vault 分解**
   `vault_balance == Σ pool_equity + Σ dormant_accrued + Σ dust`
3. **fee_vault 一致**
   `fee_vault_balance == Σ open_fees + Σ harvested_dust`

`Harness::check_invariants()` 在所有随机/对抗测试里**每一步**之后都被调用。

### 2.3 vault 与各类操作的资金流向

| 操作            | vault         | fee_vault     |
|-----------------|---------------|---------------|
| `open(gross)`   | `+ principal_into_pool + dust` | `+ open_fee`     |
| `close`         | `- withdrawable`               | 0                |
| `force_close`   | 0（recovery 转 dust，仍在 vault 内） | 0          |
| `claim_recovery`| `- redeemable`                 | 0                |
| `sync`          | 0（仅在 vault 内部 pool/dormant/dust 之间搬运） | 0 |
| `harvest_dust`  | `- amount`                     | `+ amount`       |

## 三、测试覆盖矩阵

| 套件                      | 用例数 | 目的                                              |
|---------------------------|--------|---------------------------------------------------|
| `tests` (smoke)           | 7      | 基础往返、双向、强平、收割 dust 等"教科书"用例     |
| `random_workload`         | 1      | 4 种子 × 1000 op × 100+ trader × 3 sub_pool；每步 invariant；最终强守恒 |
| `indexer_parity`          | 3      | 受控 sync→close 平价；±2% 随机走子有界漂移；含 rotation 的聚合平价 |
| `rotation_focused`        | 7      | 单/多 rotation、跨 bucket、claim+多次 recovery、新仓加入等 |
| `adversarial`             | 9      | envelope 偏离、未确认 forfeit、跨 sub_pool 隔离、价格阶跃过大、暂停/冻结、min_margin、强平正值仓 |

合计 **27 个 harness 用例**；workspace 总用例从 wave 2 的 60 升到 **103**。

## 四、Wave 3 揭示的真实漏洞 ✅ 已修复（Wave 4 收口）

> 本节是这一波最有价值的产出之一：harness 在它的第一个 1000-op 随机走子里
> 就揪出了一个**所有现有单元/属性/等价测试都漏掉的真实漏洞**。

### 4.1 现象（已修复前）

- 触发场景：≥ ±5% 的随机价格步长 → 触发多次 active→recovery 轮转，
  叠加 multi-position bucket、中途 claim_recovery、新仓在轮转后入场。
- 偏差方向：**链上 `withdrawable` 严格 ≥ indexer 计算的 equity**。
- 量级：单笔最高 ~1.3M 单位（位仓 ~2.0M），聚合层面 ~0.7%–1.5% 的总存款。

### 4.2 根因（Wave 4 定位）

并不是 indexer 自身的算法问题。真正的根因在 **harness 的 host-side
事务语义不完整**：

- `clearing_core::close_position` 内部按以下顺序变更状态：
  `sync_pool`（mutate sub_pool, 累积事件） → `lazy_migrate_position`
  （mutate position） → 烧毁 active 份额（mutate sub_pool） →
  从 dormant bucket 中 redeem（mutate sub_pool, **可能直接删除桶**）→
  最后才检查 `withdrawable == 0`。
- 在 Solana 链上这没问题：tx 任一步出错则 runtime 自动回滚账户写入，
  事件也不会向外发射，indexer 永远看不到那次失败的尝试。
- 但 harness 的 host-side 实现没有 tx revert：当 `close_position`
  以 `WithdrawableZero` 出错时，sub_pool 已被部分变更（包括桶被
  整条删除），事件累积在 `outcome.events` 中却随 `Err` 返回值丢弃，
  **indexer 永远收不到 burn 通知**——于是产生了"indexer 还在维护
  一个链上已经不存在的 ghost bucket"的现象。后续每一次
  `distribute_recovery_profit` 都会把分母（`total_outstanding`）
  虚高，进而拉低其他 bucket 的份额，逐步累积出 0.7%+ 的漂移。

### 4.3 修复方案

**统一在 harness 层加 Solana tx-revert 语义**：每一个变更性入口
（`open` / `close` / `force_close` / `claim_recovery` / `sync` /
`harvest_dust`）在调用 engine 前先 `clone()` 一份 sub_pool（必要时
还有 position）作为快照，engine 返回 `Err` 时把快照写回，丢弃事件。
这与 Solana runtime 的 atomic commit-or-rollback 行为一一对应。

实现位置：`crates/protocol-harness/src/lib.rs` 每个 pub fn。

### 4.4 验证

| 测试                                                | 修复前 drift            | 修复后 drift           |
|-----------------------------------------------------|-------------------------|------------------------|
| `aggregate_chain_payouts_match_indexer_with_rotations` (3 seeds) | ~0.7%–1.5% / 总存款 | < 1 ppm（170 raw units / ~2×10¹⁰ deposits） |
| `random_sync_then_close_indexer_drift_is_bounded`   | ≤ 1024 raw（一直）       | ≤ 1024 raw（一直）      |
| `sync_then_close_matches_indexer_within_bounds_two_traders` | 严格相等        | 严格相等                |

聚合平价测试上限已从临时的 2% 收紧回 **1 ppm（带 1024 单位绝对地板，
覆盖 floor-rounding 噪声）**。

### 4.5 回归保护

新增 `crates/protocol-harness/tests/atomic_revert.rs`（2 个用例）：

- `failed_close_with_zero_withdrawable_leaves_chain_untouched`：
  构造长仓被深度 crash 后 close 必然 `WithdrawableZero` 的场景，
  断言 sub_pool/position/buckets/indexer 在失败 close 前后字节相等。
- `failed_close_with_active_only_position_leaves_position_untouched`：
  无 recovery shares 的子场景，验证 position 端的 `active_shares = 0`
  这种 mid-close mutation 也被回滚。

### 4.6 链端契约（写进 engine.rs 文档）

`clearing_core::close_position` 的 doc-comment 现在显式说明：
> This function is *not* internally atomic. Caller is responsible
> for snapshot/restore on Err. On-chain this is provided by the
> Solana tx runtime; off-chain the harness wraps every entry point.

任何绕过 harness 直接调用 engine 的代码（除链上 Anchor 程序外，
目前没有）都必须自己实现等价回滚。

## 五、Wave 5 优先级（按价值×紧迫度排序）

> Wave 4 已收口 indexer 漂移问题，转入下一波。

1. **Solana toolchain bring-up + `solana-program-test` 集成** — 把
   `programs/mole-option` 真正编进 SBF 目标，跑 BPF 模拟器；harness
   已经为这一步给出了"参考真值"。
2. **Dormant bucket 链上 O(log N) 激活** — 当前 `BTreeMap` 实现是
   correctness reference；上链需要 segment tree（仿射 lazy 传播）
   或 lazy-replay-on-touch 加全局事件 ledger（dormant.rs 已经有
   `distribute_lazy` / `apply_pending_to_bucket` 两个分支为这一步铺路）。
3. **CU 预算标定** — 量化 sync_pool / open / close 在不同
   active_position_count、bucket_count 下的 CU 占用，决定一笔 tx
   能容纳的最大 trader / bucket 量。
4. **Squads 多签 + `schema_version` 迁移 handler** — 治理路径。
5. **前端最小可用版本** — 直接消费 `indexer::SubPoolStats` /
   `dormant_inventory` 渲染（漂移已修复，可放心上线）。
6. **Anchor 程序与 harness 的对齐性测试** — 同一组操作流，
   `solana-program-test` 与 harness 应给出 byte-equal 的事件序列。

## 六、本波 + Wave 4 收口的累计交付物

- `crates/protocol-harness/`（新增 crate）
  - `src/lib.rs`：Harness API + 资金流账本 + 不变量检查器 + **Solana tx-revert 语义封装**
  - `src/tests.rs`：smoke 测试（7）
  - `tests/random_workload.rs`：4 种子 × 1000 op 强守恒（1）
  - `tests/indexer_parity.rs`：链 vs indexer 平价（3，bound 收紧到 1 ppm）
  - `tests/rotation_focused.rs`：受控轮转/恢复路径（7）
  - `tests/adversarial.rs`：对抗场景（9）
  - `tests/atomic_revert.rs`：tx-revert 回归（2，wave 4 新增）
- `crates/clearing-core/src/engine.rs`：close_position 的原子性契约
  写进 doc-comment
- `Cargo.toml`：workspace 新增成员
- `Docs/Planning/22-wave3-protocol-harness.md`（本文件）
- `clippy --workspace --all-targets -- -D warnings`：通过
- `cargo test --workspace`：**105/105 通过**
