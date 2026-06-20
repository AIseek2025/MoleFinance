# Mole SSH Access Runbook

## 1. 当前连接信息

- SSH 主机: `admin@8.218.209.218`
- 域名: `molefinance.net`
- ECS 目录: `/var/www/molefinance`

## 2. 基础验证

```bash
ssh admin@8.218.209.218 'echo MOLE_SSH_OK && hostname && whoami'
ssh admin@8.218.209.218 'sudo -n true && echo SUDO_OK'
```

## 3. 建议 SSH config

```sshconfig
Host mole-ecs
  HostName 8.218.209.218
  User admin
  IdentityFile ~/.ssh/id_ed25519
  StrictHostKeyChecking no
  UserKnownHostsFile /dev/null
```

验证:

```bash
ssh mole-ecs 'echo MOLE_SSH_OK && hostname && whoami'
```

## 4. 首次登录后应核对

```bash
ssh admin@8.218.209.218 '
  ls -ld /var/www /etc/nginx/conf.d &&
  nginx -v &&
  certbot --version
'
```

## 5. 故障速查

| 现象 | 原因 | 处理 |
| --- | --- | --- |
| `Permission denied (publickey)` | 本机私钥未加载 | 检查 `~/.ssh/id_ed25519` 与 agent |
| SSH 能进但 `sudo` 失败 | 当前用户无免密 sudo | 在 ECS 上补 sudoers |
| 域名不通 | DNS 未生效或未指向 `8.218.209.218` | 先查 `dig +short molefinance.net` |
