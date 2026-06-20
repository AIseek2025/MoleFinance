# Mole Operations Manual

## 1. 常用命令

```bash
ssh admin@8.218.209.218 'readlink -f /var/www/molefinance/current'
ssh admin@8.218.209.218 'ls -1dt /var/www/molefinance/releases/* | head'
ssh admin@8.218.209.218 'sudo nginx -t && sudo systemctl reload nginx'
```

## 2. 证书相关

查看证书:

```bash
ssh admin@8.218.209.218 'sudo certbot certificates | sed -n "/molefinance.net/,+8p"'
```

续期测试:

```bash
ssh admin@8.218.209.218 'sudo certbot renew --dry-run'
```

## 3. keeper 现状

- 当前首发默认 keeper 继续跑在现有常驻机器
- 若前端页面价格不再更新，优先检查 keeper 是否仍在线
- 迁移 keeper 到 ECS 前，必须确保旧 keeper 已停止

## 4. 站点回滚

```bash
ssh admin@8.218.209.218 '
  PREV=$(ls -1dt /var/www/molefinance/releases/* | sed -n "2p") &&
  sudo ln -sfn "$PREV" /var/www/molefinance/current &&
  readlink -f /var/www/molefinance/current
'
```

## 5. 常见故障

| 现象 | 排查 |
| --- | --- |
| 首页打开是 mock | 检查构建时是否设置 `VITE_DEFAULT_FEED=live` |
| 首页 404 | 检查 `current` 软链与 Nginx `root` |
| HTTPS 失败 | 检查证书路径与 `certbot certificates` |
| 页面能开但数据不动 | 检查 devnet RPC key 和 keeper 常驻机 |
