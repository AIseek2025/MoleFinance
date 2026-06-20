# Mole ECS Deploy Production README

## 1. 目标

把 `Mole` 的测试网前端正式发布到 ECS：

- 域名: `molefinance.net`
- 证书: Let's Encrypt
- 形态: Nginx 托管静态站
- 链上环境: `Solana devnet`

## 2. 本次上线结论

- 可以现在上线的是“测试网体验版”
- keeper 可继续使用现有常驻机
- 本次不做主网治理、多签和真实预言机切换

## 3. 目录规划

```text
/var/www/molefinance/
  certbot/
  releases/
  current -> /var/www/molefinance/releases/<timestamp>
  repo/
```

## 4. 首次初始化

### 4.1 上传仓库

```bash
ssh admin@8.218.209.218 'mkdir -p /var/www/molefinance/repo'
rsync -az --delete \
  --exclude '.git' \
  --exclude 'target' \
  --exclude 'frontend/dist' \
  ./ admin@8.218.209.218:/var/www/molefinance/repo/
```

### 4.2 执行初始化脚本

```bash
cd /Users/surferboy/MoleOption
bash infra/scripts/setup-server.sh
```

该脚本会:

- 创建 `/var/www/molefinance` 目录结构
- 安装 bootstrap Nginx 配置
- 校验并 reload Nginx
- 为后续证书签发准备 ACME challenge 目录

### 4.3 申请 HTTPS 证书

```bash
ssh admin@8.218.209.218 '
  sudo certbot certonly --webroot \
    -w /var/www/molefinance/certbot \
    -d molefinance.net \
    -d www.molefinance.net
'
```

### 4.4 切换到 HTTPS 配置

```bash
ssh admin@8.218.209.218 '
  sudo cp /var/www/molefinance/repo/infra/nginx/molefinance.net.conf \
    /etc/nginx/conf.d/molefinance.net.conf &&
  sudo nginx -t &&
  sudo systemctl reload nginx
'
```

## 5. 本地构建配置

```bash
cp frontend/.env.production.example frontend/.env.production.local
```

至少要填:

- `VITE_DEFAULT_FEED=live`
- `VITE_RPC_URL=<受限 devnet RPC>`
- `VITE_MOLE_PROGRAM_ID=EWqyK5r4MyNaewo2s6nqLmZEVt4Xcx1yNT2dfFnTSfWp`
- `VITE_MARKET_PDA=GTAxg2pzhMAm9h5VtwfYqpojfBDUGvo5zsjzC8ZNxvCL`

## 6. 发布前端

```bash
bash deploy/scripts/mole-ecs-preflight-check.sh
bash deploy/scripts/mole-ecs-deploy-ui.sh
bash deploy/scripts/mole-ecs-smoke.sh
```

## 7. keeper 策略

### 7.1 当前推荐

- 保持现有 macOS `launchd` keeper 继续运行
- ECS 只承载站点与 HTTPS

### 7.2 可选迁移到 ECS

- 参考 `infra/systemd/mole-keeper-devnet.service`
- 需要额外准备:
  - `SOLANA_WALLET`
  - `SOLANA_RPC_URL`
  - `MOLE_PROGRAM_ID`
  - `MOCK_ORACLE_PROGRAM`
- 迁移前必须先停掉旧 keeper，避免双实例并发

## 8. 禁止事项

- 禁止把本文档当作 Solana 主网发布 SOP
- 禁止把 `mock-oracle` 作为主网预言机
- 禁止把不受限 RPC key 直接打进前端构建
