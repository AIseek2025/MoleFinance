# MoleOption 运维手册（Operator Runbook）

本手册面向 7×24 值班 ops 团队。它假设你能看到：

- 主网 RPC + 备用 RPC 端点（每个 region 至少 2 条独立线路）。
- 治理多签终端（Squads / Realms / 自研多签均可）。
- Keeper bot 主机（最少 2 台 hot-standby，跨可用区）。
- 链上事件订阅器（subscribe `mole_option` program 全部 events）。
- Prometheus / Grafana / 告警渠道（PagerDuty + 飞书/Slack 双发）。

> **此手册必须与 §16-合约升级与治理紧急响应、§09-开发 Phase 规划、§22-wave3-protocol-harness 共同阅读**。本手册是“可执行步骤”的具象化，不重复设计意图。

## 1. 角色与权限

| 角色 | 链上 Authority | 钱包 | 多签人数 / 阈值 |
|------|----------------|------|------------------|
| ProgramUpgrade | `set_upgrade_authority` 多签 | hardware wallet 多签 | M=4 / N=7，72h timelock |
| GlobalConfig | `GlobalConfig.admin` | 多签 | M=3 / N=5，24h timelock |
| Market | `Market.admin` | 多签 | M=2 / N=4，1h timelock |
| Emergency | `GlobalConfig.emergency_authority` | 热钱包 + cold backup | M=1 / N=3，立即生效 |
| Keeper | `Market.keeper_authority`（或 keeper-bot 钱包） | 热钱包 | 单签即可 |
| Fee | `GlobalConfig.fee_authority` | 多签 | M=2 / N=3，24h timelock |

> Emergency 是唯一允许「立即生效」的角色，且其权限被严格限制到 `pause_market` / `set_globally_paused` 两个调用点。其他任何变更必须走 timelock。

## 2. 健康度仪表板（每日扫描）

每个值班 shift 开始前必须确认以下 18 项：

### 链上侧（每个市场）

1. `GlobalConfig.paused_globally == false`
2. `Market.paused == false`
3. `Market.frozen_new_position == false`
4. `Market.schema_version == SCHEMA_VERSION_CURRENT`（目前为 `1`）
5. 每个 SubPool 的 `total_open_long_qty + total_open_short_qty` 在过去 24h 内出现过非零值（无人交易 = 协议失效信号）
6. 每个 SubPool 的 `dormant_inventory[Long]` 与 `[Short]` 的 sum tick 数在 0..2_000 之间；若 > 5_000，触发 P1
7. `projected_recovery_outstanding` < 0.1% 全市场名义本金；若 > 1%，触发 P0
8. 每个 DistributionLedger 的 `pending_init_hint` 数 < 50；若 ≥ 50，触发 P2

### Keeper 侧

9. Keeper bot v1 + v2 进程状态 = `running`，最近 60s 内有心跳
10. `KeeperLoopMetrics.failed_actions`（过去 1h）< 5；若 ≥ 5，触发 P1
11. `KeeperLoopMetrics.skipped_actions`（过去 1h）< 100；若 ≥ 100，触发 P2
12. `applied_vol`（过去 1h）非空；连续 3 次 `None` 触发 P2（vol estimator 退化）
13. Keeper bot 钱包余额 ≥ 0.5 SOL（rent + tx fee 准备金）；< 0.2 SOL 触发 P1

### RPC 侧

14. 主 RPC `getSlot` 延迟 < 200ms（p95）
15. 主 RPC 与备用 RPC 的 slot 差 < 5；> 32 触发 P1
16. `getProgramAccounts` 返回时间 < 5s；> 30s 触发 P2

### Pyth / Price 侧

17. 最新价格的 `slot_age` < 30 slots；> 64 触发 P0（清算停滞）
18. `confidence_interval / mid_price` < 0.5%；> 2% 触发 P1

### Keeper-leader-lock 侧（Wave 17 增量，运行细则见 §6.5.x）

19. `keeper_leader_lock_initialized` —— 多副本部署的 lock 已初始化
20. `keeper_leader_lock_freshness` —— 活动 leader 心跳新鲜（未超 takeover 阈值）
21. `keeper_leader_lock_holder_matches_expected` —— 持锁者 == `expected_leader`

### 对账侧（Wave 24 增量，运行细则见 §6.12）

22. `position_principal_drift` —— 链上仓位聚合 notional 与 indexer 报告 notional 漂移 < 0.5%（Pass）/ < 2%（Warn, P2）/ ≥ 2%（Critical, P1）；本周期未跑 open-interest 扫描时跳过（Pass）。

> 第 19–22 项在 `ctx` 缺少对应数据源时返回 `Pass`（disabled），所以单副本 / 未跑探针的部署不会误报。代码侧 `run_all_checks` 共 **22** 项。

## 3. 标准操作流程（SOP）

### 3.1 启动 Keeper Bot

```bash
# 1. 检查钱包余额
solana balance ~/keeper-keypair.json --url $RPC_URL

# 2. 拉起 keeper-bot
cargo run -p keeper-bot --release -- \
    --rpc-url $RPC_URL \
    --commitment confirmed \
    --program-id $MOLE_OPTION_PROGRAM \
    --market $MARKET_PDA \
    --keypair ~/keeper-keypair.json \
    --tick-interval-ms 800 \
    --metrics-port 9099

# 3. 验证 Prometheus 抓到数据
curl -s http://localhost:9099/metrics | grep keeper_loop_outcomes_total
```

`keeper-bot` 是无状态的——重启不会丢失任何业务数据，所有数据从链上重新拉取。重启 = 一次完整的 snapshot 重建。

> **重要**: 切勿同时启动两个 keeper bot 指向同一钱包。两者都会并发签 tx，造成 nonce 冲突和无效 tx 浪费 fee。Hot-standby 模式下 standby 节点必须保持 `--dry-run` 直到主节点失活。

### 3.2 停止 Keeper Bot（计划维护）

```bash
# 优雅关停（SIGTERM 让当前 tick 走完）
kill -TERM $(pgrep -f keeper-bot)
# 确认进程退出
sleep 5 && pgrep -f keeper-bot && echo "FORCE-KILL NEEDED"
```

> 暂停 keeper 不影响交易者：开仓/平仓/索取均可继续。受影响的只是被动数据收集（dormant 桶清理、新桶预初始化）。短时关停（< 1h）几乎无业务影响。

### 3.3 暂停市场（紧急）

```bash
# Emergency authority 可立即调用，无需 timelock
mole-cli pause-market \
    --market $MARKET_PDA \
    --emergency-keypair ~/emergency-keypair.json
```

效果：

- ✅ 所有 `open_position` / `close_position` / `claim_dormant_recovery` 立即返回 `MarketPaused`。
- ✅ Keeper bot 自动进入 idle 状态（snapshot 抛出 `MarketPaused`，KeeperLoop 跳过当 tick）。
- ❌ `pre_sync_dormant_bucket` / `close_dormant_bucket` / `migrate_*` 仍可运行（为了让 ops 在暂停期间继续清理）。

恢复：

```bash
mole-cli resume-market \
    --market $MARKET_PDA \
    --emergency-keypair ~/emergency-keypair.json
```

### 3.4 全局熔断（多市场紧急）

当多个市场同时出现异常（例如 oracle 全部失活、有跨市场漏洞被发现）：

```bash
mole-cli set-globally-paused \
    --paused true \
    --admin-keypair ~/admin-multisig.json  # 需多签
```

`paused_globally == true` 时，所有市场都被视为 `paused`（`Market.paused` 字段被忽略）。

