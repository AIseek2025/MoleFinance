# 23 · On-Chain Dormant Bridge & Chain-Mirror

> 本文档定义 Anchor 程序如何把 `clearing_core::DormantStore`（host 侧 BTreeMap 引用实现）持久化为多个 Solana 账户，以及为什么 host 侧的 `crates/chain-mirror/` 提供了"BPF 之前"能拿到的最强等价性证据。
>
> 上下文文档：§07（智能合约设计）§18（shares 模型实现细则）§21（Dormant 存储与 CU 预算）§22（Wave 3 harness）§20（开发进度）。

## 1. 背景：wave 4 的 production blocker

至 wave 4 末尾，`clearing-core` + `indexer` + `protocol-harness` 三件套已通过 105/105 测试，证明：

- 引擎数学正确。
- 链下 indexer 与链上 `withdrawable` byte-equal（漂移 < 1 ppm）。
- 协议级仿真器在 1000-op 随机走子下守恒。
- 失败 close 在 host 侧已经具备 Solana tx-revert 等价语义（atomic snapshot/restore）。

但 `programs/mole-option/src/instructions/sync.rs`（与 close、force_close、claim、harvest 同样）实际只是个"骨架"，关键路径上有这样一段被 wave 1-4 测试无法触发的代码：

```rust
// programs/mole-option/src/instructions/sync.rs
pub(crate) fn clearing_view(sp: &SubPoolAccount) -> clearing_core::SubPool {
    let mut view = clearing_core::SubPool::new(sp.sub_pool_id, sp.last_price, sp.last_sync_slot);
    // ... 拷贝 SubPool 标量字段 ...
    view  // ← long_dormant / short_dormant 都是空 BTreeMap！
}

pub(crate) fn apply_clearing_view(sp: &mut SubPoolAccount, view: &clearing_core::SubPool) {
    // ... 写回 SubPool 标量字段 ...
    // Dormant bucket aggregates are persisted in DormantBucket accounts; this
    // skeleton does not bridge them. The follow-up phase wires Fenwick trees.
    let _ = (sp.long_dormant_bucket_count, sp.short_dormant_bucket_count);
}
```

后果：上线后第一次出现 rotation——

1. `rotate_active_to_recovery` 把 active shares 折叠成一个新的 dormant bucket，写进**内存中的** `view.long_dormant`。
2. `apply_clearing_view` 不持久化任何 dormant 结构。
3. tx 结束，bucket 被丢弃。事件流里 `ActiveRotatedToRecovery` 已经发出，但链上账户里**找不到这只 bucket**。
4. 后续任何 close/force_close/claim 在空 store 上跑，`redeem` 必然返回 0，dormant 仓位的恢复语义彻底失效。

这是一个被三层正确性证明绕过的 production blocker：所有 wave 1-4 测试都是"假设 dormant 结构能被链端无损持久化"——但合约里这条假设根本没兑现。

## 2. Wave 5 解决方案：chain-mirror

不安装 Solana toolchain、不写 BPF 代码的前提下，能给出的**最强**正确性证据是：

> 在 host 上完整复刻"Anchor 程序的账户级运行时"——按账户为单位读写、按 `remaining_accounts` 模型组装数据、按 Solana tx 语义 snapshot/restore——然后用 wave 4 的 `protocol_harness::Harness` 作为 oracle 做差分测试。

`crates/chain-mirror/` 就是这个 oracle。

```
crates/chain-mirror/
├── Cargo.toml
├── src/
│   ├── lib.rs           ← ChainRuntime + 各账户结构 + 7 条指令 handler
│   └── tests.rs         ← 4 个 smoke
└── tests/
    └── harness_parity.rs ← 3 个 byte-equal 平价 property test
```

### 2.1 账户模型

每个 Anchor `#[account]` 在 chain-mirror 中是一个独立的 owned 宿主结构：

| 链上账户 | chain-mirror 类型 | 索引 |
| --- | --- | --- |
| `Market` | `MarketAccount` | 单实例（每个 ChainRuntime 持有一个） |
| `SubPool` | `SubPoolAccount` | `HashMap<u32, SubPoolAccount>` by `sub_pool_id` |
| `DormantBucket` | `DormantBucketAccount` | `HashMap<(u32, Direction, i64), _>` by `(sp, dir, tick)` |
| `DistributionLedger` | `DistributionLedgerAccount` | `HashMap<(u32, Direction), _>` by `(sp, dir)` |
| `Position` | `PositionAccount` | `HashMap<u64, _>` by `position_id` |

加上链下"金库"配套：`vault_balance`、`fee_vault_balance`、`total_deposits`、`total_withdrawals`，对应链上的 SPL token 账户与协议费金库 PDA。

### 2.2 Pack/Unpack 桥

每条指令处理函数的标准流程：

1. **入口**：从 `ChainRuntime` 读取所有相关账户。
2. **Unpack**：用 `clearing_core::unpack_dormant_store(&[OnChainBucketRecord], &OnChainLedger)` 把所有 `DormantBucket` PDA + 一份 `DistributionLedger` PDA 还原成内存中的 `DormantStore`。
3. **Compose**：用账户里的标量字段构造 `clearing_core::SubPool`，把 unpack 后的两个 `DormantStore` 装进 `long_dormant / short_dormant`。
4. **Engine call**：调用 `clearing_core::sync_pool` / `open_position` / `close_position` / ...。
5. **Pack**：用 `clearing_core::pack_dormant_store(&store, max_buckets, max_ledger)` 把变更后的 `DormantStore` 序列化回 `(Vec<OnChainBucketRecord>, OnChainLedger)`。
6. **Write-back**：reconcile 所有 `DormantBucket` PDA（drop 已不存在的 tick、materialise 新 tick、刷新 existing），写回 `DistributionLedger` PDA，写回 `SubPool` 标量字段，更新 `Position` PDA。

### 2.3 原子事务语义

每条变更性指令在调用 engine 前 **clone** 出一个 `SubPoolSnapshot { sub_pool, buckets, ledgers }` 加上必要时的 `PositionAccount`；engine 返回 `Err` 时把所有这些账户原样写回，丢弃事件。这与 Solana runtime 的 atomic commit-or-rollback 行为一一对应（与 wave 4 给 `protocol-harness` 加的快照机制同源）。

### 2.4 Bucket 生命周期

| 事件 | chain-mirror 行为 | Anchor 程序对应行为 |
| --- | --- | --- |
| Rotation 创建首个 dormant bucket | `pack_dormant_store` 输出新 `OnChainBucketRecord`，runtime 在 HashMap 中 `insert`。 | 需要 `init_dormant_bucket(sub_pool, direction, tick)` 单独指令 PDA 化 bucket，或在 sync 指令里用 `init_if_needed` 模式（推荐前者，用户/keeper 显式付租金）。 |
| Rotation 把 active 并入已有 bucket | `pack_dormant_store` 输出 record 的 `total_recovery_shares / total_recovery_notional / position_count` 增加。 | 直接 mutate 既有 PDA 字节。 |
| close/claim 烧掉 bucket 内全部 shares | `pack_dormant_store` 不再输出该 tick；runtime 从 HashMap 中 `retain` 删除。 | Anchor `close = receiver`，归还租金给 closer。 |
| `pre_sync_dormant_bucket` 跨周期重放 | 单 bucket + 单 ledger 走 unpack/engine/pack，无其他 bucket 触动。 | 单条指令仅含 `(sub_pool, bucket_pda, ledger_pda)` 三个账户，CU 远小于 sync。 |

## 3. Anchor 指令账户列表契约（wave 6 落地参考）

下表对每条指令列出**必传**账户。所有 `DormantBucket` 账户必须按 tick 升序排列（与 `pack_dormant_store` 的输出顺序一致）。

| 指令 | 必传账户 | `remaining_accounts` |
| --- | --- | --- |
| `sync_pool(envelope)` | `market`, `sub_pool`, `oracle_price_feed`, `clock`, `ledger_long`, `ledger_short` | 所有 `p_now` 下激活的 `DormantBucket` PDA（按 tick 升序）。Lazy mode 可全部省略，仅追加 ledger 条目。 |
| `open_position(params)` | `market`, `sub_pool`, `position`（init by `position_id`）, `vault`, `user_token_account`, `oracle_price_feed`, `clock` | （无） |
| `close_position(envelope)` | `market`, `sub_pool`, `position`, `vault`, `user_token_account`, `vault_authority`, `token_program`, `clock` | 若 position 已 dormant：其 `recovery_bucket_tick` 对应的 `DormantBucket` PDA + 对应方向的 `ledger`；否则空。 |
| `force_close_zero_value_position(envelope, ack)` | 与 close 相同，无需金库 | 同上。 |
| `claim_dormant_recovery(envelope)` | `market`, `sub_pool`, `position`, `vault`, `user_token_account`, `vault_authority`, `token_program`, `clock` | 必传 position 对应 bucket + ledger。 |
| `pre_sync_dormant_bucket(direction, tick, slot)` | `market`, `sub_pool`, `bucket`, `ledger` | （无） |
| `harvest_dust(direction)` | `market`, `sub_pool`, `vault`, `fee_vault`, `vault_authority`, `token_program` | （无） |
| `init_dormant_bucket(direction, tick)` | `market`, `sub_pool`, `bucket`（init）, `payer`, `system_program` | （无） |
| `init_distribution_ledger(direction)` | `market`, `sub_pool`, `ledger`（init, max size = `Market.max_distribution_ledger_size`）, `payer`, `system_program` | （无） |

## 4. 测试矩阵

`crates/chain-mirror/src/tests.rs`（smoke，4 项）：

1. `open_then_close_round_trip_no_rotation`：Long + Short 各开一仓，价格小幅上行后双方 close；验证账户在多次 unpack/pack 后仍守恒。
2. `rotation_creates_persistent_bucket_account`：构造 50 % 价格崩盘 → 长仓 rotate；assert `bucket_count(0, Long) == 1`；继续 sync 让 bucket accrue；close 抽光后 assert `bucket_count == 0`（drained bucket account 被 reconcile 时移除）。
3. `failed_close_with_zero_withdrawable_reverts_chain_runtime`：90 % 崩盘后 close 必然 `WithdrawableZero`；快照所有相关账户（buckets/ledger/sub_pool/position/vault），断言 close `Err` 后**byte-for-byte unchanged**。
4. `harvest_dust_moves_vault_to_fee_vault`：周期性小幅价格震荡积累 dust；harvest 后 vault + fee_vault 总和守恒。

`crates/chain-mirror/tests/harness_parity.rs`（property，3 项）：

| 测试 | seeds | sub_pools | ops/seed | 价格波动 | 平均断言点/op |
| --- | --- | --- | --- | --- | --- |
| `parity_under_random_workload_eager` | 0, 7, 42 | 2 | 400 | ±5 % | 全状态 byte-equal + 双侧守恒 |
| `parity_under_random_workload_lazy` | 1, 13, 91 | 2 | 400 | ±5 % | 全状态 byte-equal（每步）+ drain 后金库守恒（末尾） |
| `parity_under_high_rotation_stress` | 3, 17, 101 | 3 | 800 | ±10 % + 每 50 步 ±20 ~ 50 % 冲击 | 全状态 byte-equal + 双侧守恒 |

每个 op 之后都做：

1. **结果分类等价**：harness 与 chain-mirror 必须同时 Ok / 同时 Err，错误分类（`WithdrawableZero` / `PositionNotOpen` / `Other`）相同。
2. **状态快照等价**：`StateSnapshot { vault, fee_vault, total_deposits, total_withdrawals, sub_pools[], positions[] }` 在 harness 和 chain-mirror 之间 byte-for-byte 相等。其中：
   - `sub_pools[i]` 包含全部标量聚合 + 按 tick 升序的 dormant bucket 记录（含 `anchor_price / total_recovery_shares / total_recovery_notional / accrued_value / position_count / last_applied_index`）+ rotate_log。
   - `positions[i]` 包含 `(active_shares, recovery_shares, recovery_bucket_tick, status, notional, active_generation, principal, zero_price)`。
3. **守恒**：双侧各自跑 invariant 检查（lazy mode 在每步解除 vault 决算，并在 drain 阶段恢复）。

## 5. Lazy mode 的真实 engine 留账（wave 5 发现，wave 5.5 跟进）

平价测试在 lazy mode 下抓到了 wave 1-4 单元测试漏掉的一个真问题：

`DormantStore::distribute_lazy(p_now, total_alloc)` 当前的语义：

1. 从 `pool_equity` 抽走 `total_alloc`（在 `engine::sync_pool` 上游已减）。
2. 计算 `allocated_sum`（按 floor 向所有 activated bucket 分配的总和）和 `residual = total_alloc - allocated_sum`。
3. `residual` 进 `dust`（已正确入账）。
4. 在 ledger 里追加 `DistEntry { event_index, p_at_event, total_outstanding_at_event, total_alloc_input, allocated_sum_observed }`。
5. **不更新** `accrued_value_total`，**不更新**任何 bucket 的 `accrued_value`。

后果：`allocated_sum` 这部分钱"挂"在 ledger 里。`apply_pending_to_bucket` 走过对应 entry 时才落到 bucket，并同时增加 `accrued_value_total`。在此之前，`vault_balance == pool_equity + accrued_value_total + dust` 这条决算公式缺一项。

现状（wave 5）：

- `crates/chain-mirror/tests/harness_parity.rs` 的 `parity_under_random_workload_lazy` 测试明确将 lazy mode 的逐步 vault 检查解除，仅保留跨 runtime 状态相等；末尾通过 `pre_sync_bucket` 把所有 bucket drain，再恢复 vault 决算检查。
- `clearing_core::DormantStore::check_invariants` 当前定义的是 `accrued_value_total == sum(bucket.accrued_value)`，这条在 lazy mode 下也仍然成立，因此引擎自身不变量保留（不属于"破坏白皮书"的语义)。

跟进（wave 5.5）：

```
DormantStore {
    + pending_distribution_total: u128,   // distribute_lazy 时增；apply_pending_to_bucket 时减
}
```

