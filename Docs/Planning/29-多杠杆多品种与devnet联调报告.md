# 29 · 多杠杆 / 多品种上线与 Devnet 联调报告

> 本轮目标:在 MoleOption 上线 5×–500× 多档独立杠杆交易场、参照 Hyperliquid 扩充交易品种、K 线接入成交量(VOL),并打通智能合约升级 + Devnet 多市场 bootstrap + keeper 实时同步,完成本地端到端验证。

---

## 1. 本轮交付概览

| 模块 | 交付内容 | 状态 |
| --- | --- | --- |
| 品种目录 | 30 个标的(15 加密 / 10 传统金融 / 5 外汇),目录驱动架构 | ✅ |
| 杠杆档位 | 5/10/20/50/100/200/500× 七档,按资产类别封顶 | ✅ |
| K 线 | 多周期(1m→1Y)+ 底部 VOL 成交量柱 | ✅ |
| 行情引擎 | 合成行情(价格/24h 涨跌/24h 量)+ 链上实时价覆盖 | ✅ |
| 智能合约 | 新增 `initialize_market_vaults` 指令并部署上链 | ✅ |
| Devnet | 34 个市场全部 bootstrap(market+vaults+subpool+ledger) | ✅ |
| Keeper | 目录驱动多市场同步,launchd 持久化 | ✅ |
| 多语言 | 默认英文(确定性),6 语种 | ✅ |

---

## 2. 杠杆与品种设计

### 2.1 资产类别与最高杠杆

| 类别 | 最高杠杆 | 代表品种 |
| --- | --- | --- |
| Crypto(加密) | 100× | BTC、ETH、SOL、BNB、XRP、DOGE、AVAX、LINK、SUI、HYPE… |
| Tradfi(美股/指数/商品) | 200× | SP500、NAS100、AAPL、TSLA、NVDA、GOLD、SILVER、WTI、SPCX、SKHYNIX |
| FX(外汇) | 500× | EURUSD、GBPUSD、USDJPY、AUDUSD、USDCAD |

- 杠杆档位全集:`[5, 10, 20, 50, 100, 200, 500]`,前端按品种 `maxLeverage` 自动裁剪可选档(如 BTC 只显示 5–100×)。
- 每个 `品种 × 杠杆档` 是一个**独立的链上交易场**(独立 Market/SubPool),命名形如 `BTC-100X`、`EURUSD-500X`。

### 2.2 单一事实来源(catalog)

- `frontend/src/markets/catalog.json` —— 品种、类别、基准价、`quoteDecimals`、杠杆档的唯一定义,**前端与 keeper bootstrap 共用**。
- `frontend/src/markets/catalog.ts` —— 类型与工具函数:`findSymbol` / `tiersFor` / `marketSymbol` / `parseMarketSymbol` / `baseOf` / `formatQuote`。
- `frontend/src/markets/syntheticTicker.ts` —— 基于稳定种子的确定性合成行情(价格/24h 涨跌/24h 量),`useTickers` hook 支持以链上实时价覆盖。

---

## 3. 前端改动

- **`trade/MarketBrowser.tsx`(新增)**:Hyperliquid 风格品种选择弹窗;Tab(全部/Crypto/Tradfi/FX)、搜索、价格/24h 涨跌/24h 量,按量排序,链上已开通的市场带 ● 实时标记。
- **`trade/TradeView.tsx`(大改)**:接入 catalog + 合成行情 + 品种选择器 + 杠杆档选择器;顶部展示 24h 涨跌/24h 量;订单摘要含杠杆、名义敞口、"强平价格:无"(永不爆仓);未上链品种标 "Demo"。
- **`trade/PriceChart.tsx`**:`lightweight-charts` 增加 `addHistogramSeries` 成交量柱,合成量随周期缩放。
- **i18n**:`trade`/`market` 文案键补齐 6 语种;`market` 段新增品种选择器相关键。
- **默认语言英文**:`i18n/index.ts` 检测顺序仅 `localStorage`,无值回退 `en`;本轮将存储键升版为 `mole_lang_v1`,使旧的记忆值失效,所有客户端确定性地以英文启动,用户显式切换后才持久化。

---

## 4. 智能合约

### 4.1 新增指令 `initialize_market_vaults`

- 文件:`programs/mole-option/src/instructions/init.rs`、注册于 `programs/mole-option/src/lib.rs` 的 `#[program]`。
- 作用:为 Market 创建 `vault` 与 `fee_vault` 两个 SPL Token 账户(PDA,owner = `market_vault_authority` PDA)。`initialize_market` 仅记录地址,此前这两个 token 账户从未被创建,导致真实 `open_position` 无法结算抵押 —— 本指令补齐该缺口。

### 4.2 关键事故与修复:链上是错误旧构建

联调时 setup 报 `InstructionFallbackNotFound (101)`。排查发现:

1. 之前一次"升级"部署上传的 `.so` 是**另一个旧构建**,不含 `initialize_market_vaults`(`initialize_market` 这类旧指令能跑、新指令报 101)。
2. 用 `solana program dump` 拉下链上程序与本地 `.so` **比对哈希**确认不一致(链上 `4eb17cf9…` vs 本地正确 `3d736f28…`)。
3. 强制清理 + 重编(`cargo build-sbf`),重新部署本地正确二进制。
4. 部署受 Helius 免费档 `sendTransaction` 限流,`--use-rpc` 路径反复 "Max retries exceeded";**改用 TPU 直发路径(去掉 `--use-rpc`)约 30s 完成 swap**。
5. 顺手用 `solana program close --buffers` 回收一个孤立 buffer 的 **5.25 SOL** 租金。