恢复：`--paused false`。

> 全局熔断只能由 Admin authority（多签 + 24h timelock）调用，不能走 Emergency 路径。Emergency 只能影响单个市场。这是设计上的隔离：单市场异常不允许影响全局。

### 3.5 冻结新开仓（计划弃用）

当我们准备弃用某个市场（比如某个标的下架）：

```bash
mole-cli freeze-new-position \
    --market $MARKET_PDA \
    --admin-keypair ~/admin-multisig.json
```

效果：

- ❌ `open_position` 返回 `MarketFrozen`。
- ✅ `close_position` / `claim_dormant_recovery` 仍可调用（让现有持仓退出）。
- ✅ Keeper bot 仍可清理 dormant 桶。

恢复：`mole-cli unfreeze-new-position`。

### 3.6 Schema 升级（重大变更）

> 这是协议生命周期里最重的操作。务必在 staging 全量 dry-run 至少 7 天之后才在主网执行。

升级流程严格按以下顺序：

```text
[T-72h] 多签提案 program upgrade（含新版 binary + 新 schema 字段）
[T-48h] 多签提案 schema bump（bump_market_schema_version v=N → N+1）
[T-24h] 多签提案 migration ix 部署
[T+0h]  ProgramUpgrade timelock 到期 → 执行 upgrade
[T+0.5h] 验证：Market.schema_version == N（旧值），新逻辑兼容旧账户
[T+1h]  执行 bump_market_schema_version → schema_version = N+1
[T+1h+] Keeper bot 自动检测到 SchemaVersionMismatch，立即停止所有 tx 提交
[T+2h]  ops 批量调用 migrate_market + migrate_position 至所有受影响账户
[T+24h] 重启 keeper bot（用新版二进制），SchemaVersionMismatch 解除
```

**Migration 完整 checklist**：

```bash
# 1. 部署新 binary（多签 + timelock）
solana program deploy target/deploy/mole_option.so \
    --upgrade-authority ~/program-upgrade-multisig.json

# 2. 验证新 binary 仍能读懂 v=N 账户
mole-cli sanity-check-snapshot --market $MARKET_PDA
# 期望: schema_version=N, 所有账户解码成功

# 3. Bump schema version
mole-cli bump-market-schema-version \
    --market $MARKET_PDA \
    --new-version $((CURRENT + 1)) \
    --admin-keypair ~/admin-multisig.json

# 4. 关停 keeper（避免它在 mid-migration 提 stale tx）
kill -TERM $(pgrep -f keeper-bot)

# 5. 批量 migrate（事务级幂等，可重入）
mole-cli batch-migrate \
    --market $MARKET_PDA \
    --target-version $((CURRENT + 1)) \
    --batch-size 10 \
    --admin-keypair ~/admin-multisig.json

# 6. 验证
mole-cli sanity-check-snapshot --market $MARKET_PDA
# 期望: 100% 账户 schema_version=N+1

# 7. 启动新版 keeper bot
cargo run -p keeper-bot --release -- ...
```

**关键不变量**：

- `migrate_position` 与 `migrate_market` 必须**幂等**：同一账户 migrate 两次效果与一次相同。这一点在合约层面通过 `position.schema_version == target` 检查保证。
- Migration 期间，**禁止任何用户调 `open_position`**：他们会拿到 v=N+1 的 Market 但还有 v=N 的 Position，需要手动 migrate。可通过 `freeze_new_position` 提前阻断。
- Keeper bot 必须用**新版本二进制**重启。旧版本会因 `SchemaVersionMismatch` 拒绝所有 tx，但比较保险的做法是直接换镜像。

### 3.7 重置 Volatility Estimator

当 vol estimator 显示异常（连续 5 分钟 `applied_vol` 在 [4.5, 5.0] 上限附近，提示样本被异常 spike 污染）：

```bash
# 重启 keeper bot 即可（estimator 是进程内状态，重启清零 + 重新 warm-up）
kill -TERM $(pgrep -f keeper-bot) && sleep 5
cargo run -p keeper-bot --release -- ...
```

Warm-up 期间（默认 32 个样本，约 25 秒）`applied_vol == None`，期间 keeper 用 `predictor` 的默认 σ；这是 wave 10 设计的安全降级路径。

### 3.8 Keeper 钱包补血

```bash
# 当 keeper 钱包 < 0.5 SOL 时
solana transfer \
    --from ~/treasury-multisig.json \
    --to $(solana-keygen pubkey ~/keeper-keypair.json) \
    --amount 1 \
    --commitment confirmed \
    --url $RPC_URL
```

> Treasury 多签转账需要 M=2 签名。建议每月预算 5 SOL 充值给 keeper（按 wave 9 实测 100M 用户场景的 ~3 SOL/月推算 + 50% 余量）。

## 4. 故障应对手册（Incident Playbooks）

### IR-01: Keeper bot 持续报 `Failed`

**症状**：`KeeperLoopMetrics.failed_actions` 在 1h 内 ≥ 10。

**诊断顺序**：

1. 看最近 1 个 `Failed` 的 `reason` 字段：
   - 含 `blockhash not found` → P3，主 RPC slot 落后；切换备用 RPC 即可。
   - 含 `insufficient funds` → 立即给 keeper 充值（§3.8）。
   - 含 `SchemaVersionMismatch` → P1，确认 schema 是否被 bump 但 keeper 没换版本。
   - 含 `MarketPaused` → 这不是错误，keeper 应该已自动跳过。重启 keeper。
   - 含 `RPC response error` → 看具体 code：`-32602` 多半是 binary mismatch，立即停 keeper 排查。

2. 如果 reason 不在上述清单：
   - 抓最近 100 个 tx signature → 用 `solana confirm $sig` 看链上 logs。
   - 在 staging 重放该 tx（`mole-cli replay-tx --sig $sig`）观察是否复现。

### IR-02: `projected_recovery_outstanding` 增长到 1%（P0）

**症状**：长时间未被领取的 dormant 资金累积，意味着大量小户未关心他们的可恢复权益。

**诊断**：

```bash
# 查询所有未领取分布
mole-cli list-pending-recoveries \
    --market $MARKET_PDA \
    --min-amount 1_000_000  # 1 USDC
```

**应对**：

- 短期：发邮件/推送通知用户来领取。
- 中期：上线主动发放工具（admin 触发 `claim_on_behalf_of`，仅在 wave 12+ 计划中）。
- 长期：将 90 天未领取的资金归入 protocol treasury（已通过 wave 9 治理决议）。

### IR-03: Pyth oracle 失活

**症状**：`Market.last_oracle_slot` 与当前 slot 的差 > 64。

**应对**：

1. 立即 `pause-market`（不要等 timelock）。
2. 通知 Pyth 团队 + 同时切换到备用 oracle（如有）。
3. Pyth 恢复后，等 5 个新价格 slot 进入再 `resume-market`，避免 slot=0 被当作正常 slot。

### IR-04: 单个市场出现 share / collateral 不平衡

**症状**：`SubPool.long_collateral != sum(positions.long_collateral)`，invariant 违反。

> 这是最严重的事件。意味着 wave 8/9 的 invariant test 在主网漏报了某个边界。

**应对**：

1. **立即** `set_globally_paused` 全局熔断。
2. 抓取该 SubPool 当前快照（`mole-cli dump-sub-pool > snapshot.json`）。
3. 用 `protocol-harness` 重放最近 1000 个 tx，定位首个偏离点。
4. 不修复算法的话**绝不**`resume`。这意味着至少 24h 不可用。
5. 修复 → staging 全量回放 → 再走 schema migration 流程上线。