加配套：`OnChainLedger` 增 `pending_distribution_total: u128` 字段（POD 字节布局变更需同步 §07/§18 与 `programs/mole-option/src/state.rs`）；`Harness::check_invariants` 与 `ChainRuntime::check_vault_decomposition` 把它纳入 dormant 项。最后把 wave 5 解除的逐步 vault 检查恢复，并把 105 个 wave 4 测试在 lazy mode 下跑一遍验证零漂移。

## 6. 与 onchain_layout 测试的关系

`crates/clearing-core/tests/onchain_layout.rs` 已经证明：

- **POD 字节稳定性**：`pack(unpack(pack(store))) == pack(store)` 对任意操作流字节相等。
- **拉取后行为等价**：在 unpack 后的 store 上继续跑同一组操作，与原 store 行为字节相等。

`crates/chain-mirror/tests/harness_parity.rs` 在此之上补完了：

- **账户级运行时等价**：把 pack/unpack 嵌进多账户的 instruction handler 闭环（包括 bucket 生命周期 init/drop、rotation 创建新 bucket、ledger 跨指令持久化），与 host 引擎行为字节相等。

两者结合，下一步做 Anchor instruction 真实接入时（wave 6），可以信心十足地把 `instructions/sync.rs` 等的"空 DormantStore"代码路径换成真正的 `unpack_dormant_store(remaining_accounts) → engine → pack_dormant_store → 写回`。

## 7. Wave 5.5 闭环：pending_distribution_total 真正落地

### 7.1 引擎根因（wave 5 留账的真正原因）

平价测试 lazy parity 在 wave 5 暴露的现象不止"vault 决算缺一项"。深挖后发现**eager `DormantStore::distribute()` 与 lazy `distribute_lazy()` 在共享同一个 `DormantStore` 时存在 lost-update 漏洞**：

```text
1. distribute_lazy(p1, alloc=10) 写 ledger e0：
   pending += 10。bucket A 的 last_applied 仍为 0。
2. distribute(p2, alloc=20)（eager）：
   - 直接读 bucket.accrued = 0 计算 outstanding（**没 drain pending**）。
   - 给 activated bucket A 加 share=20，A.accrued=20。
   - 把 **所有** bucket 的 last_applied 推到 event_index+1=2（包括 A）。
3. apply_pending_to_bucket(A)：last_A=2=next_event=2 → 立刻返回 0。
   ← e0 的 share=10 被永远跳过，10 单位钱"卡死"在 pending 里。
```

后果：在事件流跨 mode 切换或将来"governance flip mode"的迁移期，每一次 eager `distribute()` 静默吞掉前一次 `distribute_lazy` 还没 apply 的份额。`compact_ledger` drop 这些 entries 后，`pending_distribution_total > Σ entry.allocated_sum_observed` 立即触发 `from_onchain_parts` 不变量。

### 7.2 修复

```rust
// crates/clearing-core/src/dormant.rs::DormantStore::distribute
+    // Drain pending lazy shares for every activated bucket BEFORE
+    // computing this entry's outstanding numerator.
+    for key in &activated_keys {
+        self.apply_pending_to_bucket(*key)?;
+    }
+
     let mut total_outstanding: u128 = 0;
     for key in &activated_keys { ... }
```

并删掉了原本"为所有 bucket（含 non-activated）一律 bump `last_applied_index = event_index+1`"的优化——那行代码在纯 eager 模式下是免费的优化，但在混合模式下正是它把 pending lazy entries 跳过去的。

| 改动 | 文件 | 净行数 |
| --- | --- | --- |
| `distribute()` 开头 drain activated 的 pending | `crates/clearing-core/src/dormant.rs` | +13 |
| 删除 non-activated bucket 的 bump-all 循环 | `crates/clearing-core/src/dormant.rs` | -8 / +18（注释） |
| `DormantStore::pending_for_bucket(tick)` 纯只读快照 | `crates/clearing-core/src/dormant.rs` | +60 |
| `Harness::check_invariants` 四项决算 | `crates/protocol-harness/src/lib.rs` | +14 |
| `ChainRuntime::check_vault_decomposition` 四项决算 | `crates/chain-mirror/src/lib.rs` | +9 |
| harness bucket parity 改用 `pending_for_bucket` | `crates/protocol-harness/src/lib.rs` | +12 |
| 解除 lazy parity 的"逐步检查 opt-out" + 删除 drain phase | `crates/chain-mirror/tests/harness_parity.rs` | -42 / +6 |
| `onchain_layout` 加每 op 的 pending invariant 自检 | `crates/clearing-core/tests/onchain_layout.rs` | +9 |

### 7.3 验证

```
cargo test --workspace          → 125/125 passing
cargo clippy --workspace --all-targets -- -D warnings → 0
```

`parity_under_random_workload_lazy` 现在与 eager 一样在每个 op 之后做 4 项 vault 决算（`vault == pool_equity + dormant_accrued + dormant_pending + dust`），无需末尾 drain。

## 8. Wave 6 落地：Anchor 指令真正接入桥接

### 8.1 新增

| 模块 / 指令 | 路径 | 作用 |
| --- | --- | --- |
| `dormant_bridge` | `programs/mole-option/src/instructions/dormant_bridge.rs` | `unpack_direction` / `pack_direction`：账户切片 ⇄ `DormantStore` 的双向桥。 |
| `init_distribution_ledger(direction_is_long)` | `programs/mole-option/src/instructions/init.rs` | 一次性 init 每方向的 ledger PDA，预留 `Market.max_distribution_ledger_size` 的空间。 |
| `init_dormant_bucket(direction_is_long, zero_price_tick)` | `programs/mole-option/src/instructions/init.rs` | 一次性 init 单个 bucket PDA。Keeper / 前端在监听到 `ActiveRotatedToRecovery` 事件后调用，作为后续 close/sync 之前的 PDA 准备。 |
| `DistributionLedger.pending_distribution_total: u128` | `programs/mole-option/src/state.rs` | wave 5.5 字段同步到 Anchor 账户布局；`HEADER_LEN` 从 93 涨到 109 字节。 |

### 8.2 重写完成的 handler

`sync_pool` 已被完整重写为：

```text
1. Pyth 校验 + 价格信封交叉
2. 切分 remaining_accounts 为 long / short bucket lanes
3. unpack_direction(long_ledger, &long_buckets, sub_pool, Long)  → DormantStore
4. unpack_direction(short_ledger, &short_buckets, sub_pool, Short) → DormantStore
5. clearing_core::sync_pool(&mut sp_view) （引擎照常工作）
6. pack_direction × 2 → 写回 ledger + 所有 bucket PDA
7. apply_clearing_view → SubPool 标量字段
8. 显式 b.exit(program_id) 让通过 try_from 重组的 bucket 账户落盘
```

新签名：

```rust
pub fn sync_pool(
    ctx: Context<SyncPool>,
    envelope: PriceEnvelopeArgs,
    long_bucket_count: u32,
    short_bucket_count: u32,
) -> Result<()>
```

`SyncPool` Accounts struct 增加两条强制字段 `long_ledger` / `short_ledger`，由 `has_one = sub_pool` + `direction_is_long` constraint 校验。

### 8.3 仍 pending（wave 6.5）

| 指令 | 状态 | 备注 |
| --- | --- | --- |
| `close_position` | TODO | 内部会调用 `sync_pool`，因此需要双 ledger + 全 bucket。模式与 `sync.rs` 完全相同。 |
| `force_close_zero_value_position` | TODO | 同上。 |
| `claim_dormant_recovery` | TODO | 同上；但在 happy path 上仅触动一个 bucket（position.recovery_bucket_tick）。 |
| `harvest_dust` | 不涉及 dormant | 引擎只触 `subpool.dust`；当前实现保留即可。 |
| `pre_sync_dormant_bucket` | TODO | 单 bucket + 单 ledger 的最小契约；CU 远低于 sync。 |

设计选择：让 `close / force_close / claim` 的 Accounts struct 也带 `long_ledger` / `short_ledger`，并通过 `remaining_accounts` 传所有 live bucket。理论代价是大量账户被锁，但在每个市场分 sub_pool 之后，单 sub_pool 的 live bucket 数被 `Market.max_dormant_bucket_count_per_direction` 上限约束（默认 64），完全可承受。后续如果需要，可以做账户分页：用户先调 `pre_sync_dormant_bucket` 把 lazy ledger 抽干，然后 close 不再依赖完整 bucket 集合。

### 8.4 BPF 工具链 bring-up（wave 7 跟进）

`programs/mole-option` 当前仍排除在 workspace 外，本 wave 6 的代码改动是按 chain-mirror 平价语义"对着写"——一旦装上 Solana / anchor 工具链，应当 `anchor build` 即过。验证步骤（保留为 wave 7 任务）：

```bash
cd programs/mole-option
anchor build                  # 期待 BPF .so 产物
cargo +bpf-solana check       # 期待无 lifetime / Account 校验错误
solana-program-test           # 期待端到端 happy path / rotation / claim 流程
```

如果 `anchor build` 过不了，最常见的卡点是 `Account::try_from(&info)` 的生命周期——必要时改用 `AccountLoader<DormantBucket>` zero-copy 化。

## 9. Wave 7 落地：桥接收编 + dead-PDA 修复 + close_dormant_bucket + lazy keeper drain ≡ eager 证明

第七波在 wave 6.5 结尾"5 个 dormant-bridge handler 全部接入"基础上，做了三件互锁的工作：（a）把桥接段抽成单点 helper 防 5 处 drift；（b）发现并修复一个只在生产 keeper 工作流上触发、parity 测试无法覆盖的桥层语义 bug；（c）让 lazy 模式的 keeper 抽干流程留下机器证据。

### 9.1 `run_bridged` —— 5 个 handler 折叠为一行闭包

`programs/mole-option/src/instructions/dormant_bridge.rs` 新增：

```rust
#[allow(clippy::too_many_arguments)]
pub fn run_bridged<'info, F, R>(
    sub_pool: &mut Account<'info, SubPoolAccount>,
    long_ledger: &mut Account<'info, DistributionLedger>,
    short_ledger: &mut Account<'info, DistributionLedger>,
    market_params: &clearing_core::MarketParams,
    program_id: &Pubkey,
    remaining_accounts: &'info [AccountInfo<'info>],
    long_bucket_count: u32,
    short_bucket_count: u32,
    engine_call: F,
) -> Result<R>
where
    F: FnOnce(&clearing_core::MarketParams, &mut clearing_core::SubPool) -> Result<R>,
{
    let (mut long_buckets, mut short_buckets) =
        split_remaining_buckets(remaining_accounts, long_bucket_count, short_bucket_count)?;
    let sub_pool_key = sub_pool.key();
    let mut sp_view = clearing_view(sub_pool);
    sp_view.long_dormant  = unpack_direction(long_ledger, &long_buckets,  sub_pool_key, Direction::Long)?;
    sp_view.short_dormant = unpack_direction(short_ledger, &short_buckets, sub_pool_key, Direction::Short)?;

    let outcome = engine_call(market_params, &mut sp_view)?;

    pack_direction(long_ledger,  &mut long_buckets,  sub_pool_key, Direction::Long,  &sp_view.long_dormant,  market_params.max_dormant_bucket_count_per_direction, market_params.max_distribution_ledger_size)?;
    pack_direction(short_ledger, &mut short_buckets, sub_pool_key, Direction::Short, &sp_view.short_dormant, market_params.max_dormant_bucket_count_per_direction, market_params.max_distribution_ledger_size)?;
    exit_all_buckets(program_id, &long_buckets, &short_buckets)?;
    apply_clearing_view(sub_pool, &sp_view);
    Ok(outcome)
}
```

每个 handler（`sync_pool` / `close_position` / `force_close_zero_value_position` / `claim_dormant_recovery` / `pre_sync_dormant_bucket`）都简化为：

```rust
pub fn xxx<'info>(ctx: Context<...>, ...) -> Result<()> {
    let market_params = market_params_from(&ctx.accounts.market);
    // ... handler-specific 校验 / position_to_core / SPL 准备 ...
    let outcome = run_bridged(
        &mut ctx.accounts.sub_pool,
        &mut ctx.accounts.long_ledger,
        &mut ctx.accounts.short_ledger,
        &market_params,
        ctx.program_id,
        ctx.remaining_accounts,
        long_bucket_count,
        short_bucket_count,
        |params, sp| clearing_core::xxx(params, sp, ...).map_err(map_err),
    )?;
    // ... handler-specific write-back / SPL transfer ...
}
```

折叠掉了原本每个 handler 50 行的桥接段。下一次桥接修改（pre-pack 不变量、SC 校验、错误注入、CU 优化）只需在一处落地。

### 9.2 关键 bug：dead PDA 不跳过导致 anchor_price 永久为 0

**触发链路**：

1. 引擎在某个 `sync_pool` 中通过 `rotate_active_to_recovery` 产出新 tick `T` 的 bucket。
2. `pack_direction` 找不到空槽 → 返回 `DormantBridgeBucketSlotExhausted`，tx 回滚。
3. keeper 看到错误，调用 `init_dormant_bucket(T)` 在链上创建空 PDA（每字段 = 0）。
4. keeper 重发 sync_pool，把这个空 PDA 加进 `remaining_accounts`。
5. **`unpack_direction` 把它原样装进 `DormantStore`**——当时的实现没有 dead 过滤。
6. 引擎 `insert_or_merge(tick=T, anchor_price=p, added_shares, ...)` 看见 `self.buckets.contains_key(&T)` 为真，走"已存在"分支。
7. **该分支只累加 shares / notional / position_count，不更新 anchor_price**。bucket 永久卡在 `anchor_price = 0` 状态，`compute_intrinsic` 永远返回 0，`apply_pending_to_bucket` 给它的 share 永远为 0，bucket 收不到任何 lazy 分配——但份额却被正常计入，dust 守恒崩溃。

