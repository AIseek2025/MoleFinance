# MoleOption Shares 模型实现细则与边界条件

## 1. 文档定位

本文件给出 `14-当前区块即时清算与简化模型评估.md` 中“方向权益池 + shares + recovery_shares + dormant bucket”模型的合约级实现细则。所有边界条件、舍入规则、攻击防护以及与白皮书原始逐仓语义的映射，都必须以本文为准。

`05-核心机制与数学模型.md` 中关于 `locked_loss / realized_profit_balance` 的逐仓表达，仅作为离线 oracle 与白皮书原则解释。生产合约不在 `Position` 账户持有这两个字段（除非作为衍生统计字段，见第 12 节）。

## 2. Market 与 SubPool 字段规范

### 2.1 Market

```rust
pub struct Market {
    pub global_config: Pubkey,
    pub symbol: [u8; 16],
    pub collateral_mint: Pubkey,
    pub vault: Pubkey,
    pub fee_vault: Pubkey,
    pub oracle_price_feed: Pubkey,
    pub leverage_bps: u32,

    pub min_margin: u64,
    pub max_margin_per_position: u64,
    pub max_total_principal: u128,
    pub max_total_notional: u128,

    pub open_fee_bps: u16,

    pub max_oracle_age_seconds: i64,
    pub max_confidence_bps: u16,
    pub max_price_move_bps_per_sync: u32,

    pub price_tick: u64,
    pub tick_aggregation_factor: u32,
    pub max_dormant_bucket_count_per_direction: u32,

    pub paused: bool,
    pub frozen_new_position: bool,
    pub schema_version: u16,

    pub sub_pool_count: u32,
    pub sub_pool_root: Pubkey,
    pub bump: u8,
}
```

### 2.2 SubPool

每个 SubPool 是该 Market 的一个独立写锁单元。

```rust
pub struct SubPool {
    pub market: Pubkey,
    pub sub_pool_id: u32,

    pub long_pool_equity: u128,
    pub short_pool_equity: u128,

    pub long_active_shares: u128,
    pub short_active_shares: u128,
    pub long_recovery_shares: u128,
    pub short_recovery_shares: u128,

    pub long_notional: u128,
    pub short_notional: u128,

    pub long_dormant_bucket_root: Pubkey,
    pub short_dormant_bucket_root: Pubkey,

    pub last_price: u64,
    pub last_sync_slot: u64,

    pub long_dust: u128,
    pub short_dust: u128,

    pub bump: u8,
}
```

### 2.3 不变量

任何指令执行后必须保持：

```text
vault_balance >= sum(all sub_pools.long_pool_equity + sub_pools.short_pool_equity)
                + sum(market.long_dust + market.short_dust)
                + sum(any pending close request reserve)
```

```text
long_pool_equity == 0      ⇔   long_active_shares == 0
short_pool_equity == 0     ⇔   short_active_shares == 0
```

active shares 为 0 时，pool_equity 必须严格为 0（不允许残留小额）。这强制约束在第 6 节的归零迁移中执行。

## 3. 子池路由规则

### 3.1 PDA 化路由

子池数量 `sub_pool_count` 在 Market 初始化时设定，且只能通过治理提案增加（见 `16-合约升级与治理紧急响应.md`）。

子池 ID 由开仓指令的输入参数决定，但合约必须强制：

```text
sub_pool_id = derive_sub_pool_id(market, owner_pubkey)
            = u32::from_le_bytes(hash(market || owner)[0..4]) % sub_pool_count
```

也即同一钱包在同一市场下被路由到固定子池。攻击者可以创建多个钱包绕过路由限制，但每个钱包仍然只能在其对应子池开仓。

### 3.2 大额订单的拆分

如果用户单笔订单本金超过 `single_position_sub_pool_cap`：

- 合约要求用户拆成多笔交易，分发到不同子池。
- 拆分规则由 `derive_sub_pool_id_for_split(market, owner, position_index)` 决定。
- 拆分后每笔的 sub_pool_id 与下标无关，但同一笔订单内部 sub_pool_id 必须按链上规则可计算。

### 3.3 跨子池 rebalance

子池之间不允许任意 rebalance，因为这会跨子池转移 pool_equity，破坏每个子池内部 share 公平。

唯一例外：

