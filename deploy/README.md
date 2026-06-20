# Mole ECS Deployment Docs

这是一套给 `Mole` 测试网版本准备的 ECS 部署文档体系，参考了
`/Users/surferboy/iBox/deploy` 的结构，并按当前项目实际形态做了裁剪：

- 前端是 `Vite` 静态站点
- 链上环境是 `Solana devnet`
- keeper 可以继续跑在现有常驻机，也可以后续迁到 ECS
- 本次首发目标域名是 `https://molefinance.net`

## 推荐阅读顺序

1. `DEPLOYMENT_SOP_INDEX.md`
2. `MOLE_DEPLOYMENT_SYSTEM_OVERVIEW.md`
3. `MOLE_ECS_DEPLOY_PRODUCTION_README.md`
4. `MOLE_ECS_UI_DEPLOY_RUNBOOK.md`
5. `DEPLOY_CHECKLIST.md`
6. `MOLE_POST_DEPLOY_RELEASE_CHECKLIST.md`
7. `OPERATIONS_MANUAL.md`

## 文档角色

- `DEPLOYMENT_SOP_INDEX.md`: 值班时的最短执行路径
- `MOLE_DEPLOYMENT_SYSTEM_OVERVIEW.md`: 整体架构、边界和部署模式
- `MOLE_DEPLOYMENT_DOC_MAP.md`: 每份文档职责
- `MOLE_SSH_ACCESS_RUNBOOK.md`: SSH 与权限检查
- `MOLE_ECS_UI_DEPLOY_RUNBOOK.md`: 前端静态站发布手册
- `MOLE_ECS_DEPLOY_PRODUCTION_README.md`: 首次 ECS 初始化与完整手册
- `DEPLOY_CHECKLIST.md`: 发布前核对项
- `MOLE_POST_DEPLOY_RELEASE_CHECKLIST.md`: 发布后验收项
- `OPERATIONS_MANUAL.md`: 日常运维与排障
- `MOLE_RELEASE_RECORD_PLAYBOOK.md`: 发布记录模板
- `MOLE_DEPLOYMENT_HISTORY_INDEX.md`: 发布历史索引
- `MOLE_DEPLOYMENT_CHANGELOG.md`: 部署体系变更记录
- `MOLE_DEPLOYMENT_GLOSSARY.md`: 术语表

## 脚本入口

- `bash deploy/scripts/mole-ecs-preflight-check.sh`
- `bash deploy/scripts/mole-ecs-deploy-ui.sh`
- `bash deploy/scripts/mole-ecs-smoke.sh`

## 运行资产

- `infra/nginx/molefinance.net.bootstrap.conf`: 证书签发前的 HTTP 配置
- `infra/nginx/molefinance.net.conf`: 正式 HTTPS 配置
- `infra/scripts/setup-server.sh`: 首次初始化脚本
- `infra/systemd/mole-keeper-devnet.service`: 可选的 Linux keeper 模板

## 当前生产基线

- 域名: `molefinance.net` / `www.molefinance.net`
- ECS IP: `8.218.209.218`
- ECS SSH: `admin@8.218.209.218`
- 站点目录: `/var/www/molefinance`
- 前端发布模式: 本地 build + rsync 到 ECS
- keeper 基线: 继续使用现有常驻 devnet keeper；ECS keeper 为可选增强