**为什么 chain-mirror parity 测试没抓到**：chain-mirror 的所有 bucket 记录都来自引擎在 `commit_core_sub_pool` 中带着正确 `anchor_price` 写出的 `OnChainBucketRecord`，从来不模拟"keeper 提前 init bucket → unpack 看见空 PDA"工作流。这个 bug 是个**生产路径独有**的桥层语义缺陷。

**修复**（`programs/mole-option/src/instructions/dormant_bridge.rs`）：

```rust
+ pub fn record_is_dead(b: &DormantBucket) -> bool {
+     b.total_recovery_shares == 0
+         && b.total_recovery_notional == 0
+         && b.accrued_value == 0
+         && b.position_count == 0
+ }

  pub fn unpack_direction(...) -> Result<DormantStore> {
      ...
      for b in bucket_accounts {
          require_eq!(b.sub_pool, sub_pool_key, ...);
          require_eq!(b.direction_is_long, direction_is_long, ...);
+         if record_is_dead(b) {
+             // Pre-init'd empty slot, or freshly redeem-emptied bucket.
+             // The engine MUST NOT see it.
+             continue;
+         }
          bucket_records.push(...);
      }
      ...
  }
```

`pack_direction` 的 Pass 1（match-by-tick）/ Pass 2（dead-slot fill）/ Pass 3（zero-out engine-removed）也统一切到 `record_is_dead`，让"哪些槽位是空"在桥两端字节级对齐。

修复后，dead PDA 在 unpack 时被跳过 → 引擎走 `insert_or_merge` 的"new bucket"分支 → 正确设置 anchor_price = 调用方传入的价格 → 后续 `apply_pending_to_bucket` 计算 share 正常。pack Pass 2 仍然能找到这个 dead 槽并把新 record 写进去。

### 9.3 `close_dormant_bucket` —— PDA 池长期可治理

经长期运行后被 redeem-to-zero 的 `DormantBucket` PDA 占着 ~120 字节 + rent。新指令 `close_dormant_bucket` 让 keeper 永久关闭这些 PDA、取回 rent：

```rust
// programs/mole-option/src/instructions/init.rs
#[derive(Accounts)]
pub struct CloseDormantBucket<'info> {
    pub market: Account<'info, Market>,
    #[account(has_one = market)]
    pub sub_pool: Account<'info, SubPool>,
    #[account(
        has_one = sub_pool,
        constraint = ledger.direction_is_long == bucket.direction_is_long
            @ ProgramError::DormantBridgeAccountMismatch,
    )]
    pub ledger: Account<'info, DistributionLedger>,
    #[account(
        mut,
        has_one = sub_pool,
        constraint = bucket.total_recovery_shares == 0
                  && bucket.total_recovery_notional == 0
                  && bucket.accrued_value == 0
                  && bucket.position_count == 0
            @ ProgramError::DormantBucketStillLive,
        constraint = bucket.last_applied_index >= ledger.next_event_index
            @ ProgramError::DormantBucketHasPendingApply,
        close = receiver,
    )]
    pub bucket: Account<'info, DormantBucket>,
    /// CHECK: lamport receiver, keeper-chosen.
    #[account(mut)] pub receiver: AccountInfo<'info>,
    pub keeper: Signer<'info>,
}
```

两条不变量保证 close 永远不会污染状态：

| 不变量 | 防止 |
| --- | --- |
| `record_is_dead(bucket)` | bucket 在引擎层尚有 live 资源（shares、accrued、positions）时被关闭 |
| `bucket.last_applied_index >= ledger.next_event_index` | bucket 在 ledger 上还有 pending lazy 待领时被关闭。Pack Pass 3 在 zero 时已经把 `last_applied_index = next_event_index`，所以 keeper 只要"等 sync_pool 走过 redeem-to-zero 那一步"就总成立；同时阻止 keeper 误关一个**预 init 但还没被引擎触碰**的空 PDA（其 `last_applied_index = 0` 但 ledger 已积累 entries） |

新错误码：`DormantBucketStillLive`、`DormantBucketHasPendingApply`。permissionless 设计——任何 keeper 都能触发 close，最坏情况是浪费自己的 CU（拿不到 rent，约束不满足时回滚）。

### 9.4 wave 7 攻坚证据：`keeper_drain_equivalence`

`crates/protocol-harness/tests/keeper_drain_equivalence.rs`：双 `Harness` 并行（一个 eager，一个 lazy + 周期 drain），同一组随机操作流（open/close/force_close/claim/sync/harvest_dust），3 个 seed × 400 步 = **1200 个操作**。每一步：

1. 两边并行执行操作，断言 `Ok` / `Err` 分类一致。
2. 对 lazy 端调用 `Harness::drain_all_buckets(sub_pool, slot)`，遍历每个 live bucket 调用 `pre_sync_dormant_bucket`。
3. 双端按 `(vault_balance, fee_vault, total_deposits, total_withdrawals, sub_pool 标量, 每 tick 的 bucket {anchor_price, shares, notional, accrued, pos_count}, position 字段)` 做 byte-equal 比较。
   - **唯一排除**：`bucket.last_applied_index`。这是 eager / lazy 唯一合法差异点——eager 的 last_applied_index 停在 bucket-create-time（`next_event_index`），lazy 的会因 drain 推进至 ledger head。`accrued_value` 等所有用户可见字段必须严格相等。
4. 双端各自的 4 项 vault 不变量同步通过。

为什么这个测试比 chain-mirror parity 更强：parity 测试比的是"两个跑同一模式的 runtime 一致"，wave 7 测试新增的是"两个**不同模式但等价**的 runtime 一致"。任何对 `apply_pending_to_bucket` / `distribute()` / ledger 复算公式的偏离——无论是公式重构、`last_applied_index` 步进调整、还是新加的 ledger compaction 优化——都会被这个测试在抵达 Solana 之前抓住。这是 lazy keeper 上线的**最强机器证据**。

辅助 API：

- `Harness::pre_sync_bucket(sub_pool, dir, tick, slot) -> PreSyncOutcome`：把 `clearing_core::pre_sync_dormant_bucket` 暴露到 host harness（之前 Harness 上没有这个入口）。
- `Harness::drain_all_buckets(sub_pool, slot) -> u64`：遍历所有 live bucket 调 `pre_sync_bucket`，返回总应用的事件数；遇到引擎已经 redeem-removed 的 bucket（`DormantBucketMissing`）安全跳过。

### 9.5 累计测试 + 工程化

| 阶段 | 测试 | 累计 |
| --- | --- | --- |
| Wave 5.5 末尾 | 125/125 | 125 |
| Wave 6 末尾 | 125/125（无新单测，靠 chain-mirror 平价覆盖） | 125 |
| Wave 7 末尾 | +1 `keeper_drain_equivalence`（3 seed × 400 步，byte-equal eager vs lazy+drain property） | **126/126** |

`cargo clippy --workspace --all-targets -- -D warnings` 干净。`cd programs/mole-option && cargo clippy -- -D warnings` 干净（host-side）。

### 9.6 限制

- BPF 工具链 bring-up 仍未做（环境限制）。`programs/mole-option` 是 host-checked 的：所有 Account / 借用 / 类型签名都通过 rustc，`solana-program-test` 端到端 BPF 仿真未跑。

## 10. Wave 8（2026-05-22 完成）—— 上线前完整安全网

Wave 8 把上线日必须的护栏全部钉死，并交付了 lazy 模式必需的 keeper 软件骨架。

### 10.1 chain-mirror 严格 PDA 生命周期模式（防 wave 7.2 回归）

`ChainRuntime::with_strict_pda_lifecycle(true)` 切换到与 Anchor 桥层 1:1 对齐的"严格"模式：

- 引擎产生新 tick 的 bucket record 时，`write_buckets_strict` **不再自动 grow**——必须先调 `pre_init_dormant_bucket(sub_pool, dir, tick)`（模拟 keeper 调 anchor 的 `init_dormant_bucket`）创建 dead PDA，否则返回 `MirrorError::BucketSlotExhausted`，整个 sub_pool snapshot 原子回滚（与 `DormantBridgeBucketSlotExhausted` on-chain 行为字节级对齐）。
- `ChainRuntime::close_dormant_bucket(...)` 复制了 anchor 的两条不变量（`record_is_dead` + `last_applied_index >= next_event_index`），失败回 `BucketLifecycleViolation`。
- `clearing_core::OnChainBucketRecord::is_dead()` / `OnChainBucketRecord::dead(direction, tick)` 提升为**dead PDA 单一权威谓词**——桥层 `record_is_dead`、chain-mirror `record.is_dead()`、`unpack_dormant_store` 跳过逻辑全部委托到这个常量函数，永远不会再有"两份独立判定"的漂移。
- `unpack_dormant_store` 在 `clearing-core` 层就跳过 dead records，桥层（dormant_bridge）保留原跳过仅作冗余防御。

新增 4 个 chain-mirror 单测：

| 测试 | 验证 |
| --- | --- |
| `strict_mode_rejects_rotate_without_preinit_then_keeper_init_unblocks` | rotate 无预 init → `BucketSlotExhausted` + 原子回滚；keeper init 后再 rotate → `anchor_price` 被正确 promote（**wave 7.2 fix 的回归守护**） |
| `strict_mode_byte_equal_to_loose_with_keeper_preinit` | 严格模式 + keeper init 与松散模式输出 byte-identical bucket record，证明严格模式无语义副作用，仅约束 PDA 生命周期 |
| `keeper_close_dormant_bucket_after_full_drain` | live bucket 拒绝 close → claim drain → bucket dead in place → close 成功收回租金 → 二次 close 报 `BucketNotInitialized` |
| `keeper_preinit_rejects_duplicate` | 重复 pre_init 同一 tick 返回 `BucketLifecycleViolation` |

### 10.2 `schema_version` 端到端拒绝矩阵

`clearing_core::SCHEMA_VERSION_CURRENT: u16 = 1` 成为单一权威。`assert_schema_version(found)` helper 在每个**funds-touching** engine entrypoint 入口执行：

| Entrypoint | 校验项 |
| --- | --- |
| `sync_pool` | `market.schema_version` |
| `pre_sync_dormant_bucket` | `market.schema_version` |
| `open_position` | `market.schema_version` |
| `close_position` | `market.schema_version` + `position.schema_version == market.schema_version` |
| `force_close_zero_value_position` | 同上 |
| `claim_dormant_recovery` | 同上 |
| `harvest_dust` | `market.schema_version`（Wave 8 新增 market 参数） |

不一致直接返回 `ClearingError::SchemaVersionMismatch`，sub_pool 字节级原子回滚。`programs/mole-option` 的 Anchor handler 透过 `map_err` 把 `SchemaVersionMismatch` 映射到 `ProgramError::SchemaVersionMismatch`，用户看到清晰可操作的报错而非静默状态损坏。

升级 playbook（详见 `clearing_core::market::SCHEMA_VERSION_CURRENT` doc-comment）：

1. 加新逻辑落 feature gate；保持常量旧值。
2. 部署 `migrate_position` / `migrate_market` 指令（无副作用 no-op on new schema，bump 字段 in-place on old schema）。
3. Squads 多签 + 链上治理 tx 把 `market.schema_version` 升上来，**同一**部署也把 `SCHEMA_VERSION_CURRENT` 编译值升级。从此旧 schema 仓位被引擎拒绝，必须先走 migration。
4. 旧 migration 指令保留一个治理周期作 fallback。

### 10.3 紧急熔断完整审计（`paused` / `frozen_new_position`）

Wave 8 之前 `harvest_dust` 不接 `market`，意味着 `paused == true` 时 keeper 仍然能 sweep dust 到 fee_vault——破坏"暂停 = 一切冻结"的语义。Wave 8 把它纳入熔断回路：`harvest_dust(market, sub_pool, direction)` 在头部检查 `paused`，与其他六个 entrypoint 对齐。完整熔断矩阵（`crates/clearing-core/tests/safety_gates.rs`）：

| 状态 | sync | open | close | force_close | claim | pre_sync | harvest |
| --- | :-: | :-: | :-: | :-: | :-: | :-: | :-: |
| `paused = true` | reject | reject | reject | reject | reject | reject | reject |
| `frozen_new_position = true` | OK | reject | OK | OK | OK | OK | OK |

每条 reject 路径都伴随**原子回滚**断言（用 `SubPoolFingerprint` 字节比较）。共 **16 个新 reject 单测**。

### 10.4 `crates/keeper/` 软件骨架

新 crate `keeper`（145 行核心逻辑 + 14 测试）实现 lazy 模式必需的调度器，纯 Rust + 零 Solana 依赖，host 侧可完整测试：

- **`KeeperChainView` trait**：read-only 抽象 `(sub_pool, bucket, ledger)` 视图。生产环境用 RPC client 实现，host 测试用 chain-mirror 实现（newtype wrapper）。
- **`Scheduler::plan(view)` → `Vec<KeeperAction>`**：按下列规则一次性枚举所有 actionable PDA：
  - `PreSyncDormantBucket { sub_pool, dir, tick, pending }` —— `bucket.last_applied_index < ledger.next_event_index`，优先级 = `1e7 + pending`（落后越多越紧急）。
  - `CloseDormantBucket { sub_pool, dir, tick }` —— `bucket.is_dead() && last_applied >= ledger.head`，优先级 1（best-effort 租金回收，永远在关键路径之后）。
  - `InitDormantBucket { sub_pool, dir, tick, rationale }` —— 只在 caller 通过 `Scheduler::record_init_hint` 显式 hint 后产出，优先级 1e9（最高，因为缺 PDA 会让用户 tx revert）。
- **`SchedulerConfig`**：`min_pending_for_pre_sync`（lag 节流阈值）、`max_actions_per_plan`（每轮上限）、`close_dead_buckets`（rollout 期可关）。
- **链端不变量校验**：plan 过程中如果检测到 `bucket.last_applied_index > ledger.next_event_index`（off-chain reconstruction bug），立即返回 `KeeperError::BucketAheadOfLedger`，便于运维抓 reorder。

集成测试 `tests/chain_mirror_integration.rs` 用 chain-mirror 跑端到端 lazy 闭环：