- 治理通过 `16` 文档 Level 5 流程，针对某个长期低活子池执行“合并提案”。
- 合并必须先冻结被合并子池的开仓（不冻结平仓），等待存量仓位自然清空 (或 dormant) 之后才执行合并。
- 合并不允许跨方向、不允许跨杠杆、不允许跨标的。

## 4. sync_pool 详细公式

### 4.1 触发条件

任何会改变状态的指令在主体逻辑执行前必须先调用 `sync_pool(sub_pool, oracle_price)`：

```text
open_position
close_position
request_close_position
claim_closed_position
```

`sync_pool` 自身也可作为公开指令被任何人调用，用于让长期无人交互的子池保持 last_price 与 oracle 接近。Keeper 必须保证每个子池在 `max_idle_slots` 内被同步至少一次。

### 4.2 公式

```text
sync_pool(sub_pool, P_now):
    if P_now == sub_pool.last_price:
        sub_pool.last_sync_slot = current_slot
        return

    require(P_now > 0)
    require(price_move_bps(P_now, sub_pool.last_price) <= max_price_move_bps_per_sync)

    long_pnl_increment = signed_mul_div(
        sub_pool.long_notional,
        P_now - sub_pool.last_price,
        sub_pool.last_price
    )

    short_pnl_increment = -1 * signed_mul_div(
        sub_pool.short_notional,
        P_now - sub_pool.last_price,
        sub_pool.last_price
    )

    if long_pnl_increment > 0 && short_pnl_increment < 0:
        loss_capacity = sub_pool.short_pool_equity
        transfer = min(long_pnl_increment as u128, loss_capacity)
        sub_pool.long_pool_equity = checked_add(sub_pool.long_pool_equity, transfer)
        sub_pool.short_pool_equity = checked_sub(sub_pool.short_pool_equity, transfer)

    elif short_pnl_increment > 0 && long_pnl_increment < 0:
        loss_capacity = sub_pool.long_pool_equity
        transfer = min(short_pnl_increment as u128, loss_capacity)
        sub_pool.short_pool_equity = checked_add(sub_pool.short_pool_equity, transfer)
        sub_pool.long_pool_equity = checked_sub(sub_pool.long_pool_equity, transfer)

    sub_pool.last_price = P_now
    sub_pool.last_sync_slot = current_slot

    if sub_pool.long_pool_equity == 0:
        rotate_active_to_recovery(LONG, sub_pool, P_now)

    if sub_pool.short_pool_equity == 0:
        rotate_active_to_recovery(SHORT, sub_pool, P_now)

    activate_dormant_buckets_if_needed(sub_pool, P_now)
```

### 4.3 路径相关性显式声明

`sync_pool` 的结果与中间路径相关。例如 `P0 = 100, P1 = 110, P2 = 105` 与 `P0 = 100, P2 = 105` 两条路径会得到不同的子池状态。这是金融市场的真实路径相关性，协议显式接受这一点：

- 任何用户都可以通过调用 `sync_pool` 把过期的中间价格“跳过”，但这不会让协议产生“凭空盈利”。`transfer = min(pnl, counterparty_equity)` 决定了协议永远不会支付超过对手池实际能承受的损失。
- 长期不被同步的子池会被攻击者抢先触发 sync 来收割 transfer。Keeper 必须主动维持子池新鲜度；同时合约设置 `max_idle_slots`，超时则强制下次任何指令必须传入最新价格，且禁止用户用过期价格抢跑。

### 4.4 价格保护

任何用户提交开仓 / 平仓 / claim 时必须传入：

```text
expected_price_min: u64
expected_price_max: u64
expected_price_max_age_slots: u64
```

合约校验：

```text
sync_pool 后的 last_price ∈ [expected_price_min, expected_price_max]
last_sync_slot - oracle_publish_slot <= expected_price_max_age_slots
```

否则交易失败，避免被三明治抢跑。

## 5. shares 铸造

### 5.1 一般情况

```text
direction_pool_equity = sub_pool.<direction>_pool_equity
direction_total_shares = sub_pool.<direction>_active_shares  // recovery 不参与稀释

if direction_pool_equity == 0:
    shares_minted = principal
else:
    shares_minted = mul_div_floor(principal, direction_total_shares, direction_pool_equity)
```

### 5.2 边界条件强校验

