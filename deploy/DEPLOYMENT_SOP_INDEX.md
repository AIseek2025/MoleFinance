# Mole Deployment SOP Index

## 1. 首次 ECS 部署最短路径

1. 本地准备前端生产环境：
   - 复制 `frontend/.env.production.example`
   - 填入受限 devnet RPC key
2. 执行发布前检查：
   - `bash deploy/scripts/mole-ecs-preflight-check.sh`
3. 首次初始化 ECS：
   - `bash infra/scripts/setup-server.sh`
4. 申请证书：
   - `ssh admin@8.218.209.218 'sudo certbot certonly --webroot -w /var/www/molefinance/certbot -d molefinance.net -d www.molefinance.net'`
5. 安装 HTTPS Nginx 配置：
   - `ssh admin@8.218.209.218 'sudo cp /var/www/molefinance/repo/infra/nginx/molefinance.net.conf /etc/nginx/conf.d/molefinance.net.conf && sudo nginx -t && sudo systemctl reload nginx'`
6. 发布前端：
   - `bash deploy/scripts/mole-ecs-deploy-ui.sh`
7. 验收：
   - `bash deploy/scripts/mole-ecs-smoke.sh`

## 2. 日常前端发布

1. `bash deploy/scripts/mole-ecs-preflight-check.sh`
2. `bash deploy/scripts/mole-ecs-deploy-ui.sh`
3. `bash deploy/scripts/mole-ecs-smoke.sh`

## 3. 目录速查

- 仓库根目录: `/Users/surferboy/MoleOption`
- ECS 站点目录: `/var/www/molefinance`
- ECS 发布目录: `/var/www/molefinance/releases`
- ECS 当前版本软链: `/var/www/molefinance/current`
- ACME challenge 目录: `/var/www/molefinance/certbot`
- Nginx 配置: `/etc/nginx/conf.d/molefinance.net.conf`

## 4. 当前上线边界

- 这是 `Solana devnet` 体验版，不是主网正式版
- `mock-oracle` 仅用于 devnet，不能迁到主网
- 首次 ECS 发布默认只承载前端静态站与 HTTPS
- keeper 允许继续使用现有常驻机器；ECS keeper 为后续可选迁移项