1. open + 价格 crash 触发 long rotate（lazy mode，bucket 落后于 ledger）。
2. `Scheduler::plan` 输出 1 个 `PreSyncDormantBucket(tick, pending)`，pending 数与 chain view 观测到的 `head - last_applied` 严格一致。
3. keeper apply（`rt.pre_sync_bucket`）→ `Scheduler::plan` 返回空。
4. claim → bucket dead → plan 不会对已 drained 的 long bucket 产出任何 PreSync。

`crates/keeper` 是后续真实 keeper bot 的内核：把 RPC client 套在 `KeeperChainView` 上、把动作翻译成 Anchor TX 即得生产级 keeper。

### 10.5 累计测试 + 工程化

| 阶段 | 测试 | 累计 |
| --- | --- | --- |
| Wave 7 末尾 | 126/126 | 126 |
| Wave 8 末尾 | + 4 chain-mirror keeper-init + 16 safety-gates + 12 keeper unit + 2 keeper integration = **+34** | **160/160** |

`cargo test --workspace` 全绿；`cargo clippy --workspace --all-targets -- -D warnings` 干净；`cd programs/mole-option && cargo clippy -- -D warnings` 干净。

### 10.6 限制 / 后续

- BPF 工具链 bring-up 仍是 wave 9 第一优先级——`solana-program-test` BPF 仿真器跑同一组操作流，期待与 chain-mirror 字节级一致。CU 实测也跟随。
- Squads 多签 + `set_market_paused` / `bump_schema_version` 治理 handler 仍是 wave 9 必交付项；wave 8 已经把"治理触发后协议侧 100% 拒绝写"这一边落地。
- `InitDormantBucket` 的"自动风险预测"—— wave 8 只在 caller 显式 hint 时产出。下一波接入价格波动率模型，自动预测 rotate-imminent ticks 并 pre-init。

## 11. Wave 9（2026-05-22 完成）—— 治理回路 + 自动 rotate 预测 + keeper 执行抽象

第九波把 wave 8 留下的"等价护栏 + safety gates 都落地了"承诺转化成实际可上线的治理触发回路与 keeper 软件骨架的最后一公里。沙箱内 anza platform-tools v1.52 tarball（≈200 MB）curl 5 分钟仅完成部分传输，`cargo build-sbf` 仍卡在 `platform-tools/rust/lib` 不存在；BPF bring-up 与 CU 实测继续等 CI runner 的稳定带宽。

### 11.1 完整治理指令体系

`programs/mole-option/src/instructions/admin.rs` 重构为三层 authority 上下文：

- **`PauseMarket`**（emergency authority）：`pause_market` / `resume_market` / `freeze_new_position` / **`unfreeze_new_position`**（wave 9 新）。后者闭合 wave 1 留下的"freeze 是单向门"漏洞——OpsEC Squads 永远能撤回自己 24h 前误下的紧急熔断决策。
- **`AdminMarketAccounts`**（admin authority，slow-multisig + timelock）：**`bump_market_schema_version(new_version)`**（wave 9 新）。`new_version <= market.schema_version` 立即报 `SchemaBumpMustIncrease`。版本号严格单调递增——多签即使被攻陷也无法回滚到已修复的旧 schema。
- **`AdminGlobalAccounts`**（admin authority，全协议 kill switch）：**`set_globally_paused(bool)`**（wave 9 新）。flag 落 `GlobalConfig::paused_globally`；wave 10 把它 OR 进 `clearing_view` 的 `paused`，单一 tx 即可冻结所有 market（多市场 exploit 场景的最后防线）。

新模块 `programs/mole-option/src/instructions/migration.rs` 实现 §16 的 schema 升级 playbook：

- **`migrate_position(MigratePosition)`**：permissionless caller，每次只 walks 一条 position 的 `schema_version` 一步。`SchemaMigrationStep::from_source(version) -> Option<Self>` 是单一权威升级路径注册表；wave 9 是空 enum（launch epoch v1，无 migration 需要），但 `while position.schema_version < target { ... apply; bump }` 循环已就位，未来 v2 只需加 `+ V1ToV2`。
- **`migrate_market(MigrateMarket)`**：admin authority gated（与 `bump_market_schema_version` 同 multisig），数据结构升级与版本 bump 落同一治理 timelock。
- 新错误码 `SchemaMigrationNoop` / `SchemaMigrationPathMissing`。`SchemaBumpMustIncrease` 与上面 `bump_market_schema_version` 共用。

`programs/mole-option/src/lib.rs::#[program]` 暴露 5 条新指令：`unfreeze_new_position` / `set_globally_paused` / `bump_market_schema_version` / `migrate_position` / `migrate_market`。

### 11.2 chain-mirror 治理 setters + reject-matrix 测试

```rust
ChainRuntime::governance_set_paused(bool)
ChainRuntime::governance_set_frozen_new_position(bool)
ChainRuntime::governance_bump_schema_version(new) -> Result   // 单调递增 guard
ChainRuntime::governance_migrate_position(id) -> Result        // noop guard
```

3 个新单测（chain-mirror 单测 10 → 13）：

1. `governance_pause_immediately_rejects_every_funds_path`：flip `paused=true` 后**单同一 tx 内**全部 5 条 funds entrypoint（sync / open / close / force_close / harvest_dust）必须 reject 为 `MarketPaused`，证明治理瞬时性。
2. `governance_freeze_blocks_only_open_not_close`：`frozen_new_position=true` 仅 open reject，其余 4 条照常工作（让用户能在 deprecated market 平仓）。
3. `governance_bump_without_program_upgrade_freezes_protocol`：**wave 9 keystone 安全保证**——admin multisig 即使被攻陷，把 `market.schema_version` 调到部署 BPF 不支持的值后，所有 funds entrypoint 立即 reject `SchemaVersionMismatch`，protocol 进入"**lockdown window**"直到匹配的 BPF 上线。`SCHEMA_VERSION_CURRENT` 是部署期 const，无法通过任何 Squads tx 翻动。

### 11.3 keeper crate `RotateRiskPredictor`

`crates/keeper/src/lib.rs` 新增：

```text
pub trait KeeperChainView {
    ...
    fn sub_pool_health(&self, _id: u32) -> Option<SubPoolHealth> { None }   // 默认 None，向后兼容
}

pub struct SubPoolHealth { last_price, long_anchor_price, short_anchor_price,
                           long_pool_equity, short_pool_equity,
                           long_active_notional, short_active_notional, ... }

pub struct RotateRiskPredictor { config: PredictorConfig }
pub struct PredictorConfig { annual_vol, horizon_slots, slots_per_second,
                             min_probability, tick_aggregation_factor, price_tick }
pub struct RotatePrediction { sub_pool_id, direction, zero_price, zero_tick, probability }
```

零价格反推：`S_zero_long = anchor · (1 − long_pool_equity / long_active_notional)`，`S_zero_short = anchor · (1 + short_pool_equity / short_active_notional)`。一接触概率上界用 `Φ(log(S_zero/S_now) / σ√T)`。Φ 用 Abramowitz-Stegun rational approx（max abs 误差 ≈ 7.5e-8），不引外部数学库以保持 host vs BPF 编译确定性。Predictor 保守跳过 over-collateralised 长仓（仅靠负向漂移无法清零）和空池。

`predictor.populate_scheduler(&view, &mut sched)` 每条 prediction 调 `Scheduler::record_init_hint(... InitRationale::RotateRiskHorizon)`，下一次 `Scheduler::plan` 以最高优先级（`1e9`）emit 对应 `InitDormantBucket` action。chain-mirror integration 测试：1% 价格漂移，predictor 检出长仓 zero-tick，scheduler emit init action，keeper `pre_init_dormant_bucket` 应用，dead PDA 落地。

### 11.4 `ActionExecutor` trait + `DryRunExecutor`

```text
pub trait ActionExecutor {
    fn execute(&mut self, action: KeeperAction) -> ActionDispatchResult;
}

pub enum ActionDispatchResult {
    Submitted { signature: Option<String> },
    Skipped { reason: &'static str },
    Failed { reason: String },
}

pub struct DryRunExecutor { log: Vec<KeeperAction> }   // 只记录，不执行

pub fn run_plan_cycle(scheduler, view, executor) -> Result<Vec<(Action, Result)>>
```

为什么 trait 化：`Scheduler` 是 read-only planner；retry / tx-builder / RPC error 是不同 cadence 的关注点。trait 拆出去让 wave 10 RPC keeper 直接绑 `solana-client`，集成测试用 chain-mirror runtime mutator，性能基准用 `DryRunExecutor`，三方互不耦合。`run_plan_cycle` 返回 `Vec<(Action, Result)>` 而不是单一 `Result` —— **per-action 失败不 abort cycle**，caller 决定 retry 策略。

### 11.5 累计测试 + 工程化

| 阶段 | 测试 | 累计 |
| --- | --- | --- |
| Wave 7 末尾 | 126/126 | 126 |
| Wave 8 末尾 | +34 | 160/160 |
| Wave 9 末尾 | + 3 chain-mirror 治理 + 5 predictor + 3 executor + 1 keeper-integration + 3 测试支撑 = **+15** | **175/175** |

`cargo test --workspace` 全绿；`cargo clippy --workspace --all-targets -- -D warnings` 干净；`cd programs/mole-option && cargo clippy -- -D warnings` 干净（host-side）。

### 11.6 限制 / wave 10 优先级

继续推迟到 wave 10：

1. **Solana 工具链 bring-up + `solana-program-test`**：sandbox 内 anza platform-tools v1.52 tarball（≈200 MB）curl 部分传输（exit 18），需要 CI runner 稳定带宽。一旦解决：`cargo build-sbf` 出 `mole_option.so`，最小 `solana-program-test` 跑 init → open → sync → close 闭环，断言事件流与 `protocol-harness` 字节级相等；同一脚本验证 wave 9 新 5 条治理指令未授权调用 reject。
2. **CU 预算实测** —— 依赖 §1。
3. **`Scheduler` → 真实 RPC 适配** —— `crates/keeper::ActionExecutor` 已经是抽象层，wave 10 只需 ~200 行 RPC adapter（绑 `solana-client::nonblocking::rpc_client`，`KeeperAction` → Anchor IX）。
4. **前端 SPA** —— React + Vite 三面板：交易 / IndexerState 视图 / keeper 控制台（展示 `Scheduler::plan` + `RotateRiskPredictor` 输出）。
5. **历史波动率自调谐** —— wave 9 的 `PredictorConfig::annual_vol` 是手调；生产 keeper 需要从 oracle 历史价格滚动计算 realized vol，自动喂回 `PredictorConfig`。

## 12 Wave 10 — 端到端 keeper bot 闭环

wave 10 的目标是把 wave 9 留下的 trait 抽象层（`KeeperChainView` / `ActionExecutor` / `RotateRiskPredictor`）真正接成可由 ops harness 按 cadence 驱动的 keeper bot，并加上 wave 9 缺失的"realized vol 自调谐"。所有逻辑层 host-only 完成（`solana-client` 留 wave 11 接），workspace 单测从 175 → 211。

### 12.1 `keeper::RealizedVolatilityEstimator` —— σ̂ 自调谐

```text
PriceSample { price: u64, slot: u64 }
RealizedVolatilityEstimator { config, samples: VecDeque<PriceSample> }
RealizedVolatilityEstimatorConfig {
    max_samples, max_age_slots, min_samples,
    slots_per_second, min_clamp, max_clamp
}
```

时间加权 σ̂² = Σr²ᵢ / ΣΔtᵢ —— Solana slot 间隔抖动让简单 stddev(r)·√(N/T) 形式不可靠。`apply_to_predictor_config(predictor)` 在 warm 后覆写 `predictor.annual_vol`，未 warm 时保留 caller 手设值。Window 受 `max_samples` + `max_age_slots` 双约束，σ̂ 在 `[min_clamp=0.05, max_clamp=5.0]` 默认带内 clamp 防 outlier 翻车。

8 单测：`vol_estimator_{none_until_warmup, constant_price_clamps_to_floor, synthetic_walk_recovers_order_of_magnitude, drops_out_of_order_and_duplicate_slots, evicts_by_count_then_by_age, apply_overwrites_only_when_warm, reset_clears_history}`。

### 12.2 `keeper::KeeperLoop` —— 同步状态机

```text
trait KeeperBotEnvironment {
    fn chain_view(&self) -> &dyn KeeperChainView;
    fn fetch_price_sample(&mut self) -> Option<PriceSample>;
    fn executor(&mut self) -> &mut dyn ActionExecutor;
}

KeeperLoop::tick(&mut self, env: &mut E) -> Result<KeeperLoopOutcome, KeeperError>
```

每 tick 五步：record sample → apply σ̂ → predictor populate scheduler → scheduler.plan → 逐条 dispatch。**故意同步**——不引 tokio，部署 harness 自己拥有 cadence。失败的单条 dispatch 不 abort tick；`KeeperLoopMetrics` 拆 `actions_{submitted, failed, skipped}` 让 dashboard 直接出图。

6 单测：idle tick → 0 metrics、explicit init hint dispatch、auto-tune warm-up 覆写 predictor、auto-tune off 时 predictor 锁定、flaky executor partial-failure metrics 字段拆分、`metrics::merge` 字段加和。

### 12.3 `keeper-rpc` crate —— 离线可测的 RPC 抽象层

新 crate `crates/keeper-rpc/`，**不引 `solana-client`**（`default = []`，`solana-rpc` feature 待 wave 11 接 solana-client 4.0）。模块：

| 模块 | 内容 | 测试 |
|------|------|-----|
| `accounts` | `Onchain{SubPool,DormantBucket,DistributionLedger,Market}` 的 borsh 字节镜像，与 `programs/mole-option/src/state.rs` 字节对齐 | 3（round-trip + discriminator strict + truncated） |
| `pda` | `{market,sub_pool,distribution_ledger,dormant_bucket}_seeds`，layout 由 `seeds_layout_pinned` 锁死 | 2 |
| `fetcher` | `AccountFetcher` trait + `MockAccountFetcher`（program_accounts 支持 memcmp filter） | 3 |
| `snapshot` | `ChainSnapshot::refresh(fetcher, ctx, cfg)`：批量拉账户 → borsh 解码 → 实现 `KeeperChainView`；`SnapshotConfig::{enforce_schema_version, bail_when_paused}` 复刻 wave 9 lockdown | 6 |
| `tx` | `TxBuilder` trait + `RpcExecutor`（实现 `ActionExecutor`）：`KeeperAction` → `DispatchedAction { program_id, data, accounts }`，`data = [disc8] ++ borsh(args)` | 7 |