```text
require(principal >= market.min_margin)
require(shares_minted > 0)
require(direction_pool_equity + principal <= MAX_U128_SAFE_BUDGET)
require(direction_total_shares + shares_minted <= MAX_U128_SAFE_BUDGET)
```

如果 `shares_minted == 0`（极小本金 + 极大池估值），交易失败并返回 `SharesMintedTooSmall`。这避免“资金被吞但用户拿不到 shares”的资金黑洞。

### 5.3 dust 与极端比率

如果 `mul_div_floor` 后产生残余：

```text
dust_into_pool = principal - mul_div_floor(shares_minted, direction_pool_equity, direction_total_shares)
```

dust 留在 sub_pool 的 `long_dust` 或 `short_dust` 字段，不进入 pool_equity。每隔治理设定窗口由 `harvest_dust` 指令把 dust 转入协议保险基金或 fee_vault，不向用户支付。

### 5.4 池权益归零时的新仓

`rotate_active_to_recovery` 执行后：

```text
sub_pool.long_recovery_shares += sub_pool.long_active_shares
sub_pool.long_active_shares = 0
sub_pool.long_pool_equity == 0  // 应已为 0
register_dormant_bucket(LONG, P_now, sub_pool.long_recovery_shares_delta)
```

之后新用户在 `long_pool_equity == 0` 时开仓：

```text
sub_pool.long_pool_equity = principal
sub_pool.long_active_shares = principal           // 新一轮 active
sub_pool.long_notional += principal * leverage
```

新仓全部进入 `long_active_shares`，不会稀释历史 `long_recovery_shares`，因为后者只有在 dormant bucket 被激活时才参与未来 transfer 分配。

### 5.5 反向稀释保护

当 `direction_pool_equity` 接近 0 但尚未归零时，新仓的 `shares_minted = principal * total_shares / pool_equity` 可能极大，造成反向稀释：

- 老用户原本持有的 shares 价值被新用户的“相对庞大” shares 占比稀释。

为防止这种情况：

```text
# 触发反向稀释保护的条件：每 share 锚定的池估值低于 dilution_safety_bps / BPS_SCALE
# 等价于  pool_equity / shares < dilution_safety_bps / 10_000
if direction_pool_equity * 10_000 < direction_total_shares * dilution_safety_bps:
    require(direction_pool_equity == 0, OR
            return DilutionRiskTooHigh)
```

注：早期版本曾把左右两侧写反（`pool_equity * dilution_safety_bps < shares * 10_000`），那种写法在稳态 `pool_equity == shares` 时也会触发拒单，是错误的；以本节为准。生产合约 `clearing-core::engine::open_position` 实现的就是上面这个正确版本。

也就是当池子估值与 shares 数量比率低于阈值（例如 1 / 10 万）时，强制走"归零路径"：先把 active shares 全量迁移到 recovery，并把残余 pool_equity 计入 dust。这样新仓在 active 层从干净状态开始，不会立即被淹没。

`dilution_safety_bps` 初值建议 `1`（即 1 bps）。即 `direction_pool_equity / direction_total_shares < 10^-4` 时强制归零。生产参数化时常用值为 `10`（0.1%）或 `100`（1%）。

## 6. recovery_shares 与 dormant bucket

### 6.1 DormantBucket 账户

```rust
pub struct DormantBucket {
    pub sub_pool: Pubkey,
    pub direction: Direction,
    pub zero_price_tick: i64,           // floor(zero_price / price_tick / tick_aggregation_factor)
    pub total_recovery_shares: u128,
    pub total_recovery_notional: u128,
    pub position_count: u64,
    pub bump: u8,
}
```

注意：单个 bucket 不存储仓位列表，只聚合 shares 和 notional。

### 6.2 Bucket 树

每个 SubPool 维护一棵分层 bucket 树：

```text
SubPoolDormantTree {
    direction
    levels: [DormantLevel; MAX_LEVELS]
    total_active_recovery_shares
    activation_index_root
}
```

每层包含若干 bucket，按 `zero_price_tick` 范围聚合。多头方向树支持 `prefix_sum_up_to_tick(P_now_tick)`，空头方向树支持 `suffix_sum_from_tick(P_now_tick)`。

`prefix_sum_up_to_tick` 和 `suffix_sum_from_tick` 必须是 O(log N) 操作，使用 Fenwick tree 或 segment tree。