升级后链上哈希 == 本地 `3d736f28…`(slot 470786857,Data Length 754696),`initialize_market_vaults` 可正常调用。

---

## 5. Devnet Bootstrap(34 市场)

- 脚本:`frontend/scripts/keeper-devnet.mjs`(目录驱动重构)。
- 范围:`MARKET_BASES="SOL,BTC,ETH,SP500,GOLD,EURUSD"` → 6 个品种 / 34 个市场。
- 每市场依次执行:`initialize_market` → `initialize_market_vaults` → `initialize_sub_pool` → 多/空 `DistributionLedger` 初始化。
- 每个品种一个 per-base 价格账户(mock Pythnet v2 预言机)。
- 成功后自动把 34 个市场的 `marketPda` 写入 `frontend/.env.local` 的 `VITE_MARKETS`,前端据此把目录品种映射到链上市场。
- 全程在**公共 devnet RPC**(`api.devnet.solana.com`)完成,约 6.8 分钟。

---

## 6. 关键问题:Node `fetch failed` 根因与修复

**现象**:`node scripts/keeper-devnet.mjs` 在本机(非沙箱)也报 `TypeError: fetch failed`,而 `curl` / Rust 的 solana CLI 均正常。

**根因**:Node 20+ 默认开启的 Happy-Eyeballs(`autoSelectFamily`)在 IPv4/IPv6 竞速建连时失败,导致 undici `fetch` 直接抛错。

**修复**(`keeper-devnet.mjs` 顶部):

```js
dns.setDefaultResultOrder("ipv4first");
if (typeof net.setDefaultAutoSelectFamily === "function") {
  net.setDefaultAutoSelectFamily(false);
}
```

验证:Helius 与公共 RPC 均稳定 `200 OK`,setup/run 全程零 `fetch failed`。

---

## 7. Keeper 持久化

- 守护脚本:`frontend/scripts/keeper-daemon.sh`(补上 `MARKET_BASES` 默认值、间隔改 8000ms)。
- 服务:`~/Library/LaunchAgents/com.moleoption.keeper.plist`(`KeepAlive`),env 注入 `SOLANA_RPC_URL` / `KEEPER_INTERVAL_MS=8000` / `MARKET_BASES`。
- 运行表现:每 8s 为全部 34 市场推 `set_price + sync_pool`,日志 `/tmp/mole/keeper.log` 显示 `tick N <base>: $price → N markets`,零失败。
- 生产侧:团队已在 ECS 用 `systemd`(`mole-keeper-devnet.service`)承载,本地 launchd 仅用于本机预览。

---

## 8. 验证

- `npx tsc --noEmit` ✅ / `npm run build` ✅(373 模块,vite 构建通过)。
- 浏览器(`localhost:5173/app`):
  - 交易页:`BTC-USDC` 正常交易、实时价、24h 涨跌、24h 量、K 线 + VOL、杠杆档随品种封顶、订单摘要"强平价格:无"。
  - 品种选择器:30 市场(15 加密 / 10 传统金融 / 5 外汇),链上 6 品种带 ● 实时标记。
  - 默认语言英文,6 语种可切换。

---

## 9. 关键地址 / 配置(Devnet)

| 项 | 值 |
| --- | --- |
| mole-option Program | `EWqyK5r4MyNaewo2s6nqLmZEVt4Xcx1yNT2dfFnTSfWp` |
| mock-oracle Program | `CLXteYm7SB9BgVmu4kC9GLhKjie9H5UmSs6czaNfcEQq` |
| 部署/运维钱包 | `8eiak6rMfk2mbZZhn6ebBFdzzEMWSBAJd1ZVxaowJAz5` |
| 抵押币(devnet USDC) | `4zMMC9srt5Ri5X14GAgXhaHii3GnPAEERYPJgZJDncDU` |
| 升级后程序哈希 | `3d736f28…`(slot 470786857) |
| 上线市场数 | 34(SOL/BTC/ETH ×5,SP500/GOLD ×6,EURUSD ×7) |

> 注:`VITE_RPC_URL` 含 Helius API key,仅用于本地 devnet 预览;上线前需轮换并改为后端代理,避免随前端包暴露。

---

## 10. 后续建议

1. **扩品种/扩档位**:目录已支持全部 30 品种 × 7 档(170 组合),但 bootstrap 大量市场会被免费 RPC 限流;上线时在 ECS(更好 RPC)分批 provision 其余品种与更高杠杆档。
2. **真实下单回归**:vaults 已就位,接钱包后跑通 `open_position`/`close_position` 端到端(持仓、交易记录、强平价为"无"的验证)。
3. **预言机**:devnet 用 mock-oracle;上线主网需切换到真实 Pyth/Hermes 价源并校准 envelope 校验阈值。
4. **RPC 与密钥**:前端 RPC 走后端代理 + 轮换 key;keeper 用付费 RPC 提升吞吐与稳定性。
