# Beacon — 运行时监控 + 公开状态页 + SSO 管理端

Beacon 是 HOLDFAST 主权基础设施栈中的 **uptime 监控** 服务：周期性探测各组件（HTTP 2xx / TCP 连接），把结果写入 Postgres，计算滚动可用率（24h / 7d / 90d），并对外提供一个 **公开的企业级状态页**（`/status`）与一个 **网关 SSO 保护的管理端**（`/admin`）。

技术栈与 keystone/keyward 一致：**Rust + axum**，rustls（`ring` 后端，无 OpenSSL），sqlx 运行期查询（无编译期宏、无数据库即可构建）。数据层只用 **可移植标准 SQL**（`TEXT/BIGINT/BOOLEAN` + `PK/NOT NULL/DEFAULT` + `INSERT .. ON CONFLICT` + `SUM(CASE WHEN ...)` 聚合），日后可在 FusionDB 上经 pgwire 原样运行。

## 角色与定位

- **仅内网服务**：Beacon 不直接对公网暴露，统一经 **Sluice 网关** 反代。
- **不做自有登录**：管理端坐落在 Sluice `auth=sso` 路由后，网关完成 OIDC 浏览器登录、**剥离**任何入站 `X-Auth-*` 后注入可信身份头（`X-Auth-Subject` / `X-Auth-Email` / `X-Auth-Scope`）。Beacon 直接 **信任** 这些头来显示「已登录为」并授权管理动作。登出走 `GET https://id.w33d.xyz/_gw/auth/logout`。
- **公开状态页无鉴权**：`/status` 与 `/api/status` 放在 `auth=public` 路由上。

## 端点

| 端点 | 鉴权 | 说明 |
|------|------|------|
| `GET /healthz` | 内部 | 200 `ok`，容器 HEALTHCHECK 使用 |
| `GET /status` | 公开 | 服务端渲染的公开状态页（总览横幅 + 组件状态药丸 + 可用率 + 事件） |
| `GET /api/status` | 公开 | 机器可读的状态快照（JSON） |
| `GET /admin` | SSO | 运维仪表盘：检查项列表 + 发布事件表单（读 `X-Auth-Email`） |
| `POST /admin/incidents` | SSO | 发布手动事件 `{title, status, body}`，随后出现在公开状态页 |

> 网关路由（部署期由 Sluice 添加）：`/status` 与 `/api/status` → `auth=public`；`/admin`（即「Beacon 管理端 / beacon」区域，前缀已覆盖 `/admin/incidents`）→ `auth=sso`。Sluice 不剥前缀，会把完整路径转发给上游，故服务内路径与网关前缀一致。

## 探针

每隔 `CHECK_INTERVAL`（默认 30s）对所有 **启用** 的检查项并发探测一次，记录 `ok` + 延迟（毫秒）+ 时间戳：

- `kind=http`：对 `target`（`http(s)://host[:port]/path`）发 `GET`，**2xx 即为 up**。`https` 用 rustls（`ring` + Mozilla 根证书 `webpki-roots`）。
- `kind=tcp`：对 `target`（`host:port`）发起 TCP 连接，**连接成功即为 up**。

**状态判定**：最近一次失败 → `down`；最近一次成功但 24h 可用率 < 99% → `degraded`（近期抖动）；否则 `operational`。总览取各组件中最差者。

## 数据模型（可移植标准 SQL）

```sql
checks(name TEXT PRIMARY KEY, kind TEXT, target TEXT, enabled BOOLEAN)
check_results(name TEXT, ok BOOLEAN, latency_ms BIGINT, ts BIGINT, PRIMARY KEY(name, ts))
incidents(id TEXT PRIMARY KEY, title TEXT, status TEXT, body TEXT, created_at BIGINT, updated_at BIGINT)
```

`check_results` 的主键 `(name, ts)` 同时支撑滚动可用率范围扫描与「最近结果」查询，无需额外索引。

## 配置（环境变量）

| 变量 | 默认 | 说明 |
|------|------|------|
| `BIND_ADDR` | `0.0.0.0:8400` | 监听地址 |
| `BEACON_STORE` | `memory` | `memory`（无数据库）或 `postgres` |
| `DATABASE_URL` | — | `BEACON_STORE=postgres` 时必填（共用栈内 DSN） |
| `CHECK_INTERVAL` | `30` | 探测扫描间隔（秒） |
| `PROBE_TIMEOUT` | `5` | 单次探针连接/响应超时（秒） |
| `BEACON_SEED` | 内置默认 | 检查项种子 JSON 数组；**仅在 checks 表为空时** 写入 |

`BEACON_SEED` 形如：

```json
[
  {"name":"Gateway","kind":"http","target":"https://id.w33d.xyz/healthz"},
  {"name":"Identity","kind":"tcp","target":"keystone:8443"},
  {"name":"CA","kind":"http","target":"http://keyward:8200/healthz","enabled":true}
]
```

不设置时使用内置默认种子（Gateway / Identity / CA，targets 在 `holdfast` 网络内可解析）。`Gateway` 默认探测公网 `https://id.w33d.xyz/healthz`，需容器能经 hairpin 回到本机 `:443`；若网络不支持，改为 `tcp sluice:443` 即可。

## 构建与测试

```bash
export PATH=$PATH:/usr/local/go/bin   # 本仓库不需要 Go；此处仅与栈内约定一致
cd /root/w33d_infra/beacon

cargo build            # 无需数据库
cargo test             # 默认全部用内存存储 + 本地 httptest 服务器，无需数据库

# Postgres 集成测试（需外部 Postgres；未设置 TEST_DATABASE_URL 时自动跳过）
docker run --rm -d -e POSTGRES_PASSWORD=pw -e POSTGRES_DB=beacon \
  -p 127.0.0.1:55441:5432 postgres:18-alpine
TEST_DATABASE_URL=postgres://postgres:pw@127.0.0.1:55441/beacon \
  cargo test --test pg_store -- --nocapture
```

测试覆盖：HTTP/TCP 探针对 up/down 服务器记录正确的 `ok`+延迟；可用率百分比数学；监控扫描落库；公开 `/status` 无鉴权渲染；事件创建后出现在公开页与 JSON；管理端 POST 需网关身份（401）。

## Docker

多阶段、非 root（uid 10001）、`ring` 后端（无 OpenSSL）、内置 `beacon healthcheck` 子命令、`EXPOSE 8400`。

```bash
docker build -t holdfast/beacon:dev .
docker run -d --name beacon -p 127.0.0.1:8400:8400 \
  -e BEACON_STORE=postgres -e DATABASE_URL=$DATABASE_URL \
  holdfast/beacon:dev
curl -fsS http://127.0.0.1:8400/healthz      # ok
curl -fsS http://127.0.0.1:8400/status       # 公开状态页
```

## 部署接线（交给 deploy）

- 在 `holdfast` 网络内新增 `beacon` 服务，`BEACON_STORE=postgres` + 共用 `DATABASE_URL`，**仅内网**（不发布公网端口）。
- Sluice 路由表新增：`/status` 与 `/api/status` → `auth=public`、上游 `http://beacon:8400`；`/admin` → `auth=sso`、上游 `http://beacon:8400`。
- 可选：通过 `BEACON_SEED` 注入真实组件种子。
