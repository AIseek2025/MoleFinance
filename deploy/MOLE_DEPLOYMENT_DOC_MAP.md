# Mole Deployment Doc Map

| 文件 | 职责 | 何时阅读 |
| --- | --- | --- |
| `deploy/README.md` | 总入口与阅读顺序 | 第一次接手部署 |
| `deploy/DEPLOYMENT_SOP_INDEX.md` | 最短执行路径 | 值班发布时 |
| `deploy/MOLE_DEPLOYMENT_SYSTEM_OVERVIEW.md` | 架构、模式、边界 | 理解全貌 |
| `deploy/MOLE_DEPLOYMENT_DOC_MAP.md` | 文档分工 | 不确定去哪看时 |
| `deploy/MOLE_SSH_ACCESS_RUNBOOK.md` | SSH、sudo、目录核验 | 登陆 ECS 前 |
| `deploy/MOLE_ECS_UI_DEPLOY_RUNBOOK.md` | 静态站发布 | 每次前端上线 |
| `deploy/MOLE_ECS_DEPLOY_PRODUCTION_README.md` | 首次初始化与完整手册 | 第一次部署 |
| `deploy/DEPLOY_CHECKLIST.md` | 发布前检查 | 发布开始前 |
| `deploy/MOLE_POST_DEPLOY_RELEASE_CHECKLIST.md` | 发布后验收 | 发布完成后 |
| `deploy/OPERATIONS_MANUAL.md` | 日常运维与故障处理 | 日常值班 |
| `deploy/MOLE_RELEASE_RECORD_PLAYBOOK.md` | 发布记录模板 | 每次发版后 |
| `deploy/MOLE_DEPLOYMENT_HISTORY_INDEX.md` | 发布历史索引 | 追溯变更 |
| `deploy/MOLE_DEPLOYMENT_CHANGELOG.md` | 部署体系演进记录 | 文档/脚本升级后 |
| `deploy/MOLE_DEPLOYMENT_GLOSSARY.md` | 统一术语 | 交接与培训 |
| `deploy/scripts/*` | 自动化执行入口 | 执行发布 |
| `infra/nginx/*` | Nginx 配置资产 | 初始化站点 |
| `infra/scripts/setup-server.sh` | 服务器首次初始化 | 首次部署 |
| `infra/systemd/mole-keeper-devnet.service` | Linux keeper 模板 | 迁移 keeper 到 ECS 时 |