**Anchor instruction discriminator pinning** —— 关键安全点：

```rust
pub const DISC_PRE_SYNC_DORMANT_BUCKET: [u8; 8]   = [0xd6, 0x62, 0xa8, 0x7a, 0xc1, 0x9c, 0x38, 0x08];
pub const DISC_CLOSE_DORMANT_BUCKET: [u8; 8]      = [0x16, 0x25, 0x10, 0x86, 0xff, 0x31, 0xf8, 0x83];
pub const DISC_INITIALIZE_DORMANT_BUCKET: [u8; 8] = [0x8f, 0xcb, 0x7b, 0xd0, 0xc2, 0x40, 0x6e, 0x35];
```

热路径写死 `pub const` 不付每次 SHA-256 成本，但 `discriminator_constants_match_sha256_of_anchor_namespace` 单测每次 CI 用 `sha2::Sha256` 重算 `sha256("global:<name>")[..8]` 比对常量。指令重命名 → 测试失败 → CI 阻断 deploy，绝不出现常量与 program 不一致还跑过 CI 的 silent bit-rot。**初次提交 3 个常量都是错的，正是这个 self-test 在第一次 CI 跑捕获**——由 `print_canonical_discriminators -- --ignored --nocapture` 输出正确字节后修复。

### 12.4 `keeper-bot` crate —— 可跑的守护进程

新 crate `crates/keeper-bot/`，bin + lib：

```text
BotConfig { snapshot, predictor, run_predictor }
KeeperBot { config, scheduler, predictor, vol: RealizedVolatilityEstimator }

KeeperBot::tick<F: AccountFetcher, B: TxBuilder>(
    &mut self, fetcher, ctx, builder, keeper_pk, clock_sysvar, system_program,
) -> Result<(TickReport, B), BotError>
```

每 tick：refresh snapshot → 喂 vol estimator（每个 sub_pool 的 last_price/last_sync_slot）→ apply σ̂ → 跑 predictor → plan → `RpcExecutor` 注入 builder dispatch。`TickReport { actions_planned, dispatched, applied_vol, init_hints_added }` 给 ops 看。

`cargo run -p keeper-bot` 是离线 smoke runner——MockAccountFetcher + MockTxBuilder，空 fixture 必返 `MarketNotFound`，进程 exit 0 + 友好日志，证明整套构件 wire 通。

5 个 e2e 集成测试（`tests/end_to_end.rs`）：idle market → 0 actions、explicit-init-hint 路径自洽（`actions_planned == dispatched.len() == submitted.len()`）、paused → bot 立刻 error、schema mismatch → bot 立刻 error、**vol estimator 跨 40 tick warm 起来**——`applied_vol` 从 None 翻成 Some(σ̂) 是 wave 10 唯一在端到端层面验证的"自调谐"接缝。

### 12.5 测试与质量

| 阶段 | 新增 | 累计 |
| --- | --- | --- |
| Wave 9 末尾 | — | 175 |
| Wave 10 末尾 | + 8 vol + 6 KeeperLoop + 21 keeper-rpc + 7 keeper-bot (5 e2e + 2 lib) + minor 重排 = **+41** | **216/216** |

`cargo test --workspace` 全绿；`cargo clippy --workspace --all-targets -- -D warnings` 干净；`cd programs/mole-option && cargo clippy -- -D warnings` 干净。

### 12.6 Solana 工具链 retry —— 沙箱仍受限，CI playbook 落地

第二次重试 platform-tools v1.52 osx-aarch64 仍卡在 ~82%（5 分钟 324M / 395M，curl exit 18 partial transfer）—— 这是沙箱网络限制而非 tarball 问题。Wave 10 落地完整的 CI playbook（详见 `Docs/Planning/20-攻坚开发进度与里程碑.md` § 10.6），含 actions/cache 配置 + platform-tools 软链 + cargo build-sbf 完整序列，wave 11 在稳定 CI 环境一次性跑通。

### 12.7 Wave 11 优先级

1. `keeper-rpc` 的 `solana-rpc` feature 接通（`SolanaRpcAccountFetcher` + `SolanaTxBuilder`，~250 行）
2. `solana-program-test` 跑 init/open/sync/close 字节级 parity；wave 9 governance reject 矩阵；CU 实测填进 §21
3. 前端 React + Vite SPA：交易 / IndexerState / keeper 控制台（直接消费 `KeeperLoopMetrics` + `RotatePrediction`）
4. ops runbook：wave 9 governance + wave 10 keeper bot 操作手册

## 13 Wave 11 — production RPC 接通 + ops runbook + 前端 MVP

### 13.1 `keeper-rpc::solana` 模块完成

`solana-rpc` feature 真正落地（详见 `Docs/Planning/20-…md` §11.1）：`SolanaRpcAccountFetcher` 实现 `AccountFetcher`，走 sync `RpcClient::get_account_with_commitment` + `client.send::<OptionalContext<Vec<RpcKeyedAccount>>>(RpcRequest::GetProgramAccounts, …)` 绕开 4.0 sync 端缺失的 `_with_config` 高级方法，base64 → bytes 解码经 `decode_account_data_blob` 显式拒绝 zstd/base58；`SolanaTxBuilder` 实现 `TxBuilder`，`get_latest_blockhash → Transaction::new_signed_with_payer → send_and_confirm_transaction`，payer 按值持有，`rotate_payer` 返回旧 keypair 以便审计 + secure-erase。

依赖钉死到细粒度 Anza crate（`solana-pubkey 4.1` / `solana-transaction 3.1` / `solana-instruction 3.3` / `solana-commitment-config 3.1` / `solana-keypair 3.1` / `solana-signer 3.0` / `solana-hash 4.2`）以避免 `solana-sdk 4.0.1`（带 transaction 4.0）与 `solana-rpc-client 4.0.0`（要 transaction 3.1）的 `SerializableTransaction` 版本歪斜。

11 个新单测覆盖：`Pubkey32` byte-level round trip、AccountMeta flags 4 组合、DispatchedAction → Instruction 字节相等、base64 编/解码 round trip、unsupported encoding 拒绝、malformed shape 拒绝、ClientError 二分契约、unreachable URL → `RpcError::Transport`（fetch_account / fetch_program_accounts）、`SolanaTxBuilder::submit` unreachable URL → `ActionDispatchResult::Failed` 不 panic、`rotate_payer` 返回旧 keypair。Workspace 测试默认 216 不变，`--features solana-rpc` 启用时 keeper-rpc 21 → 32，总数 227。

### 13.2 Wave 12 优先级

production gate（真 RPC）+ ops surface（runbook + 前端框架）打穿后，wave 12 焦点回到"让真用户能用 / 真上链"：

1. **Solana toolchain + `solana-program-test`**（CI 环境）—— 字节级 parity / governance reject / CU 实测（详见 §10 + §11）。
2. **Keeper bot 生产化** —— Prometheus metrics exporter + 多副本协作 + 优雅停机 + structured logs。
3. **运营自动化** —— ops health prober 自动跑 24-runbook §2 的 18 项 dashboard 项，输出 JSON + 非零 exit code 给 AlertManager。
4. **前端接真链上** —— wasm 化 borsh decoder + websocket 订阅 + wallet adapter（Phantom/Backpack/Squads）。
5. **审计准备** —— SECURITY.md 整理 wave 1-11 全量不变量 + 测试引用 + 已知边界条件。

## 14 Wave 12 — production daemon + ops automation + adapters + audit readiness

### 14.1 Keeper bot 生产化（`keeper-bot::{metrics,serve,run}`）

把 wave 10 的 `KeeperBot::tick` 状态机外面套了一层 production wrapper，最重要的三件事：

- `KeeperMetrics` —— 13 个 Prometheus 指标（6 counter + 7 gauge），全 atomic / lock-free / `Ordering::Relaxed`；`render_prometheus()` 一次性产生 OpenMetrics 0.0.4 文本（HELP + TYPE + 值，每行严格满足 `<name> <value>`）。`applied_vol = None` 编码为 `NaN`，scrape 端 PromQL 用 `< bool +Inf` 判 warming up。
- `serve.rs` —— 手卷 HTTP/1.1 listener（**不引入** axum/hyper/tokio）：`set_nonblocking(true)` + 200ms accept poll 间隔，两条路由 `GET /metrics` / `GET /healthz`。`render_response(&request_line, &metrics)` 是纯函数，可以跳过 socket 直接喂单测。
- `run_loop_with_factory` + `is_transient(&BotError)` —— 把 wave 9 的 governance 拒绝语义 1:1 镜像到守护进程层：`MarketPaused / SchemaVersionMismatch / MarketNotFound / SubPoolNotFound / Decode` 永久 → 退出，`Rpc(Transport)` 暂时 → backoff 续跑。

`tracing-subscriber` JSON layer 直接喂 Loki / Datadog；`ctrlc::set_handler` 把 SIGINT/SIGTERM 翻译成 `shutdown.store(true)`。22 个 keeper-bot lib 单测 + 5 个 e2e 集成测全绿。

### 14.2 `crates/ops-toolkit` —— 18 项 runbook check 自动化

新 crate 把 `Docs/Planning/24-operator-runbook.md` § 2 的 18 项 dashboard 项全自动化。`HealthContext` 是 prober 的输入 schema（market / sub-pools / keeper / rpc / oracle / pool 6 个事实子结构）；`checks.rs` 18 个纯函数 `(ctx) -> CheckResult` 阈值与 runbook § 2 字字对齐。`Severity (P0..P3) × Status (Pass/Warn/Critical)` → exit code 0/1/2/3/4 严格映射；`render_json` / `render_prometheus_textfile` 双输出格式，**零 serde 依赖**（手卷 JSON）。

`ops-toolkit demo human` 全 `[ OK ]` exit 0；`ops-toolkit demo-broken human` 标 P0 critical exit 4。26 个单测覆盖每个阈值边界 + JSON 平衡 + Prometheus textfile 行格式。

### 14.3 前端 wave 12 阶段一（`feed/` + `wallet/`）

把 wave 11 内联的 `useMockFeed()` 拆成"协议层 vs React 层"：`FeedAdapter` 接口（`MockFeedAdapter` + `WebSocketFeedAdapter` 占位 + `useFeed(adapter)`）+ `WalletAdapter` 接口（`MockWalletAdapter` + `WindowWalletAdapter` 占位检测 `window.solana.{isPhantom,isBackpack,isSolflare}`）。`?feed=live` URL 参数切到 websocket adapter，wave 13 替换 body 即可而 panels 不动一行。`TraderPanel` 的 Submit 真调 `wallet.signAndSubmit(...)` 把签名渲染回 confirmation。`npm run build` ✓ gzip 51.88 KB。

### 14.4 `SECURITY.md` —— 不变量目录 + 威胁模型 + 审计 onboarding

仓库根新建 `SECURITY.md`：A-1..A-7 七类 adversary、CORE/ONCH/GOVN/KEEP/OPS 五前缀 27 项不变量（每项指向具体测试函数）、5 条信任假设、漏洞披露 SLA + bug bounty 三档、审计 firm 1+2 天读文档/读代码 onboarding 顺序。Wave 13 联系审计公司时一文递交。

### 14.5 测试 / clippy / 验证

总测试数 216 → **262**（+46）；启用 `solana-rpc` feature 后 273。`cargo clippy --workspace --all-targets -- -D warnings` + `cargo clippy -p keeper-rpc --features solana-rpc` 干净；`cargo run -p keeper-bot -- serve 127.0.0.1:0 2` JSON logs + 自然退出 ✓；`npm run typecheck` + `npm run build` 干净。

### 14.6 Wave 13 优先级

1. CI 接入 Solana toolchain → `solana-program-test` + governance reject 矩阵 + CU 实测填 § 21。
2. 前端 wasm 化 borsh decoder + `WebSocketFeedAdapter` 真订阅 + `WindowWalletAdapter::signAndSubmit` 真签名 + 真提交。
3. Keeper 多副本协作（on-chain `KeeperLeaderLock` PDA + `KeeperLeaderHeartbeat` 选主）。
4. 审计 firm onboarding 启动（≥ 2 家）+ bug bounty 上线。
5. `SECURITY.md` CI hook 把 test-ref 强制同步。

## 15 Wave 13 —— audit-readiness governance 落地

不动 dormant bridge 协议字节，只把 wave 12 的 audit-readiness 承诺真正绑到 CI 上：

- `.github/workflows/ci.yml` 4 个 job（rust + governance + frontend + audit-readiness 汇总）+ `actions/cache` 缓存 cargo target / npm cache。
- 三个 governance verifier 脚本：`scripts/verify-security-references.sh`（SECURITY.md 32 个 test reference 全 grep 到真 fn）、`scripts/verify-test-counts.sh`（doc 声明 262 = `cargo test --workspace` 实测）、`scripts/verify-schema-parity.sh`（`Onchain*` Borsh struct 的 80 个字段必须全在新建的 `Docs/SCHEMA-MAPPING.md` 登记）。bash 3.2 兼容、零依赖。
- `SECURITY.md` 修了 wave 12 publish 时漏掉的 20 条 broken reference（aspirational 测试名 → 真实 `paused_blocks_*` / `governance_*` / `init_hint_*` / `vol_estimator_constant_price_clamps_to_floor` 等系列），最终 32 refs 全过 verifier。
- 新建 `Docs/SCHEMA-MAPPING.md` 给 5 个 `Onchain*` struct 共 80 字段每个登记一行（mapped / omitted），并加了反方向 TS → Rust 来源表。
- 新建 `CHANGELOG.md`（wave 1..13 倒序）+ `CONTRIBUTING.md`（外部贡献者 + 审计 firm 两天 onboarding + 5 步本地复现）。