### 6.3 创建 bucket

仓位归零或 active rotate 到 recovery 时：

```text
zero_tick = bucket_tick_of(P_now)
if exists bucket(direction, zero_tick):
    bucket.total_recovery_shares += migrated_shares
    bucket.total_recovery_notional += migrated_notional
    bucket.position_count += migrated_count
else:
    require(sub_pool_existing_bucket_count(direction) <
            market.max_dormant_bucket_count_per_direction)
    create new bucket
```

### 6.4 激活规则

```text
activate_dormant_buckets_if_needed(sub_pool, P_now):
    P_tick = bucket_tick_of(P_now)

    LONG side:
        sum = prefix_sum_up_to_tick(LONG_TREE, P_tick)
    SHORT side:
        sum = suffix_sum_from_tick(SHORT_TREE, P_tick)
```

`sum` 表示在当前价格下应该重新激活的 recovery_shares 总量。然而**激活只能在有可分配 transfer 出现时影响分配权重**，而不能直接把 recovery_shares 提升为 active_shares 或写回 pool_equity。

具体规则：

- 当下一次 sync_pool 产生 transfer 时，
  - `claim_weight_active = active_pnl_increment_during_period`，
  - `claim_weight_recovery = recovery_claim_demand`，其中 `recovery_claim_demand` 是该方向已激活 recovery_shares 在当前价格下相对于其 zero_price_tick 的“账面应恢复价值”。
- 按 `claim_weight_active : claim_weight_recovery` 的比例把 transfer 分别记入：
  - `pool_equity` 增量按 active_shares 分配的部分进入 active 池。
  - 进入 recovery 部分按各 dormant bucket 的 `claim_weight` 比例增加该 bucket 的可领取价值。
- recovery_shares 在领取时（用户调用 `claim_dormant_recovery`）才转换为可领取金额，并销毁对应 recovery_shares。

### 6.5 关键边界

- 价格回到 `zero_price_tick` 但还没有新对手方亏损：不会有 transfer，recovery_shares 不会凭空恢复价值。
- 价格穿越大量 bucket：使用 prefix/suffix 聚合 O(log N) 计算激活总量，不需要遍历每个 bucket。
- 价格反复抖动：每次激活总量重新计算，不会产生历史路径效应。

## 7. 仓位结构

### 7.1 Position

```rust
pub struct Position {
    pub market: Pubkey,
    pub sub_pool: Pubkey,
    pub owner: Pubkey,
    pub position_id: u64,

    pub direction: Direction,
    pub status: PositionStatus,

    pub principal: u64,
    pub leverage_bps: u32,
    pub notional: u128,

    pub active_shares: u128,
    pub recovery_shares: u128,
    pub recovery_bucket: Option<Pubkey>,
    pub zero_price: u64,
    pub zero_price_tick: i64,
    pub dormant: bool,

    pub entry_price: u64,
    pub last_sync_slot: u64,

    pub opened_at: i64,
    pub updated_at: i64,
    pub closed_at: i64,
    pub schema_version: u16,
    pub bump: u8,
}
```

### 7.2 状态机

```text
Open    持有 active_shares, 可立即平仓
Dormant 持有 recovery_shares, 处于 dormant bucket, 暂时不参与 claim
Open    重新激活后回到 active (但 active_shares = 0, 仅靠 recovery_shares 取回)
Closed  最终关闭, 不再参与
```

实际状态枚举只保留 `Open / CloseRequested / Closed`，dormant 由 `dormant: bool` 字段标记。

### 7.3 没有 locked_loss 字段

生产合约的 `Position` 不再持有 `locked_loss / realized_profit_balance`。这两个变量只在以下场景出现：

- 离线 oracle 用于解释白皮书原则。
- 索引服务可以从 shares 历史与 sync_pool 事件推导每个仓位的等价 `locked_loss`，用于展示与审计。

合约层用户最终可领取金额公式由 shares 决定：

```text
withdrawable = pool_equity_active_part * position.active_shares / sub_pool.<direction>_active_shares
             + recovery_bucket_claimable_for_position
             - pending_dust
```

第一项是用户在 active 层的份额价值；第二项是用户作为 recovery_shares 累积可领取价值（只有在 dormant bucket 被激活并产生 transfer 时增长）。

## 8. 平仓流程

### 8.1 close_position 主路径

