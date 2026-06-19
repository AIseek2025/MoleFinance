# MoleOption 开发规划文档索引

本文档集基于 `Docs/MoleOption.md` 白皮书整理，用于把 MoleOption 从概念设计推进到 Solana/Rust 工程实现。

## 文档结构

1. `01-项目总纲.md`  
   项目定位、目标、原则、边界、里程碑和成功指标。

2. `02-产品PRD.md`  
   用户画像、核心流程、功能需求、非功能需求、前端展示和风险提示。

3. `03-技术选型-Solana-Rust.md`  
   链、语言、框架、预言机、索引、前端、测试、安全与运维技术栈。

4. `04-底层系统架构.md`  
   账户模型、程序边界、数据流、交易流、系统组件和可扩展架构。

5. `05-核心机制与数学模型.md`  
   双层盈亏、全局重置、权益映射、流动性兑现率和关键不变量。

6. `06-清算引擎设计.md`  
   重置算法、结算流程、精度处理、状态更新、异常场景和伪代码。

7. `07-智能合约设计.md`  
   Solana Program 模块、账户结构、指令接口、权限、事件和安全约束。

8. `08-杠杆交易场与风控设计.md`  
   杠杆隔离、市场参数、风险暂停、预言机防护、费用与保险基金。

9. `09-开发Phase规划.md`  
   从研究验证、生产级合约、测试网到主网发布的分阶段交付计划。

10. `10-测试验证与安全审计.md`  
    单元测试、属性测试、仿真、经济攻击测试、审计清单和上线门槛。

11. `11-工程规范与开发指引.md`  
    代码组织、Rust/Anchor 规范、数值精度、提交质量、CI 和文档维护。

12. `12-高并发应对方案.md`  
    亿级用户、分片清算、epoch 最终化、懒结算、索引与运维扩展方案。

13. `13-前后端高并发架构.md`  
    面向千万到亿级用户的前端、后端、索引、缓存、实时推送和降级方案。

14. `14-当前区块即时清算与简化模型评估.md`  
    定义“方向权益池 + shares + recovery/dormant bucket”作为大规模链上实时结算主模型。

15. `15-总设计师审计与漏洞清单.md`  
    第一轮 + 第二轮总设计师审计与黑客攻击面推演。锁定 shares 主模型，列出全部高危问题与修复方案。

16. `16-合约升级与治理紧急响应.md`  
    权限分级、时间锁分级、紧急暂停、状态版本化、迁移、回滚、多签操作规范与升级演练要求。

17. `17-合规与地理屏蔽.md`  
    地理屏蔽、钱包风控、KYC 入口、风险揭示、营销规范、隐私与税务，定义官方前端的合规边界。

18. `18-shares模型实现细则与边界条件.md`  
    Market / SubPool / DormantBucket / Position 字段、sync_pool 公式、shares 铸造防稀释、`dilution_safety_bps`、PDA 路由、不变量、边界测试。生产合约必须以本文为准。

19. `19-第二轮全面审计与漏洞总结.md`  
    第二轮全面审计的总结输出，整合所有新发现的规划漏洞与攻击面，并给出修补方案与上线门槛。

20. `20-攻坚开发进度与里程碑.md`  
    底层 Rust 工作区当前实现进度（molemath / clearing-core / simulation / indexer / pyth-adapter）、各 crate 的测试覆盖、已锁定的关键工程决策，以及下一波攻坚的优先级清单。

21. `21-Dormant存储与CU预算.md`  
    把 `clearing-core::DormantStore` 的 Eager / Lazy 两种模式翻译成 Solana CU 数字，给出"K 个被激活 bucket 在不同 CU 预算下的容量上限"矩阵；列出 `Market` 中所有 dormant 相关治理参数与紧急上限；标记 host 实现 vs on-chain 实现的差距与 Wave 4+ 的对接清单。

22. `22-wave3-protocol-harness.md`  
    第三波硬核开发：新增 `crates/protocol-harness` 端到端仿真器，把 clearing-core / indexer / pyth-adapter 三个组件首次缝合到同一个状态机里，跨 100+ trader × 多 sub_pool × 1000+ op 的随机/对抗负载下逐步验证资金守恒、vault 分解、indexer 平价、对抗路径拒绝。揪出 indexer 在 rotation+claim+多仓 bucket 复合路径下系统性低估 0.5%-1.5% 的真实漏洞（链上正确性不受影响），并把它锁进 known-issue 测试，作为 Wave 4 首要修复目标。

23. `23-on-chain-dormant-bridge.md`  
    第五波硬核开发：定位并解决 `programs/mole-option/src/instructions/sync.rs` 把 `DormantStore` 整条数据通路丢弃的 production blocker（wave 1-4 全部测试覆盖不到）。新增 `crates/chain-mirror` 在 host 上完整复刻 Anchor 多账户运行时（每个 `#[account]` 一个独立宿主结构 + remaining_accounts 模型 + Solana tx-revert 语义），把 `clearing_core::pack_dormant_store` / `unpack_dormant_store` 嵌进每条指令的 read-engine-write 流水线，再用 `protocol_harness::Harness` 作 oracle 做 byte-equal 平价测试（eager + lazy + stress 三套参数 × 4400 个随机 op）。给出 wave 6 Anchor 指令落地的账户列表契约。同时记录在 lazy mode 下抓到的 `DormantStore::distribute_lazy` pending allocation 留账问题及 wave 5.5 修复方案。

## 当前推荐路线

MoleOption 可以先用离线模拟器和测试网环境验证数学模型，但真实资金智能合约不能设计成“后续再替换”的临时合约。首个真实资金版本必须采用 `14 / 18` 中固化的“方向权益池 + shares + recovery/dormant bucket + 子池分片”模型，并在主网部署当天满足 `16 / 17` 中的治理与合规要求：

- 标的：BTC/USD 或 SOL/USD。
- 抵押资产：USDC。
- 杠杆场：10x 或 20x。
- 预言机：Pyth（Pyth program ID 校验 + 价格保护参数）。
- 核心动作：初始化市场、`sync_pool`、开仓（带价格保护）、平仓（当前区块即时领取）、强制关闭 0 价值仓位、领取 dormant 恢复价值、harvest dust。
- 验证重点：系统资金守恒、用户最大亏损不超过本金、盈利分配不超过亏损方可兑现损失、零权益仓位在价格反转和新亏损流动性出现后可恢复权益、shares 模型不稀释、抢跑保护、子池路由不可滥用、合约升级与紧急暂停演练通过、合规屏蔽 QA 通过。

通过离线仿真、测试网压力测试、外部审计与合规审定后，再以限额方式开放真实资金；合约账户结构和结算机制不能依赖后续高风险迁移。