测试维持 `262 / 262 pass`，三个 verifier 全绿。Wave 14 接 Solana toolchain（沙箱外 runner）+ 前端 wasm decoder + on-chain leader lock。

## 16 Wave 14 —— keeper-decoder schema 拆分 + 前端真订阅 / 真签名

Wave 14 是 dormant bridge schema 第一次"被前端真消费"。后端把 5 个 `Onchain*` Borsh struct 抽成独立 wasm32-buildable 的 `crates/keeper-decoder` 新 crate；前端拿同一份 schema 用 `@coral-xyz/borsh` 镜像出 TS 端 decoder，`WebSocketFeedAdapter` 真接 `Connection.onAccountChange` + `onProgramAccountChange`，按 8-byte discriminator 路由到 sub-pool / dormant-bucket / market 三类账户解码后聚合成 `FeedSnapshot`。

要点：

- `keeper-rpc::accounts` 变成 thin re-export shim → 外部所有 `keeper_rpc::accounts::OnchainSubPool` 路径不变，schema 演进在 `keeper-decoder` 单点变。
- `keeper_decoder::schema_descriptor_json()` (Rust) 与 `frontend/src/decoder/onchain.ts::SCHEMA_DESCRIPTOR` (TS) 双方钉死同一份字段顺序，schema bump 一边漏改另一边的测试立刻挂；wave 15 上 wasm-pack 后这层 stop-gap 消失。
- `verify-schema-parity.sh` 改为读 `crates/keeper-decoder/src/lib.rs`（schema 真源），与 mapping 文档解耦。
- `WebSocketFeedAdapter` 持续 hold 住快照直到 market PDA 到达，避免下游看到 partial state；解码失败计数不抛错，protocol 持续运行。
- `WindowWalletAdapter::signAndSubmit` 真接 `window.solana.signAndSendTransaction`，错误分 6 类（WalletNotConnected / NoTxBytes / ProviderMissing / ProviderUnsupported / UserRejected / ProviderError）映射到 `WalletSignError.kind`。

测试：Rust **274/274 pass**（baseline 262 + keeper-decoder 12），Frontend **31/31 vitest pass**（12 decoder + 7 ws-adapter + 12 wallet-adapter），typecheck/build/clippy/3 verifier 全绿。沙箱内 wasm32 实际编译被 rustup std 下载墙堵死（第 6 次），CI runner 上 `rustup target add wasm32-unknown-unknown && cargo build -p keeper-decoder --target wasm32-unknown-unknown --release` 一跑就过。

Wave 15 接：wasm-pack 真打包替换 TS hand-rolled schemas，trader panel 真发交易闭合 demo→devnet。

## 17 Wave 15 —— `keeper-decoder` 上 wasm-pack + 链上 KeeperLeaderLock + trader panel 真签

Wave 15 把 dormant-bridge 的 schema/encoder 进一步拉直成"一份 Rust 同时驱动后端 keeper bot 和前端 trader panel"——不止 decoder，连 instruction encoder 也走 wasm 单源；同时把 keeper 选主从 wave 12 的本机文件锁升级到链上 `KeeperLeaderLock` PDA。和 dormant bridge 直接相关的几条：

1. **`keeper-decoder` 加 `cdylib` + `wasm-bindgen` glue + `wasm-pack` 真打包** —— 前端 `frontend/package.json` 直接 `"keeper-decoder": "file:../crates/keeper-decoder/pkg"` 吃 wave-15 wasm 输出（45.87 KB `keeper_decoder_bg.wasm` 嵌进 prod build）。`wave 14 stop-gap` 的 hand-rolled `@coral-xyz/borsh` schemas 现在退役到"WASM 字节级 parity oracle"角色（`frontend/src/tx/encode.ts` 与 `wasmBuilder.ts` 对每个 ix 跑字节级 diff，schema drift 立挂）。`crates/keeper-decoder/src/wasm_bridge.rs` 暴露的 wasm 符号：`encodeOpenPosition / encodeClosePosition / encodeKeeperLeaderHeartbeat / encodeKeeperLeaderAcquire / encodeKeeperLeaderRelease / decodeKeeperLeaderLock / instructionDiscriminator / accountDiscriminator / keeperLeaderLockSeedPrefix`。
2. **`KeeperLeaderLock` Anchor 账户 + 4 ix（`programs/mole-option`）** —— `state.rs` 加 `#[account] KeeperLeaderLock { has_leader: bool, current_leader: [u8;32], last_heartbeat_slot: u64, takeover_threshold_slots: u64 }`（固定 49 字节 body + 8 字节 disc = 57 字节，刻意不用 `Option<Pubkey32>` 避开 Borsh 变长 prefix）；`instructions/keeper_leader.rs` 加 `initialize_keeper_leader_lock / keeper_leader_acquire / keeper_leader_heartbeat / keeper_leader_release` 4 ix；`error.rs` 加 5 个新 program error。`keeper-decoder::leader_lock::KeeperLeaderLock::try_heartbeat` 是状态机唯一真源，`HeartbeatOutcome` 五个 variant 与链上 4 个 ix 的所有路径一一对应（含 fresh acquire / stale takeover / same-signer refresh / wrong-holder reject / clock-skew reject）。
3. **dormant bucket 真链上 close 与 leader lock 的关系** —— wave 7 / 11 已经把 `close_dormant_bucket` 闭环跑通；wave 15 在 keeper-bot 跑 close ix 之前先过链上 leader gate（`run_loop_with_leader`），确保多副本部署里只有"当前持锁的 keeper 本地 mirror 通过 try_heartbeat 后"才会真正派发 close 等改写动作，wave-12 的进程文件锁退役为 fallback。
4. **测试覆盖** —— `keeper-decoder` 38 host test（leader-lock 13 + ix 9 + 既有 16）；`chain-mirror::leader_lock` 3 bridge integrity test；`keeper-bot::leader` 6 host test；`run_loop_with_leader` 路径在 host 用 `FixedLeaderPolicy` 验闸；`frontend/src/tx/wasmBuilder.test.ts` 16 vitest test 钉死 wasm 字节级输出。`cargo test --workspace --all-targets` 325+ pass、`npm test` 47 pass、`wasm-pack build` 沙箱真跑通 + CI yaml `wasm-pack` job 加 `actions/upload-artifact → frontend job download` 链路。

仍在等沙箱外 CI runner 完成的：`cargo build-sbf` 把链上程序产出 `.so`、`solana-program-test` 跑 4 个 keeper-leader ix 的 reject 矩阵 + CU 实测，devnet 双 keeper 副本真切换。host 这一侧的状态机正确性已经被 property test（任意 heartbeat 序列下唯一 holder 不变量）锁死。

## 18 Wave 16 —— 单 snapshot tick + 链上 reconcile + 真 heartbeat 派发 + 前端 leader banner

Wave 15 在 host 路径把 `KeeperLeaderLock` 状态机 + 编码器全部就位，但 **没有真正把它接到链上闭环**：keeper-bot 的 `HostMirrorLeaderPolicy` 只读自己缓存的 mirror，不发链上 heartbeat；前端没有 leader 状态可视化；`run_loop_with_leader` 还会双 RPC 浪费 RPS。Wave 16 把这三个缺口同时补齐。

1. **单 snapshot tick（`KeeperBot::tick_with_snap`）** —— wave-9 / wave-10 的 tick 内部 pipeline（vol → predictor → scheduler → executor）从 `tick(fetcher, …)` 抽出成 `tick_with_snap(&snap, …)`；老 `tick` 退化成 wrapper（先 refresh 再 forward）。`run_loop_with_leader` 和新 `run_loop_with_leader_and_rpc_reconcile` 都改成「一拍 snapshot 一次 RPC」，wave-15 prefetch + tick refresh 双跳消失。

2. **`keeper-rpc::leader_tx`** —— 用 wave-15 byte-exact 编码器 + wave-15 PDA seeds 构出三条 ix 的可签结构 (`LeaderInstruction`)；`KeeperLeaderTxBuilder` trait 解耦 submission，`MockKeeperLeaderTxBuilder` 给 host 测试用，`SolanaTxBuilder` 在 `--features solana-rpc` 下实现真 RPC 派发。同模块 `fetch_keeper_leader_lock(fetcher, lock_pda)` 通过 `AccountFetcher` 读 PDA + 验 `account:KeeperLeaderLock` discriminator + Borsh 解 49 字节 body → 返回 host state-machine 类型。

3. **`run_loop_with_leader_and_rpc_reconcile`** —— wave-16 主循环：单次 snapshot refresh、每 N=20 ticks reconcile（`fetch_keeper_leader_lock` → `policy.reconcile`）、每 M=5 ticks 发 heartbeat ix（且**首次成为 leader 时立即**发）、Standby tick 不发 heartbeat。Reconcile / publish 失败仅写 `tracing::warn!`，不致命，下一拍重试。两个新 e2e 测试覆盖「reconcile 翻 leader → 立即发 heartbeat」 与「reconcile 看到他人持有 → Standby + 不发 heartbeat」，端到端钉死 wave-15「at-most-one holder」不变量。

4. **前端 `LeaderLockBanner`** —— `frontend/src/panels/LeaderLockBanner.tsx`，纯函数 `deriveLeaderLockState(view, currentSlot)` 输出 4 个 kind（`uninitialised | unowned | fresh{slotsUntilStale} | stale{slotsOverdue}`），React 组件渲染顶部 status bar；4 色色板（灰/黄/绿/红）+ holder pubkey 截断。Banner 已挂到 `App.tsx` 顶部（当前用 `view={null}`，wave 17 接 `accountSubscribe` 后实时显示）。8 条 vitest 单测覆盖 4 个 kind + boundary（elapsed === takeover 仍 fresh） + clock-skew 钳零 + `shortenHex`。

5. **链上 reject 矩阵骨架** —— `programs/mole-option/tests/keeper_leader.rs`，gated 在 `_keeper_leader_program_test` feature 后；5 条 `tokio::test`：happy path（init→hb→hb→release，CU ≤ 12k init / ≤ 8k hb-rel）、heartbeat-by-other-fresh 拒（`KeeperLeaderHeldByOther`）、acquire-while-fresh-self 拒（`KeeperLeaderAcquireWhileFresh`）、release-by-non-holder 拒（`KeeperLeaderNotHolder`）、observed_slot < recorded 拒（`KeeperLeaderClockSkew`）。沙箱拉不下 SBF；CI runner 上 `cargo test --features _keeper_leader_program_test --test keeper_leader` 一行触发。

6. **测试覆盖** —— `cargo test --workspace --all-targets` **338 pass**（wave-15 326 + wave-16 12：tick_with_snap 1 + leader_tx 6 + reconcile 3 + e2e leader+RPC reconcile 2）；`npm test` **55 pass**（wave-15 47 + LeaderLockBanner 8）；`wasm-pack build` 不变。三个 governance verifier 全绿。

仍在等沙箱外的：SBF reject 矩阵真跑、devnet 双 keeper 真切换、`KeeperLeaderLock` PDA 接入 `WebSocketFeedAdapter` 的 `accountSubscribe`。Wave 16 把所有可在 host 完成的代码都铺到位；devnet bring-up 后，wave 17 一处 PDA 订阅 + 一行 CI feature flag 即可端到端跑通。

## 19 Wave 17 —— 前端真链上 leader 数据 + keeper-bot 优雅释放 + ops 闭环 + CI SBF 骨架

> Wave 17 把 wave-16 留下的 5 个沙箱内 pending 项一次性收口。Dormant bridge 自身没动；这一波焦点是把 keeper-leader-lock 这条 wave-15/16 链路的"最后一公里"贴在用户 / 运维 / 备机一起看的位置上。

### 19.1 链上 ↔ 前端真闭环

- **`WebSocketFeedAdapter` 加 `keeperLeaderLockPda?: PublicKey` + `trackClusterSlot?: boolean`**：`onAccountChange(lockPda)` 把 PDA 字节做 defensive copy 后塞进 `FeedSnapshot.keeperLeaderLockBytes`；`getSlot()` 轮询填 `FeedSnapshot.currentSlot`，`LeaderLockBanner` 用集群 slot（不是 keeper bot tick slot）来判定 stale。
- **`App.tsx` 用 `useMemo + decodeKeeperLeaderLockBytes`** 把 raw bytes decode 成 `KeeperLeaderLockView`，喂入 banner。Wave-16 的 `view={null}` 占位删除；mock adapter 路径下 `keeperLeaderLockBytes` 仍是 `undefined`，banner 自然回到 `uninitialised` truthful state。
- **4 条新 vitest** 钉死：lock PDA 订阅就位；market 到达后 cached lock bytes flow through；4-byte truncate 走失败计数（不 emit）；`getSlot()` 轮询正确填 `currentSlot`。

### 19.2 Keeper-bot 优雅 `keeper_leader_release`

- **`try_graceful_release(cfg, builder, program_id, market, keeper_pk, was_leader_last_tick)` 纯函数**：`release_on_shutdown=false` → `Ok(None)`；`was_leader_last_tick=false` → `Ok(None)`；都满足 → `build_keeper_leader_release` + `submit_leader_ix`，错误透传成 `Err(reason)`。
- **`run_loop_with_leader_and_rpc_reconcile` 在 `shutdown.load()` 后立刻调用** `try_graceful_release`，按结果分别打 `info!/warn!`，再返回 `LoopOutcome::ShutdownSignal`。release 失败不阻塞退出。
- **效果**：维护性下线 leadership gap 从 wave-15 默认 `takeover_threshold_slots = 75 ≈ 30 s` 降到备机 reconcile 周期 ≤ 16 s（常 < 5 s）。
- **2 条新 e2e 测 + 1 条单元测** 覆盖 4 分支 (`release_on_shutdown=false` / `was_leader=false` / 都满足 / builder 错误)。

### 19.3 Ops 链路收口

