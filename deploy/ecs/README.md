# Mole ECS Assets Map

## 代码资产与 ECS 对应关系

- `infra/nginx/molefinance.net.bootstrap.conf`
  -> `/etc/nginx/conf.d/molefinance.net.conf`
- `infra/nginx/molefinance.net.conf`
  -> `/etc/nginx/conf.d/molefinance.net.conf`
- `infra/scripts/setup-server.sh`
  -> 首次初始化脚本
- `infra/systemd/mole-keeper-devnet.service`
  -> `/etc/systemd/system/mole-keeper-devnet.service`

## 站点目录

- `/var/www/molefinance/certbot`
- `/var/www/molefinance/releases`
- `/var/www/molefinance/current`
- `/var/www/molefinance/repo`