```text
close_position:
    sync_pool(sub_pool, P_now, expected_price_check)
    require(position.status == Open)
    require(position.owner == signer)

    # active 部分
    active_value = mul_div_floor(
        sub_pool.<direction>_pool_equity,
        position.active_shares,
        sub_pool.<direction>_active_shares
    )

    sub_pool.<direction>_pool_equity -= active_value
    sub_pool.<direction>_active_shares -= position.active_shares
    sub_pool.<direction>_notional -= position.notional

    # recovery 部分
    recovery_value = take_recovery_share_value(position.recovery_bucket, position.recovery_shares)

    withdrawable = active_value + recovery_value

    require(vault_balance >= withdrawable)
    transfer_from_vault_to_user(withdrawable)

    position.status = Closed
    position.closed_at = now
    emit PositionClosed { ... }
```

### 8.2 关闭 0 价值仓位

如果 `withdrawable == 0`：

- 普通 `close_position` 拒绝执行，要求用户调用 `force_close_zero_value_position`。
- `force_close_zero_value_position` 必须传入显式标志 `acknowledge_forfeit_recovery = true`，否则失败。
- 强制关闭事件 `PositionForfeitClosed { position }` 用于审计与前端警告。

### 8.3 dormant 仓位关闭

当 `position.dormant == true`：

- 用户可以选择保留仓位等待恢复。
- 也可以调用 `force_close_zero_value_position` 主动放弃。
- 关闭后 dormant_bucket 减去对应 recovery_shares 与 notional。

### 8.4 转账顺序

转账永远在状态写入之后。任何 `transfer_from_vault_to_user` 调用前必须保证：

```text
sub_pool 状态已写入
position.status = Closed
recovery_bucket 已扣减
event 已 emit
```

## 9. 开仓流程

### 9.1 open_position 主路径

```text
open_position:
    sync_pool(sub_pool, P_now, expected_price_check)
    require(!market.paused && !market.frozen_new_position)
    require(amount >= market.min_margin + open_fee_estimate)

    open_fee = mul_div_ceil(amount, market.open_fee_bps, 10_000)
    principal = amount - open_fee

    require(principal >= market.min_margin)

    transfer_from_user_to_vault(principal)
    transfer_from_user_to_fee_vault(open_fee)

    direction_pool_equity = sub_pool.<direction>_pool_equity
    direction_active_shares = sub_pool.<direction>_active_shares

    if direction_pool_equity == 0:
        shares_minted = principal
    else:
        if direction_pool_equity * dilution_safety_bps < direction_active_shares * 10_000:
            return DilutionRiskTooHigh   # 客户端可重试: 先调用 sync_pool 让 active 池归零迁移
        shares_minted = mul_div_floor(principal, direction_active_shares, direction_pool_equity)
        require(shares_minted > 0, SharesMintedTooSmall)

        accounted_principal = mul_div_floor(shares_minted, direction_pool_equity, direction_active_shares)
        dust = principal - accounted_principal
        sub_pool.<direction>_dust += dust

    sub_pool.<direction>_pool_equity += accounted_principal_or_principal
    sub_pool.<direction>_active_shares += shares_minted
    sub_pool.<direction>_notional += principal * leverage

    create Position { active_shares = shares_minted, ... }

    emit PositionOpened { ... }
```

### 9.2 同区块顺序

同一区块多笔交易按 Solana 提交顺序串行执行。每笔交易前都执行 `sync_pool`。第一笔 sync 后池状态变化，第二笔 sync 检查 `last_price == P_now` 直接跳过 transfer 计算。这天然防止同价格连续 transfer。

## 10. 极端价格保护

### 10.1 单次价格跳变上限

```text
price_move_bps(P_now, sub_pool.last_price) <= max_price_move_bps_per_sync
```

超出则暂停该 SubPool。建议 `max_price_move_bps_per_sync = 2000` (20%)。

### 10.2 oracle 多源

虽然首版仅使用 Pyth，但合约接口必须设计为可扩展接收多个 oracle 源。Pyth 提供 confidence interval；合约同时检查：

```text
oracle.publish_time fresh
oracle.confidence / oracle.price <= max_confidence_bps
oracle.price > 0
```

### 10.3 大跳变情况下 dormant bucket 防爆