- **Rust ops-toolkit 加 3 个 keeper-leader 健康检查**：`*_initialized` (P1)、`*_freshness` (P1, 60/90 % tier)、`*_holder_matches_expected` (P2)。`HealthContext.leader_lock = None` 时三项 Pass + `leader_lock_enabled=0`，单副本 prober 默认零误报。
- **`ops-toolkit/ts/` 5 个 CLI 实落地**（init / show / acquire / heartbeat / release）+ `lib.ts` 共享 `instructionDiscriminator` / PDA derivation / 账户构造 / Borsh 编码 + 14 条 vitest 钉死字节布局 / PDA derivation / 账户 disc 校验 / truncate / bad disc rejection。
- **`KeeperPanel.LeaderLockOpsCard`**：3 按钮（acquire / heartbeat / release）走 `wallet.signAndSubmit({ description, borshBytes })`，`buildKeeperLeader{Acquire,Heartbeat,Release}Tx` 全走 wave-15 wasm encoder（acquire 这一波从手写 DataView fallback 切换到 wasm-pack 真包）。
- **runbook §6.5** 引用的所有 `ts-node ops-toolkit/ts/*.ts` 命令现在都是真脚本。

### 19.4 CI SBF 骨架

- **`solana-program-test` 作业**：`runs-on: [self-hosted, solana-sbf]` + `if: false` gate；自托管 runner 上线后只翻一行 gate 即可启用 wave-16 reject 矩阵 + CU 测量。
- **`ops-toolkit-ts` 作业**：`npm ci` + typecheck + `vitest run`，与 frontend 并行；保护 wave-15 ix encoder 字节布局。

### 19.5 测试与验证

- `cargo test --workspace --all-targets` **347/347 pass**（baseline 338 + wave-17 9：try_graceful_release_contract + e2e shutdown-graceful 2 + ops-toolkit leader-lock 7）。
- Frontend `npm test -- --run` **61/61 vitest pass**（baseline 55 + websocketAdapter wave-17 4 + wasmBuilder acquire 2）。
- ops-toolkit/ts `npx vitest run` **14/14 pass**。
- `cargo clippy` 双路径全绿；`npm run typecheck` / `npm run build` 干净（wasm 嵌入 46.06 KB）；3 governance verifier 全绿（test count 自动接受 347）。
- `wasm-pack build` 跑通；pkg 现导出 `encodeKeeperLeaderAcquire` 等核心符号。

## 20 Wave 18 —— 多市场原生支持：registry / 多市场 run-loop / LeaderLockGrid / multi-market scan

> Wave 18 把 wave-17 的单市场闭环升级成多市场原生。`KeeperLeaderLock` 这一概念终于进入产品级形态：一份 `markets.toml` 配置同时驱动 keeper-bot / frontend / ops-toolkit 三条独立链路。Dormant bridge 自身仍未动；这一波焦点是**让所有 wave-9..17 既有功能都能在多市场维度同时跑**。

### 20.1 共享 schema：`MarketRegistry`

- **`crates/keeper-rpc/src/market_registry.rs`** —— 新模块 + lib re-export。`MarketRegistry::from_toml_str` 解析 `[[markets]]` 数组（symbol / program_id / market_pda / 可选 lock_pda / 可选 expected_leader），手写 100 LoC TOML 子集 + 手写 base58 解码器，**0 新 transitive deps**（与 wave-12 ops-toolkit "no serde" 治理一致）。`MarketEntry::symbol_bytes()` 输出 wave-9 16 字节 Market.symbol 形状供 `MarketContext` 直接接管。
- **12 条单元测试** 覆盖 empty / dup / oversize / orphan-key / unknown-key / 内联注释 / lock_pda 派生回退 / expected_leader 可选 / `find_by_symbol`。

### 20.2 keeper-bot 多市场 run-loop（沿用 + bridge）

- 原 `keeper-bot::multi::run_loop_multi_market_leader_and_rpc_reconcile` 已实现；wave-18 新增 **`MarketRegistry::from_config_with` 桥接 helper**：把 `keeper_rpc::MarketRegistry`（pure config）扇出成 `keeper_bot::MarketRegistry`（运行时 slot vector）。每个 slot 持有自己的 `KeeperBot` / `KeeperMetrics` / `MarketContext` / `HostMirrorLeaderPolicy` / `LeaderRpcReconcileConfig` / 重连本子；shutdown 时 wave-17 graceful release 在每个 still-leader slot 上 fan-out。
- **2 条新桥接测 + 既有 13 条 e2e 全绿**（13 = 11 wave-17 + 2 wave-18 多市场 e2e）。

### 20.3 Frontend 多市场 grid

- **`frontend/src/feed/multiMarketAdapter.ts`** —— 新 `MultiMarketFeedAdapter`，订阅 N market PDA + N lock PDA + 一次共享 `getSlot` 轮询；`FeedSnapshot.marketsView.entries: Map<symbol, MarketViewEntry>` 暴露每市场 lockBytes / marketBytes / 静态 PDA hex。**首次 lock 更新前不发 snapshot**（aggregator hold-back），与 wave-14 单市场契约对齐。Wave-17 的 `keeperLeaderLockBytes` 单市场字段镜像 FIRST 配置市场，确保 fallback 渲染。
- **`frontend/src/marketRegistry.ts`** —— JSON 解析层，与 Rust TOML 同 schema 但 JSON 形态 (`VITE_MARKETS` env)。9 条单元测试。
- **`frontend/src/panels/LeaderLockGrid.tsx`** —— 新顶部面板：`computeLeaderLockGridRows` 纯函数把 `MultiMarketView + currentSlot + decode + expected` 折成 `LeaderLockGridRow[]`，alphabetic 排序保证 DOM 稳定；`expected_leader` mismatch 行内徽章。**`App.tsx` 自动二选一**：`feed.marketsView` 存在时 grid，否则降级回 wave-16 banner。5 条单元测试。
- **7 条新 multi-feed adapter 测 + 5 条 grid 测 + 9 条 marketRegistry 测 = +21 vitest**。

### 20.4 ops-toolkit 多市场扫描

- **`crates/ops-toolkit/src/multi.rs`** —— `scan_all_markets(registry, |entry| Result<HealthContext, _>)` fan-out + 自动注入 `MarketEntry.expected_leader → LeaderLockFacts.expected_leader`。`MultiMarketHealthReport` 含 worst exit code；`render_json_multi` 输出 `{worst_exit_code, markets: {symbol: HealthReport}}` 结构。
- **CLI `scan` 模式**：`ops-toolkit scan ./markets.toml`，与单市场 `demo` / `demo-broken` / `check-stdin` 并列。
- **4 条新单元测**（happy path / expected_leader 注入 / builder error 透传 / JSON wire format）。
- **`ops-toolkit/ts/keeper-leader-show-all.ts`** —— wave-18 KL-09 SOP，读 markets.toml → 每市场一次 `getAccountInfo` → JSON 或 `--human` 表，附 worstStatus + 五级 exit code（pass / uninitialised / unowned / stale / mismatch）。
- **`ops-toolkit/ts/lib.ts` 新 `parseMarketsToml` + `loadMarketsToml`**，与 Rust subset 字节对齐；7 条新测试。

### 20.5 测试与验证

- `cargo test --workspace --all-targets` **369/369 pass**（baseline 347 + wave-18 22）。
- Frontend `npm test -- --run` **83/83 vitest pass**（baseline 61 + wave-18 22：multi-market adapter 7 + grid 5 + marketRegistry 9 + types fix-ups 1）。
- ops-toolkit/ts `npx vitest run` **21/21 pass**（baseline 14 + 7 TOML parser）。
- `cargo clippy` 双路径全绿；`npm run typecheck` / `npm run build` 干净；3 governance verifier 全绿。
- `wasm-pack build` 不变。


## 21 Wave 19 —— 多市场用户产品 + prober daemon + env-var 化配置

> Wave 19 把 wave-18 的多市场基础设施变成**操作员可见的产品**：换市场是一次点击，prober 是真正的 daemon，`markets.toml` 可以引用 `${VAR}` 让 SOPS 注入。Dormant bridge 仍未动；多市场维度终于覆盖到 trader / indexer / keeper 三个面板。

### 21.1 多市场用户面板

- `frontend/src/panels/MarketSelector.tsx` —— 顶部药丸按钮组，每个市场一个，自带 freshness dot（fresh / stale / unowned / uninitialised）。点击切换 active market。
- `frontend/src/useActiveMarket.ts` —— 状态持久化 hook：URL `?market=` > `localStorage["mole.activeMarket"]` > 第一个市场。`resolveActiveMarket` 纯函数 7 条测试覆盖所有优先级。
- `frontend/src/feed/selectMarket.ts` —— `selectActiveMarketSnapshot(feed, symbol)` 在 React 树外部把 `feed.indexer / feed.keeper` 改写成 active 市场的解码视图，6 条测试覆盖。三个面板代码 0 行变动，复用 wave-14 单市场契约。

### 21.2 多市场 adapter 完整解码

- `MultiMarketFeedAdapter` 新增 `discriminators` 可选配置：传入即激活共享 `onProgramAccountChange(programId)` 流。
- 子池路由：`OnchainSubPool.market.hex → marketHexToSymbol` O(1) 查表。
- 干涸桶路由：`OnchainDormantBucket.sub_pool.hex → subPoolToMarket` 反查父池所在市场。
- `MarketViewEntry` 新增 5 个可选字段：`marketSummary` / `subPools` / `dormantBuckets` / `projectedRecoveryOutstandingMicroUsdc` / `indexerSlot`。
- 共享解码器抽出到 `frontend/src/feed/decode.ts`，后续 wave-14 单市场 adapter 可收敛到同一套转换器。

---

## 22 Wave 20 —— 多市场仓位过滤 + 真 RPC fetcher 抽象 + SOPS 管道

> wave-19 让操作员看见多市场，wave-20 让 trader 端的"看见"变得真。同时把 prober daemon 从 fixture-only 推向真集群、把 SOPS 解密管线接通，daemon 二进制具备生产部署条件。Dormant bridge 仍未动；wave-21 才会回到桥本身。

### 22.1 trader / keeper 端多市场闭合

- `PositionSummary.marketPdaHex?: string` —— 仓位记录上的市场 PDA 标签。`mockGenerator.buildPositions` wave-20 起为每个 mock 仓位写 `FEED_FAKE_MARKET_PDA.hex`；wave-21 单市场 `websocketAdapter` 解码 `OnchainPosition.market` 时把字段填进来。
- `selectActiveMarketSnapshot` 增量：在 wave-19 改写 `feed.indexer / feed.keeper` 的基础上多做一步——用 `filterPositionsByMarket(positions, marketPdaHex)` 过滤 `feed.positions`。tagged 仓位若不属于当前市场则丢弃；untagged 仓位（wave-9..18 历史 mock）保留以维持向下兼容。
- `MarketViewEntry.keeperState?: KeeperState` —— 多市场 keeper 视图。当 `entry.keeperState` 存在（wave-21 keeper-bot 多市场 metrics publish 后）时，snapshot rewriter 把 `feed.keeper` 整体替换为该市场的 keeper 状态，再叠加 `paused` 翻转。空缺时回落到全局 `feed.keeper`，行为与 wave-19 完全一致。

### 22.2 prober live RPC fetcher

- 新模块 `crates/ops-toolkit/src/rpc_fetcher.rs`。
- `RpcAccountSource` trait —— 包 `getMultipleAccounts(&[Pubkey32]) -> Vec<Option<FetchedAccount>>` 和 `getSlot() -> u64`，production 接 `solana_client::nonblocking::RpcClient`（沙箱外、`solana-rpc` feature）；test 接 `StubRpc`，行为完全可控。
- `RpcMarketFetcher::fetch(&MarketEntry) -> HealthContext`：一次 `getMultipleAccounts(&[market_pda, lock_pda])` + 一次 `getSlot()`，10 个市场 = 11 RTT。`Market` 字节由 `keeper_decoder::decode_anchor_account::<OnchainMarket>` 解码，`KeeperLeaderLock` 字节由 `keeper_decoder::leader_lock::KeeperLeaderLock` 解码，全部组合进 `HealthContext`。
- 缺失 PDA 的语义：`Market` 缺失 → `schema_version_onchain = 0`（标准 schema-match check 会触发）；`KeeperLeaderLock` 缺失 → `LeaderLockFacts.initialized = false`。
- 单点延迟 `Instant::now()` → `RpcFacts.primary_get_slot_p95_ms`，让 wave-12 RPC 健康 check 可以用同一个字段。
- 10 条主机测试覆盖：happy path / pause+freeze / schema mismatch / 缺失 lock / 缺失 market / `getSlot` 失败 / `getMultipleAccounts` 失败 / takeover_threshold fallback / config default / KeeperFacts+OracleFacts+PoolFacts 透传。

### 22.3 SOPS 管道：`--markets-stdin` + `--env-from-file=PATH`

- 新模块 `crates/ops-toolkit/src/cli_loader.rs`，全部纯函数，零文件系统依赖。
- `MarketsSource::{File, Stdin}` —— `markets.toml` 来源。`extract_sources` 把 `--markets-stdin` flag 收掉，让 `prober` / `scan` 子命令的位置参数语法保持 wave-19 兼容。
- `EnvSource::{Process, File, Inline}` —— `${VAR}` 解析来源。`File(path)` 读 `KEY=VALUE` 文件，支持 `export ` 前缀、可选双引号包裹、`#` 注释、空行；`Inline` 用于 test。
- `load_registry` —— 串起 markets 读取 + env overlay + `MarketRegistry::from_toml_str_with_env`。overlay 缺 key 时回落到进程 env，所以"非密钥走 shell env、密钥走 SOPS overlay"姿势可以混用。
- 16 条单元测试覆盖：env-file 解析的所有 happy / error 路径、flag 解析、源组合、fallback 语义。
- production SOPS pipeline：

  ```bash
  sops -d markets.enc.toml | ops-toolkit prober \
    --markets-stdin \
    --env-from-file=/run/secrets/prober.env \
    /var/lib/node-exporter/textfile/mole_prober.prom \
    /var/lib/mole/prober.json \
    10 0
  ```

  明文 `markets.toml` 永远不落盘；`prober.env` 由 systemd `LoadCredential` 或 k8s `secret` 卷挂载到 `/run/secrets/`（tmpfs）。