### IR-05: `pending_init_hint` 队列堆积

**症状**：某个 DistributionLedger 的 `pending_init_hint.len()` > 50。

**含义**：keeper bot 没在按时初始化新 dormant bucket，新开仓会因为目标 bucket 不存在而失败。

**应对**：

1. 看 keeper bot 是不是 alive。
2. 看 keeper 钱包余额（每个 init_dormant_bucket 大约 0.0023 SOL rent + 0.000005 SOL fee）。
3. 手动触发：`mole-cli init-dormant-bucket --tick X`（绕过 keeper 直接发）。
4. 升级 keeper bot 的 `--max-init-per-tick` 上限（默认 20）。

## 5. 告警阈值表

| 告警 | 触发条件 | 严重度 | 响应 SLA |
|------|----------|--------|----------|
| `KEEPER_DOWN` | 60s 内无心跳 | P1 | 5 min |
| `KEEPER_FAIL_RATE_HIGH` | failed_actions > 5/h | P1 | 15 min |
| `KEEPER_WALLET_LOW` | balance < 0.2 SOL | P1 | 30 min |
| `KEEPER_VOL_DEGRADED` | applied_vol = None × 3 ticks | P2 | 1h |
| `MARKET_PAUSED_UNEXPECTED` | paused 但 ops 未提案 | P0 | 2 min |
| `SCHEMA_MISMATCH` | snapshot 拒绝 tick | P1 | 15 min |
| `RECOVERY_OUTSTANDING_HIGH` | > 1% 名义本金 | P0 | 5 min |
| `RECOVERY_OUTSTANDING_WARN` | > 0.1% 名义本金 | P2 | 1h |
| `DORMANT_PENDING_INIT_HIGH` | pending_init > 50 | P2 | 1h |
| `RPC_LAG` | 主备 RPC slot 差 > 32 | P1 | 15 min |
| `ORACLE_STALE` | last_oracle_slot 差 > 64 | P0 | 2 min |
| `INVARIANT_VIOLATION` | sum 检查失败 | P0 | **立即熔断** |

## 6. 例行任务（Cron）

| 频率 | 任务 |
|------|------|
| 1 min | 抓 KeeperLoopMetrics → Prometheus |
| 5 min | 抓 GlobalConfig + Market 快照 → 持久化 + 比对 invariants |
| 10 min | 抓所有 SubPool / DistributionLedger → 比对 sum invariants |
| 1 hour | 计算 `projected_recovery_outstanding` 累计趋势 |
| 1 day | 清理 90 天未领取的 dormant 资金（wave 12+） |
| 1 week | 统计 keeper bot tx 成本，调拨 treasury 补血 |
| 1 quarter | 完整复现 staging 部署演练 + IR drill（演练 IR-01..IR-05） |

## 6.5 Keeper-leader 锁运维（Wave 15/16/17 新增）

Wave 15 引入了链上 `KeeperLeaderLock` PDA：每个 market 一把锁，所有 keeper-bot 副本透过它共享一个**强排他**的 leader 身份；wave 16 把锁的 RPC reconcile + heartbeat 发布闭环到 bot 主循环；wave 17 把"前端真链上 banner / SIGTERM 优雅 release / ops-toolkit 健康检查 + ts CLI"全部贴上去。这一节给值班同学完整的 init / acquire / release / 故障切换 SOP。

> **Wave 17 要点**：
>
> - 所有 `ts-node ops-toolkit/ts/keeper-leader-*.ts` 引用现在都是真脚本，5 个齐全：`init / show / acquire / heartbeat / release`。第一次用前 `cd ops-toolkit/ts && npm install`；脚本默认是 dry-run（打印 ix bytes 不发 tx），加 `--confirm` 才上链。
> - `keeper-bot` 收到 SIGTERM/SIGINT 时若是 leader 会自动发 `keeper_leader_release` 再退出（`LeaderRpcReconcileConfig.release_on_shutdown=true` 是默认值）。**KL-02 计划性切主可以直接 `systemctl stop`，wave-17 不再需要先手动 release**。
> - `ops-toolkit` Rust 二进制扩了 3 项 keeper-leader 健康检查（`keeper_leader_lock_initialized / _freshness / _holder_matches_expected`），`HealthContext.leader_lock = Some(LeaderLockFacts{…})` 时启用。Prober 启动注入 `expected_leader: Some(<active replica pubkey>)` 即可对"未授权切主"实时告警。
> - Frontend `LeaderLockBanner` 接 `WebSocketFeedAdapter.accountSubscribe(lockPda)` 实时显示 holder / freshness / 剩余阈值；`KeeperPanel.LeaderLockOpsCard` 三个按钮（acquire / heartbeat / release）走 wallet adapter 派发，浏览器直接当 ops console 用。

### 6.5.1 资源模型

| 资产 | 位置 | 说明 |
|------|------|------|
| PDA seeds | `[b"keeper_leader_lock", market.key()]` | 见 `keeper_decoder::ix::KEEPER_LEADER_LOCK_SEED` 与 `keeper-rpc::pda::keeper_leader_lock_seeds` |
| 账户布局 | 8 字节 disc + 49 字节固定 body | `bool has_leader + [u8;32] current_leader + u64 last_heartbeat_slot + u64 takeover_threshold_slots` |
| 默认 takeover 阈值 | `KeeperLeaderLock::DEFAULT_TAKEOVER_THRESHOLD_SLOTS = 75 slots ≈ 30s` | wave 15 默认；wave 17 之前不应改 |
| 4 条 ix | `initialize_keeper_leader_lock / keeper_leader_acquire / keeper_leader_heartbeat / keeper_leader_release` | 全部 permissionless（链上检查 holder/状态） |
| 链上 reject 矩阵 | `programs/mole-option/tests/keeper_leader.rs` | 用 `cargo test -p mole-option --features _keeper_leader_program_test --test keeper_leader` 在 SBF CI runner 上执行 |

### 6.5.2 KL-01 — 上线一个新市场后初始化 lock

> **场景**：Anchor `init_market` 之后，`KeeperLeaderLock` PDA 还没创建。Bot 副本启动时会日志 `keeper-leader-lock PDA not initialised on chain — ops must send …`。

```bash
# 1. 推 init tx（任何能付 rent 的钱包都可，建议直接用 keeper-bot 主热钱包）
ts-node ops-toolkit/ts/keeper-leader-init.ts \
  --rpc "$MOLE_RPC_URL" \
  --program "$MOLE_PROGRAM_ID" \
  --market "$MARKET_PDA" \
  --payer ~/.config/solana/keeper-hot.json
# 2. 验证：lock PDA 存在 + has_leader=false + takeover=75
ts-node ops-toolkit/ts/keeper-leader-show.ts --market "$MARKET_PDA"
# 3. 重启 bot 副本（或等待 reconcile 周期，默认 20 ticks ≈ 16s）
```

### 6.5.3 KL-02 — Bot 副本切主（活动 leader 仍在线）

> **场景**：要把 leader 从 v1 切换到 v2（计划性维护）。前提：v1 的 wallet 仍能签 tx。Wave 17 把 leadership gap 从 wave-15 的 `≈30 s` 降到 `≤ 16 s`（备机 reconcile 周期），常 < 5 s。

