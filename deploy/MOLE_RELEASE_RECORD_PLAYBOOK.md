# Mole Release Record Playbook

每次发版后新增一份记录，建议命名:

`MOLE_RELEASE_NOTE_<YYYY-MM-DD>-<topic>.md`

模板:

```md
# Mole Release Note - <date> - <topic>

- 环境: ECS devnet demo
- 域名: https://molefinance.net
- 发布方式: `bash deploy/scripts/mole-ecs-deploy-ui.sh`
- 当前 release: `<remote release path>`
- keeper 承载: `macOS launchd` / `ECS systemd`

## 变更内容

- ...

## 执行记录

- `bash deploy/scripts/mole-ecs-preflight-check.sh`
- `bash deploy/scripts/mole-ecs-deploy-ui.sh`
- `bash deploy/scripts/mole-ecs-smoke.sh`

## 验收结果

- ...

## 回滚方式

- `ssh admin@8.218.209.218 'sudo ln -sfn <old_release> /var/www/molefinance/current'`
```
