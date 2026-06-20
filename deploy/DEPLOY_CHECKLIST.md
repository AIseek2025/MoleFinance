# Mole Deploy Checklist

## 发布前

- [ ] `git status` 干净
- [ ] `frontend/.env.production.local` 已配置
- [ ] `VITE_DEFAULT_FEED=live`
- [ ] `VITE_RPC_URL` 使用受限 devnet RPC key
- [ ] `VITE_MOLE_PROGRAM_ID` 为 devnet 程序 ID
- [ ] `VITE_MARKET_PDA` 为当前 devnet 市场 PDA
- [ ] `molefinance.net` / `www.molefinance.net` 已解析到 `8.218.209.218`
- [ ] ECS 上 `nginx` / `certbot` 可用
- [ ] 现有 keeper 正在正常运行

## 首次部署额外项

- [ ] `/var/www/molefinance/repo` 已上传仓库
- [ ] `infra/scripts/setup-server.sh` 已执行
- [ ] Let's Encrypt 证书已申请
- [ ] `/etc/nginx/conf.d/molefinance.net.conf` 已启用 HTTPS 配置

## 发布执行

- [ ] `bash deploy/scripts/mole-ecs-preflight-check.sh`
- [ ] `bash deploy/scripts/mole-ecs-deploy-ui.sh`
- [ ] `bash deploy/scripts/mole-ecs-smoke.sh`

## 发布后

- [ ] `https://molefinance.net` 正常打开
- [ ] `https://www.molefinance.net` 跳转到主域
- [ ] 首页为 live feed 基线
- [ ] 浏览器控制台无明显静态资源 404