```bash
# 1. v1 计划性下线（wave-17 keeper-bot 在 SIGTERM 下若仍是 leader 会自动
#    在退出前发 keeper_leader_release ix，无需手动）
ssh keeper-v1 'systemctl stop mole-keeper-bot.service'
# 2. 验证：show 应当显示 has_leader=false（v1 graceful release 到账）
ts-node ops-toolkit/ts/keeper-leader-show.ts --market "$MARKET_PDA"
# 3. 启动 v2（它的 reconcile 立刻看到 unowned 并发 keeper_leader_heartbeat 拿锁）
ssh keeper-v2 'systemctl start mole-keeper-bot.service'
# 4. 备机 reconcile 周期内（默认 ≤16s）v2 的 Prometheus
#    `mole_keeper_leader_status` 应当 = 1（Leader）
# 5. 如果 v1 graceful release 失败（看 keeper-bot 日志的 warn 行），
#    走 KL-03 路径强抢即可，不用手动 release。

### 6.5.4 KL-03 — Bot 副本切主（活动 leader 已宕机）

> **场景**：v1 故障无法签 release。备用 v2 必须等到 takeover 阈值（默认 ≈30s）后才能用 `keeper_leader_acquire` 强抢。

```bash
# 1. 确认 v1 真的宕机：systemctl + Prometheus + 仪表板心跳
# 2. 让 v2 自然抢占。它的 reconcile 看到 stale (elapsed > 75 slots) 会直接发 acquire
#    人工查看 v2 日志：`leader-lock gate denied` -> `keeper_leader_heartbeat submitted`
journalctl -u mole-keeper-bot -f | grep -E "leader|gate"
# 3. 如果 v2 也没自动抢（reconcile 周期 < 30s 但被网络抖动卡住），手动注入：
ts-node ops-toolkit/ts/keeper-leader-acquire.ts \
  --market "$MARKET_PDA" \
  --keeper ~/.config/solana/keeper-v2.json \
  --observed-slot $(solana slot)
# 4. 验证：show.current_leader == v2.pubkey
```

### 6.5.5 KL-04 — 锁状态卡死（has_leader=true 但 holder 已离线超 N 分钟）

> **场景**：v1 holder 进程仍在跑但 RPC 链路断了，无法 heartbeat；v2 因为某种原因（reconcile 失败 / clock skew）也没自动接管。

1. 先用 `keeper-leader-show.ts` 看 `lastHeartbeatSlot`：
   - 距 `current cluster slot` < 75 slots → 还没 stale，**不要**强抢，先排查 v1 的 RPC（IR-04 的 keeper 章节）。
   - 距 `current cluster slot` > 75 slots → 已 stale，任何副本都可以 acquire。
2. 用 v2 推 `keeper_leader_acquire`（同 KL-03 步骤 3）。如果 ix 报 `KeeperLeaderAcquireWhileFresh`，回到第 1 步重新看 elapsed slot 数；说明 v1 的最新 heartbeat 还在你查看 show 的窗口之后落账。
3. 把 v1 systemd 标记 `failed`，进 IR-04 keeper hot-failover 流程。

### 6.5.6 KL-05 — 治理变更：升级 takeover 阈值

> **场景**：稳定期想从 75 slots（≈30s）放宽到 250 slots（≈100s）以容忍更大网络抖动。**警告**：抬高阈值会延后宕机检测。

1. 设计评审 + 审计签字（属 §16-合约升级与治理 流程，不要单独走）。
2. 部署一个 wave-17 `set_keeper_leader_takeover` ix（暂未上线；如临时需要，走 `program upgrade` 重新初始化锁，**先 release 再 init**，期间所有 bot 副本都进入 standby）。
3. 改完之后，全网 bot 副本必须在下一个 reconcile 周期内显式 reload 配置；建议直接重启。

### 6.5.7 KL-06 — 链上 reject 矩阵（CI / staging 巡检）

每次合约 release 之前，必须在 SBF runner 上跑：

```bash
cargo build-sbf --manifest-path programs/mole-option/Cargo.toml
cargo test --manifest-path programs/mole-option/Cargo.toml \
  --features _keeper_leader_program_test \
  --test keeper_leader -- --nocapture
```

通过条件：
- `happy_path_init_acquire_refresh_release` ≤ 12 000 CU（init）/ ≤ 8 000 CU（hb / release）
- `heartbeat_by_other_while_fresh_rejects` 必须 reject `KeeperLeaderHeldByOther`
- `acquire_while_fresh_self_rejects` 必须 reject `KeeperLeaderAcquireWhileFresh`
- `release_by_non_holder_rejects` 必须 reject `KeeperLeaderNotHolder`
- `heartbeat_with_observed_slot_below_recorded_rejects` 必须 reject `KeeperLeaderClockSkew`

### 6.5.8 前端 LeaderLockBanner（wave 16/17）

`frontend/src/panels/LeaderLockBanner.tsx` 透过 wave-15 wasm `decodeKeeperLeaderLockBytes` 在 console 顶部渲染：

- `uninitialised` — 灰色，显示「ops must send `initialize_keeper_leader_lock`」
- `unowned` — 黄色，显示「no keeper currently holds this lock」
- `fresh` — 绿色，显示 holder pubkey 截断 + 「N slots until stale」
- `stale` — 红色，显示 holder + 「overdue by N slots · standby keeper may acquire」

**Wave 17 接活数据**：`WebSocketFeedAdapter` 在配置了 `VITE_RPC_URL / VITE_MOLE_PROGRAM_ID / VITE_MARKET_PDA` 三件套时自动 `accountSubscribe` lock PDA + 轮询 `getSlot()`，banner 在每次链上更新即时刷新。Mock feed (`?feed=mock`) 路径下 banner 仍走 `uninitialised` truthful state 不报假数据。

**ops 监控建议**：把 console URL 挂在大屏上，与 Prometheus 仪表板互为兜底。Prometheus 的 `mole_keeper_leader_status` 是 server-side gauge；前端 banner 是 client-side decode，二者不一致即说明本地副本与链上状态发散，立即触发 IR-04 排查。

### 6.5.9 KL-07 — 浏览器内手动派发 keeper-leader ix（wave 17）

> **场景**：值班同学在外地、只有 Phantom + 笔记本，需要立刻 `acquire / heartbeat / release`。`KeeperPanel` 顶部 wave-17 起新增"Keeper-leader-lock ops"卡。

1. 把 Phantom 切到对应 market 的 keeper 钱包（一定是带 SOL、有 CU 的那个）。
2. 打开 console URL → 切到 `Keeper` panel。
3. 点对应按钮：
   - **Acquire (force-take stale)**：等价于 `keeper-leader-acquire.ts`；链上 reject 矩阵会 enforce stale 才允许。
   - **Heartbeat (refresh as holder)**：等价于 `keeper-leader-heartbeat.ts`；只有当前 holder 能成功。
   - **Release (planned handoff)**：等价于 `keeper-leader-release.ts`；只有当前 holder 能成功。
4. feedback 行显示 `[wallet] sig=…` 即上链；显示 `[wallet error]` 时点 banner 看链上真实状态做下一步。

### 6.5.10 KL-08 — Ops prober 配置 keeper-leader 健康检查（wave 17）

> **场景**：把链上 `KeeperLeaderLock` 接入 wave-12 的 18-check ops daemon。

1. Prober 启动时拉一次 `getAccountInfo(lockPda)` + `getSlot()`，构造 `LeaderLockFacts`：
   ```rust
   ctx.leader_lock = Some(LeaderLockFacts {
       initialized: true,
       has_leader: lock.has_leader,
       current_leader: lock.current_leader,
       last_heartbeat_slot: lock.last_heartbeat_slot,
       takeover_threshold_slots: lock.takeover_threshold_slots,
       current_slot,
       expected_leader: Some(active_replica_pubkey), // 从 vault 读
   });
   ```
2. `cargo run -p ops-toolkit` 输出 21 个 check（wave-12 18 + wave-17 3）。三个 keeper-leader check 配合 AlertManager 走 P1/P2 队列：
   - `keeper_leader_lock_initialized` Critical → P1 page（KL-01 没跑）
   - `keeper_leader_lock_freshness` Warn (60..=90 %) → P2 ticket；Critical (>=90 % 或无 holder) → P1 page
   - `keeper_leader_lock_holder_matches_expected` Critical → P1 page（"未授权切主"或 KL-04 死锁）；Warn → P3 ticket（让 ops 配 `expected_leader`）
3. `expected_leader` **必须**用 `LeaderLockFacts.expected_leader: Some(_)` 注入，不能 hardcode 进 ops-toolkit 二进制；推荐走 SOPS / vault 加密 → 启动时解密注入环境变量。Wave 18 已经把 prober config 落地（`markets.toml` 内 `expected_leader = "..."` 字段，`scan_all_markets` 自动注入 `LeaderLockFacts`），运维侧只剩"密钥怎么落到 ops VM"——SOPS 是推荐路径，wave 19 会加配套加密 helper。

## 6.6 多市场运维（Wave 18 新增）

> Wave 18 把 KeeperBot / frontend / ops-toolkit 全部升级成多市场原生。下面是与 §6.5 单市场 SOP 并列的 wave-18 路径；单市场运维仍可按 §6.5 KL-01..KL-08 跑。

### 6.6.1 multi-market 配置文件 `markets.toml`

ops VM 上落一份 `/etc/mole/markets.toml`：

```toml
[[markets]]
symbol = "SOL-USD"
program_id = "Mole11111111111111111111111111111111111112"
market_pda = "MktPDA111111111111111111111111111111111111"
# lock_pda 可选；缺省时由 [b"keeper_leader_lock", market_pda] 派生
expected_leader = "KeepHot111111111111111111111111111111111"

