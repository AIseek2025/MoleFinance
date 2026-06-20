# Mole Ops Handoff Summary

## 1. 当前线上基线

- 域名: `https://molefinance.net`
- ECS: `admin@8.218.209.218`
- 站点目录: `/var/www/molefinance`
- 当前前端 release: `/var/www/molefinance/releases/20260620-160949`
- 链上环境: `Solana devnet`
- 当前市场:
  - `program_id`: `EWqyK5r4MyNaewo2s6nqLmZEVt4Xcx1yNT2dfFnTSfWp`
  - `market_pda`: `GTAxg2pzhMAm9h5VtwfYqpojfBDUGvo5zsjzC8ZNxvCL`
  - `lock_pda`: `5sP17DnbgMd8d9JmvbVSrKV6b2wF6f4CPdgYjdbNfDq6`
  - `expected_leader`: `8eiak6rMfk2mbZZhn6ebBFdzzEMWSBAJd1ZVxaowJAz5`

## 2. 本次补齐的运维资产

- `deploy/` 文档体系
- ECS 静态站发布脚本
- Nginx 站点配置与证书流程
- ECS keeper systemd 模板
- ECS prober systemd 模板
- `markets.devnet.toml`

## 3. keeper 目标形态

- 服务名: `mole-keeper-devnet`
- 工作目录: `/var/www/molefinance/repo/frontend`
- 环境文件: `/etc/mole/mole-keeper-devnet.env`
- 关键秘密:
  - `/etc/mole/keeper-devnet.json`
  - `/etc/mole/price-account.json`

## 4. prober 目标形态

- 服务名: `mole-prober`
- 可执行文件: `/var/www/molefinance/repo/target/release/ops-toolkit`
- 市场注册表: `/etc/mole/markets.toml`
- 输出:
  - `/var/lib/mole/prober.json`
  - `/var/lib/mole/prober.prom`
- 前端读取:
  - `VITE_PROBER_SNAPSHOT_URL=https://molefinance.net/prober.json`

## 5. 常用命令

```bash
ssh admin@8.218.209.218 'sudo systemctl status mole-keeper-devnet --no-pager'
ssh admin@8.218.209.218 'sudo systemctl status mole-prober --no-pager'
ssh admin@8.218.209.218 'sudo journalctl -u mole-keeper-devnet -n 100 --no-pager'
ssh admin@8.218.209.218 'sudo journalctl -u mole-prober -n 100 --no-pager'
ssh admin@8.218.209.218 'cat /var/lib/mole/prober.json | head'
```

## 6. 注意事项

- 这是 devnet 体验环境，不是主网环境
- `mock-oracle` 只能用于 devnet
- 前端包内存在 demo RPC key，后续应改为限域或轮换
- 不要同时运行本机 keeper 与 ECS keeper
