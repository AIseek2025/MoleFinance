# Mole Deployment System Overview

## 1. 设计原则

- 低风险: 首次 ECS 发布只新增 `molefinance.net` 站点，不改现有 `ibox.love`
- 可回滚: 前端发布采用 `releases/<timestamp>` + `current` 软链
- 可审计: 文档、脚本、验收与发布记录分层沉淀
- 可扩展: 当前是静态前端首发，后续可平滑扩展到 ECS keeper / prober

## 2. 当前部署模式

| 模式 | 用途 | 入口 |
| --- | --- | --- |
| ECS 静态站 | 对外 demo / devnet 体验 | `deploy/scripts/mole-ecs-deploy-ui.sh` |
| 常驻机 keeper | 持续喂价 + sync_pool | 现有 macOS `launchd` |
| ECS keeper 可选增强 | 后续把 keeper 迁到 Linux | `infra/systemd/mole-keeper-devnet.service` |

## 3. 当前架构

```text
Browser
  -> Nginx :443
  -> /var/www/molefinance/current (Vite dist)
  -> frontend connects to Solana devnet RPC directly

Existing keeper host
  -> mock-oracle set_price
  -> mole-option sync_pool

Optional future ECS services
  -> mole-keeper-devnet.service
  -> mole-prober / snapshot serving
```

## 4. 关键约束

- 站点必须指向 `Solana devnet`
- 根站点默认进入 live feed，而不是 mock feed
- `mock-oracle` 只说明 devnet 用法，不能被误写成主网 SOP
- Vite 的 `VITE_*` 变量会编进静态包，RPC key 必须做域名限制与限流

## 5. 已知上线形态

- `mole-option` 程序 ID:
  `EWqyK5r4MyNaewo2s6nqLmZEVt4Xcx1yNT2dfFnTSfWp`
- `mock-oracle` 程序 ID:
  `CLXteYm7SB9BgVmu4kC9GLhKjie9H5UmSs6czaNfcEQq`
- 前端市场 PDA:
  `GTAxg2pzhMAm9h5VtwfYqpojfBDUGvo5zsjzC8ZNxvCL`
- 域名:
  `molefinance.net`
- ECS IP:
  `8.218.209.218`

## 6. 部署边界

- 本文档体系覆盖:
  - ECS 站点初始化
  - HTTPS 证书申请
  - 前端发布
  - 日常运维与验收
- 本文档体系不假装覆盖:
  - Solana 主网发布
  - 多签治理迁移
  - 真实预言机切换
  - vault/fee_vault 主网资产链路