[[markets]]
symbol = "BTC-USD"
program_id = "Mole11111111111111111111111111111111111112"
market_pda = "MktPDA222222222222222222222222222222222222"
```

**约束**：symbol 必须 1..16 ASCII 字节，程序内不允许重复；pubkey 必须合法 base58 32-byte。Rust prober 与 TS CLI 解析同一份文件，schema 字节对齐。

### 6.6.2 SOP KL-09 —— 一次性 dump 所有市场状态

> 触发：incident response、值班交接、DR 演练前的状态盘点。

```bash
ts-node ops-toolkit/ts/keeper-leader-show-all.ts \
  --markets /etc/mole/markets.toml \
  --rpc https://api.mainnet-beta.solana.com \
  --human
```

输出：每市场一行 `(symbol, status, holder, expected, elapsed)`，附 worst status。退出码：
- `0` 全部 PASS
- `1` 至少一个 uninitialised / unowned（P3）
- `2` 至少一个 stale（P2）
- `3` 至少一个 mismatch / RPC 错误（P1）

JSON 模式（无 `--human`）适合 cron + AlertManager。可加 `--expected-only` 仅扫描配了 `expected_leader` 的市场。

### 6.6.3 SOP KL-10 —— Rust prober 多市场扫描

```bash
ops-toolkit scan /etc/mole/markets.toml > /var/log/mole/scan.json
```

输出形如 `{worst_exit_code, markets: {SOL-USD: HealthReport, BTC-USD: HealthReport}}`。`worst_exit_code` 与 wave-12..17 单市场 exit code 同语义（0 / 1 warn / 2 P2 / 3 P1 / 4 P0），AlertManager 取它进 paging 决策。**注意**：当前 `scan` mode 仍用内置 demo `HealthContext`；真正集群数据接入 wave-19 prober 守护进程。

### 6.6.4 Frontend `LeaderLockGrid` 启用

ops 编辑 `frontend/.env.production`：

```
VITE_RPC_URL=https://api.mainnet-beta.solana.com
VITE_MARKETS=[{"symbol":"SOL-USD","programId":"Mole...","marketPda":"...","expectedLeader":"KeepHot..."},{"symbol":"BTC-USD","programId":"Mole...","marketPda":"..."}]
```

`npm run build` 编译时把数组烤进 bundle。前端启动后 `App.tsx` 检测 `feed.marketsView` 自动渲染 `LeaderLockGrid`（取代 wave-16 banner）；mismatch 行内徽章 = "运维侧 expected_leader ≠ 当前 holder"，触发条件下立刻人工介入。

### 6.6.5 多市场切主流程（KL-11）

> 触发：某一市场需要主备切换，但其他市场必须保持 leader 不变。

1. **确认目标市场**：`keeper-leader-show-all --markets ... --human`，记录目标市场当前 holder。
2. **目标市场 graceful release**：登录当前 holder 钱包的 keeper-bot，发 `SIGTERM`。Wave-17 graceful release 自动只释放该 keeper-bot 持有的所有市场锁（多市场 keeper-bot 一并释放——这是**已知约束**：多市场切主目前是"全量切"，单市场细粒度切主走 `keeper-leader-release.ts --market <pda>` 手动模式）。
3. **standby 接管**：standby keeper-bot 多市场 reconcile 命中，自动每市场 acquire。
4. **回放确认**：再次 `keeper-leader-show-all`，确认所有市场 holder 都已切换且 status=pass。

### 6.6.6 失败模式与处置

| 故障表现 | 可能原因 | 处置 |
| --- | --- | --- |
| `keeper-leader-show-all` 某市场 status=mismatch | 未授权切主 / `expected_leader` 配置过期 | KL-04 死锁排查 + 检查 `markets.toml` |
| `keeper-leader-show-all` 多市场同时 stale | 全部 keeper-bot 进程死了 / RPC 全断 | 立即 standby 启动 + RPC 故障检查 |
| Frontend grid 某行卡 uninitialised | 该市场 `KeeperLeaderLock` PDA 没初始化 | 跑 KL-01 `keeper-leader-init.ts --market <pda>` |
| Frontend grid 某市场行不出现 | `markets.toml` / `VITE_MARKETS` 漏配 | 添加配置后重启前端 / 重新 build |

## 6.7 多市场用户产品 + prober daemon（Wave 19 新增）

### 6.7.1 SOP KL-12 —— 启动 `ops-toolkit prober` daemon

```bash
# 长跑 daemon（推荐 systemd unit 托管，max_cycles=0 表示无限循环）
ops-toolkit prober \
  /etc/mole/markets.toml \
  /var/lib/node-exporter/textfile/mole_prober.prom \
  /var/lib/mole/prober.json \
  10 \
  0
```

输出契约：

- Prometheus textfile：每条 `mole_health_check_status{name=...,severity=...}` 自动加 `market="<symbol>"` 标签，所以 `node_exporter` 把每市场识别为不同时间序列；顶部 `mole_prober_cycle` / `mole_prober_worst_exit_code` 全局指标。
- JSON 快照：wave-18 `MultiMarketHealthReport` 稳定 wire format（`{"worst_exit_code": N, "markets": {...}}`）。
- 失败语义：fetcher 返回 `Err` → 整个周期不写文件，让 AlertManager 因 textfile gap 触发，**不会**写 stale all-Pass 数据。
- 周期失败 ≠ daemon 退出：daemon 继续下一个周期，AlertManager 看见 `up=0` 即可。

systemd 单元（生产示例）：

```ini
[Service]
ExecStart=/usr/local/bin/ops-toolkit prober /etc/mole/markets.toml \
  /var/lib/node-exporter/textfile/mole_prober.prom \
  /var/lib/mole/prober.json 10 0
