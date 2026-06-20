# Mole Post Deploy Release Checklist

## 基础可用性

- [ ] `curl -I https://molefinance.net` 返回 `200`
- [ ] `curl -I https://www.molefinance.net` 返回 `301` 到主域
- [ ] 首页 HTML 包含 `MoleOption Console`

## HTTPS

- [ ] 证书主题为 `molefinance.net`
- [ ] 证书同时覆盖 `www.molefinance.net`
- [ ] 浏览器无证书告警

## 业务连通性

- [ ] 首页默认不再是 mock feed
- [ ] 页面可以连接 devnet RPC
- [ ] 当前 keeper 仍在持续推进价格和 `sync_pool`

## 留痕

- [ ] 记录 release 时间
- [ ] 记录当前 release 目录
- [ ] 记录使用的前端 env 基线
- [ ] 记录是否仍由 macOS keeper 承载
