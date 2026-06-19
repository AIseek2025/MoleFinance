# MoleOption 技术选型：Solana + Rust

## 1. 总体技术路线

MoleOption 推荐采用 Solana 作为首发链，Rust 作为链上程序语言，Anchor 作为合约开发框架。原因是协议需要高频低成本交易、稳定的预言机接入、低延迟状态更新和可组合 DeFi 生态。

## 2. 公链选择

### 2.1 首选：Solana

优势：

- 低交易成本，适合频繁开平仓和重置。
- 高吞吐，适合多市场并发交易。
- 账户模型适合把市场、仓位、配置、金库拆成明确账户。
- Pyth 在 Solana 生态成熟。
- DeFi 用户与衍生品用户重合度高。

风险：

- 网络历史上存在拥堵和不稳定。
- 程序账户和交易大小受限。
- 并发写账户冲突需要专门设计。
- Anchor 和 Solana 版本升级可能带来维护成本。

缓解：

- 初期限制市场数量和仓位规模。
- 避免单笔交易遍历大量仓位。
- 使用聚合状态、懒结算、批处理或 keeper 分片。
- 设置市场暂停和价格保护。

## 3. 链上开发语言

### 3.1 Rust

Rust 是 Solana Program 的主流语言，适合开发资金安全要求高的协议。

要求：

- 禁用浮点数，所有金额和价格使用整数定点数。
- 使用 checked arithmetic，避免溢出。
- 所有除法必须明确舍入方向。
- 所有状态转换必须通过不变量测试。

### 3.2 Anchor

推荐使用 Anchor 构建首个真实资金版本。

优势：

- 账户校验简洁。
- IDL 自动生成，方便前端集成。
- 测试工具成熟。
- 错误码、事件、账户初始化开发效率高。

注意：

- 不要过度依赖宏隐藏安全边界。
- 关键数值逻辑应抽离为纯 Rust 函数，便于单元测试和属性测试。
- PDA seeds、bump、authority 规则必须清晰文档化。

## 4. 合约技术栈

推荐：

- `solana-program`
- `anchor-lang`
- `anchor-spl`
- SPL Token / Token-2022 视 USDC 发行选择而定
- Pyth Solana Receiver 或当前 Pyth 推荐 SDK

开发工具：

- Anchor CLI
- Solana CLI
- Rust stable toolchain
- `cargo test`
- `anchor test`
- LiteSVM 或 Mollusk 用于更快的 Solana 程序测试

## 5. 预言机选型

### 5.1 首版：Pyth

Pyth 适合作为首个价格来源：

- Solana 生态支持强。
- 更新频率高。
- 支持置信区间。
- 适合加密资产价格。

首版必须校验：

- 价格账户对应正确标的。
- 价格更新时间未过期。
- 置信区间不超过阈值。
- 指数和定点换算正确。
- 价格为正数。

### 5.2 后续扩展：多源预言机

可增加：

- Chainlink。
- Switchboard。
- 中心化交易所 TWAP 观察服务。

多源逻辑：

- 取有效价格集合。
- 剔除偏离中位数超过阈值的数据源。
- 使用中位数或加权中位数。
- 若有效来源不足，暂停市场。

## 6. 前端技术栈

推荐：

- Next.js + TypeScript。
- React。
- Tailwind CSS。
- Solana Wallet Adapter。
- Anchor TypeScript client。
- TanStack Query 管理链上读请求和索引 API。
- Lightweight Charts 或 TradingView widget 显示价格。

前端重点：

- 明确区分参考盈亏和当前权益。
- 所有风险提示必须在用户操作路径中出现。
- 对预言机延迟、市场暂停、低流动性给出直观解释。

## 7. 索引与后端服务

测试网和真实资金版本必须配套索引服务。前端不能在高并发场景下直接依赖逐个读取链上账户：

- 监听程序事件。
- 同步市场状态、仓位状态、重置历史。
- 提供用户仓位聚合查询。
- 计算前端展示型统计指标。

推荐技术：

- Node.js / TypeScript 或 Rust 后端。
- PostgreSQL。
- Redis 缓存热点市场。
- Helius、Triton、Yellowstone gRPC 或自建 RPC 作为数据源。

注意：

- 索引服务只能用于展示，不得作为用户权益的信任来源。
- 任何可提款金额必须由链上程序计算或验证。

## 8. 测试技术栈

### 8.1 Rust 单元测试

用于测试：

- 数学函数。
- 定点数换算。
- 重置算法。
- `locked_loss` 不可逆与 `realized_profit_balance` 恢复权益的组合模型。
- 舍入策略。
- 不变量。

### 8.2 属性测试

推荐使用：

- `proptest`
- `quickcheck`

重点生成：

- 随机价格路径。
- 随机多空仓位。
- 随机杠杆和本金。
- 极端价格跳变。
- 接近溢出的数值。
- 大规模 epoch 分片统计和懒结算回放。

### 8.3 Solana 程序测试

用于验证：

- PDA 权限。
- SPL Token 转账。
- 开仓和平仓流程。
- 市场暂停。
- 预言机账户校验。

### 8.4 经济仿真

推荐用 Python 或 Rust 写离线模拟器：

- 批量交易者行为模拟。
- 多空不平衡场景。
- 价格路径冲击。
- 恶意用户抢跑和重置操纵模拟。

## 9. 安全工具

推荐：

- `cargo clippy`
- `cargo fmt`
- `cargo audit`
- `cargo deny`
- Anchor security checklist。
- 自定义不变量测试脚本。

审计前准备：

- 合约架构文档。
- 账户权限图。
- 状态机图。
- 数学模型说明。
- 已知风险和假设清单。

## 10. 运维技术栈

### 10.1 Keeper

职责：

- 触发周期性重置。
- 检查市场是否需要暂停。
- 更新 Pyth 价格。
- 广播市场健康状态。

语言：

- TypeScript 或 Rust。

要求：

- 多实例运行。
- 幂等。
- 不持有用户资金权限。
- 只调用公开指令。

### 10.2 监控

指标：

- 市场 TVL。
- 市场总名义价值。
- 预言机价格年龄。
- 预言机置信区间。
- 重置频率。
- 指令失败率。
- 市场暂停次数。
- 金库余额与账面权益差异。

推荐工具：

- Grafana。
- Prometheus。
- Datadog 或自建日志栈。

## 11. 技术选型结论

首个真实资金版本推荐组合：

- 链：Solana Devnet。
- 合约：Rust + Anchor。
- 抵押资产：USDC SPL Token 测试币。
- 预言机：Pyth。
- 前端：Next.js + TypeScript。
- 索引：PostgreSQL 分区表 + Redis 缓存 + 流式链上事件消费。
- 测试：Rust 单元测试 + Anchor 集成测试 + 属性测试 + 离线仿真。

该组合能最大化开发速度，同时保留向生产级协议演进的路径。