Restart=always
RestartSec=5
EnvironmentFile=/etc/mole/prober.env
```

### 6.7.2 SOP KL-13 —— `markets.toml` SOPS 加密管线

明文 `markets.toml` 含 `expected_leader` 是审计敏感字段（一旦泄露，攻击者可针对性发起 `keeper_leader_acquire` 抢主）。Wave 19 在 `markets.toml` 里支持 `${VAR}` 语法，配合 SOPS 解密：

```toml
# /etc/mole/markets.toml （明文可入 git，受 OPA policy 保护）
[[markets]]
symbol = "SOL-USD"
program_id = "Mo1eOpti0nProgram111111111111111111111111111"
market_pda = "MarketSO1USDPda11111111111111111111111111111"
lock_pda   = "LockSO1USDPda1111111111111111111111111111111"
expected_leader = "${EXPECTED_LEADER_SOL_USD}"
```

加载流程：

```bash
# decrypt secrets to env vars (临时 shell 内存，永不落明文盘)
eval "$(sops -d /etc/mole/secrets.enc.env | sed 's/^/export /')"
ops-toolkit prober /etc/mole/markets.toml /var/lib/.../mole.prom /var/lib/.../prober.json
```

验证：`${VAR}` 语法 Rust ↔ TS 字节级一致（13 + 12 测试覆盖），同一份 `markets.toml` 在 keeper-bot / frontend / ops-toolkit/ts CLI 三条链路读出来等价。

### 6.7.3 Frontend 多市场用户产品

- `VITE_MARKETS` 设置后，`MarketSelector` 自动出现在 `LeaderLockGrid` 下方、Tabs 上方。
- 选择持久化：URL `?market=BTC-USD` 优先 → `localStorage["mole.activeMarket"]` 兜底 → 第一个市场默认。
- 切换 active market：`TraderPanel` / `IndexerPanel` / `KeeperPanel` 透明切换到该市场的解码视图（`indexer.market`、子池、干涸桶、`projectedRecoveryOutstandingMicroUsdc` 全部跟随）。
- Freshness dot：每个药丸右侧的小圆点反映 leader heartbeat 状态：绿（fresh）/ 橙（stale）/ 黄（unowned）/ 灰（uninitialised）。

### 6.7.4 失败模式与处置（Wave 19 增量）

| 故障表现 | 可能原因 | 处置 |
| --- | --- | --- |
| `ops-toolkit prober` 日志频繁 "scan error" | `MarketFetcher` 返回 Err（RPC unreachable） | 检查 `solana-client` 连接 + RPC 配置；wave-20 会引入 live RPC 缓存降级 |
| Prometheus textfile 时间戳超过 30s 未更新 | daemon 卡死 / `markets.toml` 损坏 | `journalctl -u mole-prober`；坏配置由 `from_toml_str_with_env` 阻断启动 |
| `parseMarketsToml` 报 `'EXPECTED_LEADER_SOL_USD' is unset` | SOPS 没解密成功 / env var 未导出 | 先跑 `sops -d` + `eval export`，再启动 daemon |
| Frontend selector 看不见某市场 | `VITE_MARKETS` JSON 漏配 / 校验失败 | 浏览器 console 看 `[mole/frontend] VITE_MARKETS parse failed`；用 wave-18 `parseMarketsConfig` 单元测试本地复现 |
| 多市场切换后仓位列表"穿越" | wave-19 `feed.positions` 仍是全局（已知未修） | wave-20 P0：`MarketViewEntry.positions` 按 `marketPdaHex` 过滤 |

## 6.8 多市场仓位过滤 + 真 RPC fetcher + SOPS 管道（Wave 20 新增）

### 6.8.1 多市场仓位过滤生效

切换市场后，`TraderPanel` 仓位列表只显示当前市场的仓位。语义：

- `feed.positions` 上的 `PositionSummary` 现在带可选 `marketPdaHex`（mock 路径已写入；wave-21 单市场 `websocketAdapter` 会接管）。
- `selectActiveMarketSnapshot` 用 `filterPositionsByMarket` 过滤：tagged 不匹配 → 丢弃；untagged → 保留（向下兼容）。
- 同样的 active market 切换路径也驱动 `feed.keeper`：当多市场 keeper-bot 发布 per-market metrics（wave-21）时，`MarketViewEntry.keeperState` 会被 wave-20 路径直接套用；空缺时回落到全局 `feed.keeper`，所以 wave-20 部署不会因为后端 metrics 还没就位而黑屏。

操作员侧的可见变化：换市场后右侧仓位 KPI（数量 / 锁仓总额 / open interest）只统计当前市场。如果 wave-20 mock 看不到这种变化，检查：

1. `VITE_MARKETS` 是否配齐了多市场。
2. `feed.positions[i].marketPdaHex` 是否存在 —— 浏览器 console `console.log(feed.positions[0])` 应能看到 `marketPdaHex`。

### 6.8.2 SOP KL-14 —— SOPS 管线启动 prober

```bash
sops -d markets.enc.toml | ops-toolkit prober \
  --markets-stdin \
  --env-from-file=/run/secrets/prober.env \
  /var/lib/node-exporter/textfile/mole_prober.prom \
  /var/lib/mole/prober.json \
  10 0
```

要点：

- `--markets-stdin` —— 从 stdin 读 `markets.toml`，永不落盘。
- `--env-from-file=PATH` —— 从一个 `KEY=VALUE` 文件读 `${VAR}` 替换值。文件应放 tmpfs（systemd `LoadCredential` 自动是 tmpfs）。文件未覆盖的 key 自动回落到进程 env，所以混用"非密钥 shell env + 密钥 overlay"是 OK 的。
- 文件格式：每行 `KEY=VALUE`，支持 `export KEY=VALUE`、可选 `"value with spaces"`、`#` 注释、空行；不支持反斜线转义。
- systemd unit 推荐：

  ```ini
  [Service]
  ExecStart=/bin/sh -c '/usr/bin/sops -d /etc/mole/markets.enc.toml | \
    /usr/local/bin/ops-toolkit prober --markets-stdin \
      --env-from-file=${CREDENTIALS_DIRECTORY}/prober.env \
      /var/lib/node-exporter/textfile/mole_prober.prom \
      /var/lib/mole/prober.json 10 0'
  LoadCredential=prober.env:/etc/mole/prober.env
  Restart=always
  ```

  `LoadCredential` 把 secret 文件放进 tmpfs `${CREDENTIALS_DIRECTORY}`，单元退出后自动消失。

### 6.8.3 SOP KL-15 —— prober 真 RPC 接入（wave 21 部署预演）

wave-20 引入 `RpcAccountSource` trait 与 `RpcMarketFetcher`。production 装配链路：

```rust
// wave-21 粘合层（沙箱外，需要 `solana-rpc` feature）
struct SolanaRpc { client: solana_client::nonblocking::rpc_client::RpcClient }

impl RpcAccountSource for SolanaRpc {
    fn get_multiple_accounts(&mut self, pubkeys: &[[u8;32]])
        -> Result<Vec<Option<FetchedAccount>>, String> { … }
    fn get_slot(&mut self) -> Result<u64, String> { … }
}

let fetcher = RpcMarketFetcher::new(SolanaRpc { … }, RpcMarketFetcherConfig::default());
let prober = ProberLoop::new(registry, fetcher, StdClock, FileSink { … }, cfg);
```

