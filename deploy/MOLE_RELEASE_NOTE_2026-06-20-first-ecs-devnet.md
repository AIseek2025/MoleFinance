# Mole Release Note - 2026-06-20 - First ECS Devnet Launch

- 环境: ECS devnet demo
- 域名: `https://molefinance.net`
- ECS: `admin@8.218.209.218`
- 当前 release: `/var/www/molefinance/releases/20260620-160949`
- 证书: Let's Encrypt，覆盖 `molefinance.net` / `www.molefinance.net`
- keeper 承载: 现有常驻机

## 本次完成

- 新建 `Mole` ECS 部署文档体系
- 初始化 ECS 站点目录 `/var/www/molefinance`
- 安装 bootstrap Nginx 配置并签发 HTTPS 证书
- 切换正式 `molefinance.net` HTTPS 配置
- 发布前端静态站到 ECS
- 修复浏览器端 `Buffer is not defined`
- 修复 live adapter 首帧不拉取 market 导致的长期 offline
- 使用现有 devnet Helius RPC 完成可用的 live demo 发布
- 把 devnet keeper 从本机 `launchd` 迁移到 ECS `systemd`
- 启动 `mole-prober`，对外暴露 `https://molefinance.net/prober.json`
- 前端接入 `VITE_PROBER_SNAPSHOT_URL`，首页显示 `PROBER HEALTH`

## 验收结果

- `https://molefinance.net` 返回 `200`
- `https://www.molefinance.net` `301` 跳转到主域
- 浏览器首页渲染 `MoleOption Console`
- 浏览器首页显示 `WAVE-12 LIVE · CONNECTED`
- 浏览器首页显示 `PROBER HEALTH`
- ECS `mole-keeper-devnet` 连续 tick 正常
- ECS `mole-prober` 周期性输出 `prober.json` 与 `prober.prom`

## 风险提示

- 当前前端包内使用的是 devnet demo RPC key，后续应补域名限制或轮换
- 这仍然是 `Solana devnet` 体验站，不是主网正式环境
- 当前健康面板接的是 `prober.json`；`VITE_KEEPER_METRICS_URL` 仍未启用，因为活跃 keeper 仍是 Node 守护脚本，不带 `/metrics-multi` 端点
