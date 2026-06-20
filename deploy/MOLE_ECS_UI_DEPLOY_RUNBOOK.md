# Mole ECS UI Deploy Runbook

## 1. 适用范围

- 仅发布 `frontend/dist`
- 不触碰链上程序
- 不切换 keeper 运行位置
- 适用于当前 `Solana devnet` 体验版

## 2. 发布命令

```bash
cp frontend/.env.production.example frontend/.env.production.local
# 填入实际 devnet RPC key

bash deploy/scripts/mole-ecs-preflight-check.sh
bash deploy/scripts/mole-ecs-deploy-ui.sh
bash deploy/scripts/mole-ecs-smoke.sh
```

## 3. 脚本做了什么

### `mole-ecs-deploy-ui.sh`

1. 若本地没有 `frontend/dist`，自动执行 `npm ci` 和 `npm run build`
2. 在 ECS 创建新的 release 目录
3. 用 `rsync` 上传新的静态资源
4. 将 `/var/www/molefinance/current` 原子切换到新 release
5. 输出当前 release 路径供回滚使用

## 4. 回滚方式

查看历史 release:

```bash
ssh admin@8.218.209.218 'ls -1dt /var/www/molefinance/releases/* | head'
```

回滚到上一版:

```bash
ssh admin@8.218.209.218 '
  PREV=$(ls -1dt /var/www/molefinance/releases/* | sed -n "2p") &&
  sudo ln -sfn "$PREV" /var/www/molefinance/current &&
  readlink -f /var/www/molefinance/current
'
```

## 5. 发布后要确认

- `https://molefinance.net` 返回 `200`
- 浏览器直接打开首页即为 live feed 基线
- 页面能显示 `MoleOption Console`
- 证书主题为 `molefinance.net`

## 6. 当前不做的事

- 不把 `mock-oracle` / `mole-option` 重新部署到 ECS
- 不把现有 macOS keeper 强制迁移到 Linux
- 不启用主网模式