每周期每市场代价：1 次 `getMultipleAccounts(2)` + 1 次 `getSlot()` = 2 RTT。10 个市场 ≈ 11 RTT（共享 `getSlot` 计数）。`getSlot` 延迟自动写入 `RpcFacts.primary_get_slot_p95_ms`，wave-12 的 RPC 健康 check 直接落地。

### 6.8.4 失败模式与处置（Wave 20 增量）

| 故障表现 | 可能原因 | 处置 |
| --- | --- | --- |
| 切换市场后仓位列表没换 | mock 路径下 `marketPdaHex` 没写 / live 路径 wave-21 未部署 | 检查 `feed.positions[i].marketPdaHex`；如果都是 `undefined`，说明 adapter 还没升级 |
| `--env-from-file` 报 `env-file line N: invalid key` | 文件里有非 `[A-Za-z_][A-Za-z0-9_]*` 的 key（常见：数字开头） | 修复 key 名；语义跟 shell `export` 一致 |
| `--markets-stdin` 但 stdin 是空的 | `sops -d` 失败但管道错误未传播 | 把命令换成 `set -o pipefail; sops -d ... | ops-toolkit ...`；shell 默认丢失左侧错误 |
| prober 启动后立刻退出 exit 2 + `markets parse failed: 'EXPECTED_LEADER_SOL_USD' is unset` | overlay 文件没有该 key + 进程 env 也没有 | 检查 `prober.env` 文件内容；或导出 `EXPECTED_LEADER_SOL_USD` 后再启动 |
| `RpcMarketFetcher` 报 `expected 2 accounts, got N` | RPC 实现不遵守 `getMultipleAccounts` 的"输入输出长度对齐"语义 | 升级 `solana-client`，或把 trait 实现里的填充补齐 |

## 6.9 `OnchainPosition` 解码器 + 真 RPC 适配器 + per-market JSON 指标（Wave 21 新增）

### 6.9.1 SOP KL-16 —— 启用 `solana-rpc` feature 跑真 prober

```bash
# 编译（需要 solana-client 工具链；沙箱 CI 只做 compile smoke）
cargo build -p ops-toolkit --features solana-rpc --release

# 生产 prober（示意 —— 粘合层在 wave-21 是 library API，wave-22 会 ship CLI flag）
# RpcMarketFetcher::new(
#   SolanaRpcAccountSource::new(RPC_URL, CommitmentConfig::confirmed()),
#   RpcMarketFetcherConfig { retry_attempts: 2, retry_backoff_ms: 500, .. },
# )
# .with_backup(SolanaRpcAccountSource::new(BACKUP_RPC_URL, …))
```

要点：

- 默认 `cargo build -p ops-toolkit` **不**拉 `solana-client`；生产镜像用 `--features solana-rpc` 构建。
- `retry_attempts = 0`（默认）与 wave-20 行为一致；生产建议 `retry_attempts = 2`，`retry_backoff_ms = 500`。
- 备用 RPC 只用于 `getSlot` 采样 → `primary_backup_slot_diff`；**永远**不用备用读账户。AlertManager 规则 `RPC_PRIMARY_BACKUP_SLOT_LAG` 在 wave-21 起才有真实数据。

### 6.9.2 SOP KL-17 —— keeper-bot `/metrics-multi` 给前端喂 per-market 指标

```bash
# 多市场 keeper-bot 启动时改用新 API（wave-12 单市场仍用 spawn_metrics_server）
spawn_metrics_server_with_multi(
  "0.0.0.0:9090",
  &global_metrics,
  Some(|| registry.render_per_market_json()),
  &shutdown,
)
```

- `GET /metrics` —— wave-12 Prometheus 文本，不变。
- `GET /metrics-multi` —— JSON 数组 `[{ "market": "SOL-USD", "metrics": { "ticksTotal": 42, … } }, …]`。
- 前端 wave-22 会 poll `/metrics-multi` 填充 `MarketViewEntry.keeperState`；wave-21 只交付 JSON 契约与 HTTP 路由。
- 没接 provider 时 `/metrics-multi` 返回 404 —— 单市场部署零影响。

### 6.9.3 `OnchainPosition` 解码器（wave-22 已接入 live adapter）

wave-22 把 wave-21 解码器接到 `websocketAdapter` / `multiMarketAdapter`：

1. live websocket 路径现在填充 `feed.positions[i].marketPdaHex` —— 来自链上 `Position.market`。
2. 切换市场后 trader 面板仓位列表不再"穿越"（配合 wave-20 `selectActiveMarketSnapshot`）。
3. Closed 仓位（`status === 2`）自动从 feed 剔除。

若仍看到穿越行为，检查：

- Position PDA 是否被 program-account 订阅收到（discriminator 匹配）。
- `decodeOnchainPosition` 手动验证：`decodeOnchainPosition(bytes)` 在浏览器 console。
- `verify-schema-parity.sh` 103/103 确认 Rust ↔ TS ↔ SCHEMA-MAPPING 对齐。

### 6.9.4 失败模式与处置（Wave 21 增量）

| 故障表现 | 可能原因 | 处置 |
| --- | --- | --- |
| `primary_backup_slot_diff` 永远 0 | 没调 `with_backup` / 备用 RPC 故障被折叠为 0 | 确认 prober 装配了 backup source；单独 curl 备用 `getSlot` |
| prober 单次 RPC 抖动就 fail-closed | `retry_attempts = 0`（默认） | 设 `retry_attempts = 2`，`retry_backoff_ms = 500` |
| `/metrics-multi` 404 | 单市场部署 / 没接 `spawn_metrics_server_with_multi` | 预期行为；多市场才需要此路由 |
| `verify-schema-parity.sh` 失败 | `OnchainPosition` 字段漂移 | 同步 `SCHEMA-MAPPING.md` + TS `SCHEMA_DESCRIPTOR` |
| `cargo build --features solana-rpc` 失败 | 沙箱无 Solana 工具链 | 自托管 runner 或 compile-only smoke job（wave-23 CI 通道） |

## 6.10 live position 解码 + 前端 `/metrics-multi` 轮询 + `serve-multi` daemon（Wave 22 新增）

### 6.10.1 SOP KL-18 —— 启动多市场 keeper-bot 并暴露 `/metrics-multi`

```bash
# 编译
cargo build -p keeper-bot --release

# 多市场 daemon（mock fetcher 路径 —— devnet 真 RPC 见 wave-23）
cargo run -p keeper-bot -- serve-multi 0.0.0.0:9099 ./markets.toml

# 可选：限制 tick 次数（smoke / CI）
cargo run -p keeper-bot -- serve-multi 0.0.0.0:9099 ./markets.toml 50
```

- `GET /metrics` —— wave-12 Prometheus 文本，不变。
- `GET /metrics-multi` —— JSON 数组 `[{ "market": "SOL-USD", "metrics": { … } }, …]`。
- SIGINT/SIGTERM 触发 graceful shutdown（与 wave-12 `serve` 一致）。

### 6.10.2 SOP KL-19 —— 前端启用 per-market keeper 指标

```bash
# frontend/.env.local（或部署环境变量）
VITE_KEEPER_METRICS_URL=http://127.0.0.1:9099
```

- 前端每 4 s 轮询 `{VITE_KEEPER_METRICS_URL}/metrics-multi`。
- 解析结果写入 `MarketViewEntry.keeperState`；`selectActiveMarketSnapshot` 把 active market 的 keeper 状态提升到 `feed.keeper`。
- **未设 env var** → hook 静默，行为与 wave-21 完全一致（mock/offline dev 零配置）。