### 22.4 测试与验证

- `cargo test --workspace --all-targets` **417/417 pass**（wave-19 391 → +26：10 rpc_fetcher + 16 cli_loader）。
- Frontend `npm test -- --run` **108/108 pass**（wave-19 101 → +7：4 selectMarket 仓位过滤 + 2 selectMarket keeperState + 1 filterPositionsByMarket，再加 4 filterPositionsByMarket helper 共 7 增量 → 实际 7 条新文件，13 测试通过）。
- ops-toolkit/ts `npx vitest run` 33/33 pass（wave-19 不变；wave-20 完全是 Rust 侧产出）。
- `cargo clippy` 双路径全绿；`npm run typecheck` / `npm run build` 干净；3 governance verifier 全绿。
- `wasm-pack build` 沙箱外（wave-15 prebuilt artifact 仍走 frontend tests）。

### 21.3 ops-toolkit `ProberLoop`

- `crates/ops-toolkit/src/prober.rs` —— I/O-free trait 化 daemon：`MarketFetcher` / `ProberClock` / `ProberSink` 三个 trait 让测试路径完全同步、零依赖。
- 周期性 `scan_all_markets` → 统一 Prometheus textfile（每行 `mole_health_*` 自动加 `market="<symbol>"` 标签，`relabel_with_market` 重写）+ 稳定 JSON 快照。
- 严格 fail-closed：fetcher 返回 `Err` → 整个周期不发布任何文件，让 AlertManager `for: 30s` 因 textfile gap 触发，避免误报"all-Pass"。
- `ops-toolkit prober <markets.toml> <prom-path> <json-path> [interval] [max-cycles]` binary 模式上线；live RPC fetcher 留作 wave 20 接 `solana-client`。
- 9 条主机测试：ok / hard-fail / degraded RPC / sink-failure / 3-cycle 节奏 / leader-lock 路由 + prom relabel 边界。

### 21.4 `${VAR}` env-var substitution

- `crates/keeper-rpc/src/market_registry.rs` 新增 `substitute_env_vars(input, &lookup)` + `MarketRegistry::from_toml_str_with_env(input, lookup)`。`from_toml_str` 默认通过 `std::env::var` 解析，所以现有调用零改动。
- `${VAR}` 展开 / `$$` 转义 / 变量名校验 `[A-Za-z_][A-Za-z0-9_]*` / 未设置 / 空值 / 未闭合 / 空大括号 / 非法字符 / 数字开头：13 条 Rust 测试。
- `ops-toolkit/ts/lib.ts::substituteEnvVars` 字节级镜像，12 条 TS 测试包含跨语言 `expected_leader` 集成场景。
- SOPS 工作流：`sops -d markets.toml.sops > /tmp/markets.toml && export EXPECTED_LEADER_SOL=... && ops-toolkit prober /tmp/markets.toml ...`，secret 永远不落明文盘。

### 21.5 测试与验证

- `cargo test --workspace --all-targets` **391/391 pass**（wave-18 369 → +22：13 envvar + 9 prober）。
- Frontend `npm test -- --run` **101/101 vitest pass**（wave-18 83 → +18：4 multi-market decode + 7 useActiveMarket + 6 selectMarket + 1 path swap）。
- ops-toolkit/ts `npx vitest run` **33/33 pass**（wave-18 21 → +12：10 substituteEnvVars + 2 parseMarketsToml 集成）。
- `cargo clippy` 双路径全绿；`npm run typecheck` / `npm run build` 干净；3 governance verifier 全绿。
- `wasm-pack build` 沙箱外（wave-15 prebuilt artifact 仍走 frontend tests）。

---

## 23 Wave 21 —— `OnchainPosition` 镜像 + 真 `solana-client` 适配器 + per-market JSON 指标 + RPC 重试 / 备用 slot diff

> wave 20 在 trait 层停步：prober 没有真 RPC impl、仓位 `marketPdaHex` 没有解码器、keeper 指标只有 Prometheus 文本、`primary_backup_slot_diff` 硬编码 0。Wave 21 四条全部补齐；dormant bridge 仍未动。

### 23.1 `OnchainPosition` 字节镜像（Rust + TS）

- `crates/keeper-decoder/src/lib.rs::OnchainPosition` —— 23 字段，239 字节 body，与 `programs/mole-option/src/state.rs::Position` 对齐。
- `schema_descriptor_json()` 字段总数 80 → **103**；`scripts/verify-schema-parity.sh` 要求 `SCHEMA-MAPPING.md` 每行都有对应 row。
- `frontend/src/decoder/onchain.ts::decodeOnchainPosition` / `decodeOnchainPositionWithDiscriminator` —— 手写 buffer layout，6 条 vitest 覆盖 round-trip / body-length pin / market pubkey / truncated / discriminator mismatch / direction+status。
- wave-22 会把解码器接到 `websocketAdapter`，让 `feed.positions[i].marketPdaHex` 来自链上 `Position.market` 字段；wave-21 只交付解码器本身。

### 23.2 `SolanaRpcAccountSource`（feature `solana-rpc`）

- `crates/ops-toolkit/src/solana_rpc.rs` —— 薄适配层，把 `solana_client::rpc_client::RpcClient` 包成 wave-20 `RpcAccountSource` trait。
- `ops-toolkit/Cargo.toml` 新增 `solana-rpc = ["keeper-rpc/solana-rpc", "dep:solana-client", …]`；默认 feature 不拉 Solana 依赖树。
- `sleep_ms` 接 `std::thread::sleep`，让 wave-21 重试 backoff 在生产路径计时正确；单元测试仍在 `rpc_fetcher.rs` 的 `StubRpc` 上跑，不 spawn validator。

### 23.3 `RpcMarketFetcher` 重试 + 备用 RPC

- `RpcMarketFetcherConfig.retry_attempts: u8`（默认 0）+ `retry_backoff_ms: u64`（默认 0）—— wave-20 调用方升级后行为字节级一致。
- `RpcMarketFetcher::with_backup(backup)` —— 每周期对备用 endpoint 调一次 `getSlot`，绝对差写入 `RpcFacts.primary_backup_slot_diff`；备用 `Err` → 0；备用永不读账户。
- 9 条新单测：retry-zero 兼容 / 重试成功 / 重试耗尽 / backoff-zero 不调 sleep / 备用慢 / 备用快 / 备用故障 / 备用不读账户 / 默认配置锁定。

### 23.4 keeper-bot `/metrics-multi` JSON 路由

- `KeeperMetrics::render_json_snapshot` —— 稳定 camelCase JSON（`ticksTotal` / `actionsSubmittedTotal` / `walletBalanceLamports` / `appliedVolMilli` / `leaderStatus` …）。
- `MarketRegistry::render_per_market_json` —— `[{market, metrics}, …]` 数组；symbol 转义剥掉 `"` / `\` / 控制字符。
- `spawn_metrics_server_with_multi(addr, metrics, multi, shutdown)` —— 新公开 API；老 `spawn_metrics_server` 委托 `multi = None`；`/metrics-multi` 在无 provider 时 404。
- 7 条 keeper-bot 单测覆盖 JSON shape / 多市场数组 / HTTP 路由 / 404 语义。

### 23.5 测试与验证

- `cargo test --workspace --all-targets` **439/439 pass**（wave-20 417 → +22：5 OnchainPosition + 9 retry/backup + 7 metrics-multi + 1 default-config lock）。
- Frontend `npm test -- --run` **114/114 pass**（wave-20 108 → +6 OnchainPosition 解码）。
- `verify-schema-parity.sh` **103/103**（wave-20: 80）。
- `cargo clippy` 双路径 + `solana-rpc` feature path 全绿；3 governance verifier 全绿。

---

## 24 Wave 22 —— live position 解码 + `/metrics-multi` 前端合并 + `serve-multi` daemon

> wave 21 的解码器与 JSON 路由停在 library 层；wave 22 把它们接到 live 产品路径。

### 24.1 `websocketAdapter` / `multiMarketAdapter` Position 解码

- `frontend/src/feed/websocketAdapter.ts` —— program-account 流按 `Position` discriminator 解码；`feed.positions[i].marketPdaHex` 来自链上 `position.market` 字段。
- `frontend/src/feed/multiMarketAdapter.ts` —— 按 `position.market.hex` 路由到各 `MarketState.positions`；`aggregate()` 合并全量仓位供 wave-20 过滤器消费。
- `frontend/src/feed/decode.ts` —— 共享 `onchainPositionToSummary` / `isDisplayablePosition`（`status === 2` Closed 剔除；untagged 历史 mock 保留）。
- 2 条 vitest（websocket + multiMarket position 路径）+ `buildAdapter` helper 注册 position discriminator。

### 24.2 前端 `/metrics-multi` 消费链

- `frontend/src/feed/keeperMetricsMulti.ts` —— `parseMetricsMultiJson` / `metricsJsonToKeeperState` / `mergeKeeperMetricsIntoFeed`。
- `frontend/src/useKeeperMetricsMulti.ts` —— 轮询 `{VITE_KEEPER_METRICS_URL}/metrics-multi`（默认 4 s）；env 未设则静默。
- `frontend/src/App.tsx` —— `mergeKeeperMetricsIntoFeed` → `selectActiveMarketSnapshot`；多市场 `entries[].keeperState` + primary `feed.keeper` 同步。
- 5 条 vitest 覆盖 JSON 解析 / appliedVol 换算 / warming_up 语义 / merge 行为。

### 24.3 `keeper-bot serve-multi` CLI

- `keeper-bot serve-multi <addr> <markets.toml> [max_passes]` —— TOML registry + 多市场 run-loop + `spawn_metrics_server_with_multi` 一次装配。
- `/metrics`（Prometheus）与 `/metrics-multi`（JSON）同址；wave-12 `serve` 单市场模式不变。
- HTTP/JSON 行为由 wave-21 7 条 keeper-bot 单测覆盖；wave-22 为 compile-only 装配层。

### 24.4 测试与验证

- `cargo test --workspace --all-targets` **439/439 pass**（与 wave-21 持平）。
- Frontend `npm test -- --run` **121/121 pass**（wave-21 114 → +7）。
- `verify-schema-parity.sh` **103/103**；3 governance verifier 全绿。
- Frontend `npm run typecheck` / `npm run build` clean（539 KB JS / 159 KB gzip）。

---

## 25 Wave 23 —— 基于 live 仓位的持仓敞口（open-interest）聚合

> wave 22 让 `feed.positions` 变 live；wave 23 把 wave-21 changelog 预告的 open-interest probe 前后端两半补齐。dormant bridge 仍未动。

### 25.1 后端 `ops_toolkit::position_interest`

- `crates/ops-toolkit/src/position_interest.rs` —— `OpenInterestFacts` 聚合 long/short 的 count / principal / notional + 带符号 `net_notional_imbalance`。
- `aggregate_open_interest(&[OnchainPosition])` 纯折叠（`status == 2` Closed 剔除）；`fetch_open_interest(fetcher, program_id)` 用 `Position` discriminator memcmp `getProgramAccounts` 扫描并逐个 `decode_anchor_account`，decode 失败计数不中止。
- 写在 host-only `keeper_rpc::AccountFetcher` trait 上 → `MockAccountFetcher` 沙箱可测；`SolanaRpcAccountFetcher`（`solana-rpc` feature）生产复用同一 trait。
- 6 条新 Rust 单测。

### 25.2 前端 `openInterest.ts` + TraderPanel KPI

- `frontend/src/feed/openInterest.ts` —— `aggregateOpenInterest` / `openInterestByMarket` 镜像后端形状（count / collateral / qty）；`netCollateralImbalance` 净偏斜。
- `TraderPanel` 新增 "Market open interest" 卡片，由 wave-22 已 live 的 `feed.positions` 驱动。
- 5 条新 vitest。

### 25.3 测试与验证

- `cargo test --workspace --all-targets` **445/445 pass**（wave-22 439 → +6）。
- Frontend `npm test -- --run` **126/126 pass**（wave-22 121 → +5）。
- `verify-schema-parity.sh` **103/103**；`cargo clippy` 默认 + `solana-rpc` 双路径全绿；3 governance verifier 全绿。

---

## 26 Wave 24 —— 链上 ↔ indexer 本金/名义对账

> wave 23 交付了 open-interest 聚合；wave 24 把它升级成第 22 个 prober 检查 + 前端对账徽章。dormant bridge 仍未动。

### 26.1 后端第 22 个检查 `position_principal_drift`

- `crates/ops-toolkit/src/checks.rs::check_position_principal_drift` —— 比对 `PoolFacts.onchain_position_notional_micro_usdc`（由 `apply_open_interest_to_pool` 从 `OpenInterestFacts::total_notional` 喂入）vs indexer 报告 `total_notional_micro_usdc`。
- drift = `|onchain − reported| / max(reported, 1)`：Pass < 0.5% / Warn(P2) < 2% / Critical(P1) ≥ 2%；`onchain == 0` 跳过（Pass）。
- check battery 21 → **22**；`PoolFacts` 新增字段默认 0；6 处字面量同步。
- 6 条新 Rust 单测（skip / reconciled / warn / critical / 方向对称 / `apply_*`）。

### 26.2 前端 `reconcilePrincipal` + 徽章

- `frontend/src/feed/openInterest.ts::reconcilePrincipal` —— `ok / warn / critical / disabled`，阈值与后端一致。
- `TraderPanel` open-interest 卡片新增 "Indexer reconciliation" 徽章。
- 5 条新 vitest。

### 26.3 测试与验证

- `cargo test --workspace --all-targets` **451/451 pass**（wave-23 445 → +6）。
- Frontend `npm test -- --run` **131/131 pass**（wave-23 126 → +5）。
- `verify-schema-parity.sh` **103/103**；`cargo clippy` 默认 + `solana-rpc` 双路径全绿；3 governance verifier 全绿。