价格剧烈变化激活大量 bucket。激活逻辑使用前缀/后缀聚合 O(log N)，不遍历 bucket。但如果 prefix sum 需要的账户超过 Solana 单笔交易上限：

- 把激活改为“延迟生效”：本次 sync_pool 不直接更新每个 bucket 的可领取价值，只更新 `pending_activation_total`。
- 后续用户在领取 recovery 资金时，需先调用 `apply_dormant_activation_for_position`，惰性更新自己仓位对应 bucket 的 claim 数据。
- 限制单笔 sync_pool 触及的 bucket 数量上限；超过时拒绝 sync 并要求 keeper 分批触发。

## 11. 不变量与运行时检查

每个状态修改指令结束前必须自动执行：

```text
INV1: sub_pool.long_pool_equity, short_pool_equity >= 0
INV2: sub_pool.long_active_shares == 0  ⇔  long_pool_equity == 0
INV3: vault_balance >= sum_active_pool_equity_all_sub_pools + recovery_redeemable_total
INV4: position.active_shares <= sub_pool.<direction>_active_shares
INV5: position.recovery_shares <= recovery_bucket.total_recovery_shares
INV6: long_notional, short_notional >= 0
INV7: market.schema_version == position.schema_version (or migration_in_progress)
```

任何 INV 失败立即触发自动暂停，并 emit `InvariantViolation` 事件。

## 12. 与 05 文档逐仓表达的兼容映射

为了让审计、文档说明、风险解释与白皮书一致，合约层必须提供索引能力：

- 通过事件 `PositionOpened`, `PoolSync`, `Transfer`, `RecoveryAccrual`, `PositionClosed`, 索引服务可以重建每个仓位的等价 `locked_loss / realized_profit_balance`。
- 索引重建必须满足：
  ```text
  shares_withdrawable_t = principal - locked_loss_t + realized_profit_balance_t
  ```
- 这只是展示逻辑，不影响合约层资金流。

合约可以选择性地在 `PositionClosed` 事件中输出当时计算出的等价 `locked_loss / realized_profit` 字段（基于事件历史聚合），方便链上审计但不作为资金路径。

## 13. 与 16/17 文档的衔接

- 任何 shares 模型字段的添加、移除、语义变化必须按 `16-合约升级与治理紧急响应.md` 的 Level 5 流程执行。
- `dilution_safety_bps`、`max_price_move_bps_per_sync`、`max_dormant_bucket_count_per_direction` 调整属于 Level 1 风险参数。
- 合规与披露要求遵循 `17-合规与地理屏蔽.md`。

## 14. 边界情况测试清单

合约级实现必须覆盖以下测试：

```text
T1  pool_equity == 0 时新仓只进入 active, recovery 不被稀释
T2  pool_equity 极小时触发 dilution_safety 自动归零迁移
T3  shares_minted == 0 必须拒绝
T4  同区块两笔同方向开仓后单笔平仓, total_shares 与 pool_equity 比率不变
T5  价格穿越多个 dormant bucket, 激活总量等于 prefix 聚合值
T6  recovery_shares 在没有对手亏损时不会增加价值
T7  transfer 路径相关性: P0->P1->P2 vs P0->P2 结果不同时, 协议状态自洽
T8  极端跳变 (+/-50%) 触发自动暂停
T9  attacker 创建大量微小 dormant bucket, 被 max_dormant_bucket_count_per_direction 限制
T10 强制关闭 0 价值仓位前必须显式 acknowledge
T11 多签升级期间 force_close 仍可用 (受暂停模式控制)
T12 子池路由攻击者多钱包加入相同子池, shares 增长保持公平
T13 极端 idle 子池被 keeper 触发 sync, 不会让攻击者一次性收割大量 transfer
T14 dust 不可被用户直接提取
T15 dormant bucket 激活 OOG 时退化为 lazy activation 路径
```

## 15. 结论

shares 模型的实际实现远比 `pool_equity * shares / total_shares` 一行公式复杂。本文确定了所有边界条件、防稀释、抢跑保护、dormant bucket 实现、子池路由和不变量，作为生产合约工程的强制规范。`05-核心机制与数学模型.md` 中的逐仓表达只用于解释和离线 oracle，合约不在 Position 账户持有那些字段。

任何对本文的偏离都视为破坏白皮书原则的同等可扩展实现，不允许私自实施。