### 6.10.3 SOP KL-20 —— live websocket 仓位 `marketPdaHex` 验证

1. 启动 live feed：`?feed=live` + websocket RPC endpoint。
2. 打开 trader 面板，切换市场 —— 仓位列表应只显示当前市场（tagged positions）。
3. 浏览器 DevTools → 检查 `feed.positions[0].marketPdaHex` 是否为 64-char hex（非 `undefined`）。
4. 平仓后确认仓位行消失（Closed status 剔除）。

### 6.10.4 失败模式与处置（Wave 22 增量）

| 故障表现 | 可能原因 | 处置 |
| --- | --- | --- |
| 切市场后仓位仍穿越 | live websocket 未 decode Position / discriminator 不匹配 | 确认 program-account 订阅；检查 `MoleAccountDiscriminators.position` |
| Keeper Console 仍显示全局指标 | `VITE_KEEPER_METRICS_URL` 未设或 `/metrics-multi` 404 | 设 env var；确认 `serve-multi` 而非单市场 `serve` |
| `/metrics-multi` poll 失败（console warn） | CORS / 网络 / keeper-bot 未启动 | curl `http://127.0.0.1:9099/metrics-multi`；检查 bind addr |
| `serve-multi` 启动失败 | TOML 语法 / 市场 registry 校验 | 先用 `ops-toolkit scan ./markets.toml` 验证 |
| typecheck 失败 `useKeeperMetricsMulti` | import 路径错误 | 确认 `./types` + `./feed/keeperMetricsMulti` |

## 6.11 持仓敞口（open-interest）聚合（Wave 23 新增）

### 6.11.1 SOP KL-21 —— prober 侧扫描 open-interest

```rust
// 生产 prober（solana-rpc feature）：用 SolanaRpcAccountFetcher 扫全程序仓位
// let fetcher = SolanaRpcAccountFetcher::new(RPC_URL, CommitmentConfig::confirmed());
// let oi = ops_toolkit::fetch_open_interest(&fetcher, &PROGRAM_ID)?;
// tracing::info!(long = oi.long_count, short = oi.short_count,
//                principal = %oi.total_principal(),
//                skew = oi.net_notional_imbalance());
```

要点：

- `fetch_open_interest` 走 `Position` 8 字节 discriminator 的 `getProgramAccounts` memcmp（offset 0）。
- 关闭仓位（`status == 2`）自动剔除；`decode_failures > 0` 说明 discriminator 过滤放进了非 `Position` 账户（schema 漂移）→ 立即排查。
- 默认 build 用 `MockAccountFetcher` 即可在 CI 跑全链路；生产镜像 `--features solana-rpc`。

### 6.11.2 前端 "Market open interest" 卡片

- TraderPanel 顶部新增敞口卡片：live 仓位数（多/空拆分）、多/空抵押、多/空 qty、净偏斜（net skew）。
- 数据来自 wave-22 已 live 的 `feed.positions`（已按 active market 过滤）。
- 单市场 mock / offline dev 下也工作（mock 仓位写了 `marketPdaHex`）。

### 6.11.3 失败模式与处置（Wave 23 增量）

| 故障表现 | 可能原因 | 处置 |
| --- | --- | --- |
| `OpenInterestFacts.decode_failures > 0` | Position schema 漂移 / discriminator 撞车 | 核对 `account_discriminator("Position")`；跑 `verify-schema-parity.sh` |
| 敞口卡片全 0 但有仓位 | `feed.positions` 未 live（mock 未写 tag / websocket 未解码） | 见 §6.10.4「切市场后仓位仍穿越」排查 |
| 净偏斜符号反了 | 期望 `long − short`，正=净多 | 确认 `netCollateralImbalance` / `net_notional_imbalance` 语义 |

## 6.12 链上 ↔ indexer 本金/名义对账（Wave 24 新增）

### 6.12.1 SOP KL-22 —— prober 漂移检查接入

```rust
// 每个 prober cycle：扫 open-interest → 折进 PoolFacts → 跑 22 项检查
// let oi = ops_toolkit::fetch_open_interest(&fetcher, &PROGRAM_ID)?;
// ops_toolkit::apply_open_interest_to_pool(&mut ctx.pool, &oi);
// let report = ops_toolkit::run_all_checks(&ctx);
// // report 含第 22 项 position_principal_drift
```

要点：

- `position_principal_drift` 比对 `PoolFacts.onchain_position_notional_micro_usdc`（链上真值）vs `total_notional_micro_usdc`（indexer 报告）。
- 漂移阈值：Pass < 0.5%、Warn(P2) < 2%、Critical(P1) ≥ 2%。
- **未调 `apply_open_interest_to_pool`（onchain == 0）→ 检查跳过（Pass）**；单源 prober 不会误报。
- Critical 漂移 = indexer 与链上对 live 敞口认知分歧 → 立即排查 indexer sync 滞后 / 漏 event。

### 6.12.2 前端 "Indexer reconciliation" 徽章

- TraderPanel open-interest 卡片新增对账徽章：live 仓位抵押和（链上）vs sub-pool 报告抵押和（indexer）。
- `ok`（绿）/ `warn`（黄）/ `critical`（红）/ `disabled`（灰，无 live 仓位）。
- 前端走 collateral（principal），后端走 notional —— 各自 apples-to-apples。

### 6.12.3 失败模式与处置（Wave 24 增量）

| 故障表现 | 可能原因 | 处置 |
| --- | --- | --- |
| `position_principal_drift` Critical | indexer sync 滞后 / 漏 EngineEvent / 链上有未索引仓位 | 对比 `fetch_open_interest` 输出与 indexer 账目；重放 indexer |
| 检查一直 Pass(disabled) | prober 未调 `apply_open_interest_to_pool` | 生产 prober 接入 §6.12.1 |
| 前端徽章常驻 `disabled` | `feed.positions` 无 live 仓位（mock 未写 / websocket 未解码） | 见 §6.10.4 / §6.11.3 |
| 前后端漂移值不一致 | 前端比 collateral、后端比 notional（预期） | 二者意图相同，数值口径不同，不需对齐 |

## 7. 应急联系

| 角色 | 主联系 | 备用 | PagerDuty 队列 |
|------|--------|------|----------------|
| On-call ops | 当班 1 名 | 跨班 1 名 | `mole-ops-primary` |
| Protocol engineer | 主作者 | 副作者 | `mole-eng-secondary` |
| Multisig signer (Emergency) | 3 名（异地） | — | 直拨电话清单见 `AUTHORITY-CONTACTS.md`（红色文件） |
| Multisig signer (Admin) | 5 名 | — | 同上 |

## 8. 不要做的事

- ❌ **不要**用 keeper-bot 钱包签 admin tx。它的私钥放在热钱包，如果泄露需要立即换。
- ❌ **不要**在 schema bump 之后立刻 resume market 而不 migrate。会让用户的 open_position 全失败。
- ❌ **不要**手动同时多次 `pre_sync_dormant_bucket` 同一个桶——keeper 已经 dedupe 了，重复手动操作只会浪费 fee。
- ❌ **不要**禁掉 invariant assertion alerts。它们是协议安全的最后一道线。
- ❌ **不要**在没有 incident report 草稿的情况下 resume 市场。事后补 report 难度极大。

---

> 本手册随 wave 11 一起 ship。Wave 12 会增加：on-call 自动化（半自动 IR-01..IR-05）+ 链上 events → CRM 集成。
