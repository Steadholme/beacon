# Beacon — 运行时监控 + 公开状态页 + SSO 管理端

Beacon 是 Steadholme 主权基础设施栈中的 **uptime 监控** 服务：周期性探测各组件（HTTP 2xx / TCP 连接），把结果写入 Postgres，计算滚动可用率（24h / 7d / 当前 read model 的 evidence window），并提供一个 **incident-first 公开状态页**（`/status`）与一个 **网关 SSO 保护的管理端**（`/admin`）。公开组件由显式 catalog 投影，不再等同于内部 raw checks；新增内部 listener 给 Portal 等服务端消费者读取完整模型。

技术栈与 keystone/keyward 一致：**Rust + axum**，rustls（`ring` 后端，无 OpenSSL），sqlx 运行期查询（无编译期宏、无数据库即可构建）。数据层只用 **可移植标准 SQL**（`TEXT/BIGINT/BOOLEAN` + `PK/NOT NULL/DEFAULT` + `INSERT .. ON CONFLICT` + `SUM(CASE WHEN ...)` 聚合），日后可在 FusionDB 上经 pgwire 原样运行。

## 角色与定位

- **仅内网服务**：Beacon 不直接对公网暴露，统一经 **Sluice 网关** 反代。
- **不做自有登录**：管理端坐落在 Sluice `auth=sso` 路由后，网关完成 OIDC 浏览器登录、**剥离**任何入站 `X-Auth-*` 后注入可信身份头（`X-Auth-Subject` / `X-Auth-Email` / `X-Auth-Scope`）。Beacon 直接 **信任** 这些头来显示「已登录为」并授权管理动作。登出走 `GET https://id.w33d.xyz/_gw/auth/logout`。
- **公开状态页无鉴权**：`/`、`/status`、`/api/status` 与 `/feed.xml` 放在 `auth=public` 路由上，并始终返回 `Cache-Control: no-store`（RSS 同样不缓存）。
- **公网 read model 显式投影**：`BEACON_PUBLIC_CATALOG` 只列可公开的稳定产品名；一个公开产品可聚合多个 raw checks。新增内部探针不会自动公开，raw `target`、内部组件名和内部-only incident/maintenance 不进入 HTML、JSON 或 RSS。
- **内部 read model 独立 listener**：设置 `BEACON_INTERNAL_BIND_ADDR=0.0.0.0:8401` 后，trusted Docker 网络可从该 listener 的 `/api/status` 读取全量 raw model。该 route 不注册到公网 `8400` router，Sluice 也不得连接 `8401`。
- **Webhook 默认关闭**：RSS 与 JSON 永远可用；只有显式启用 `BEACON_PUBLIC_WEBHOOKS_ENABLED=true` 才显示注册表单并投递。出站 webhook 每次投递都会解析 DNS、拒绝 private/loopback/link-local/metadata/multicast/unspecified/special-use 地址，并连接已校验的 `SocketAddr`（TLS SNI 仍使用原 hostname）。

## 端点

| 端点 | 鉴权 | 说明 |
|------|------|------|
| `GET /healthz` | 内部 | 200 `ok`，容器 HEALTHCHECK 使用 |
| `GET /status` | 公开 | 服务端渲染的公开状态页（v2 · Status：状态即标题 + 全估算 30 天条 + 事故/维护账本 + 组件瓷砖网格 + 容量 + 14 天历史） |
| `GET /api/status`（8400） | 公开 | 显式 catalog 投影的机器可读状态快照（JSON，30 天 daily evidence） |
| `GET /assets/beacon-20260910.css` | 公开 | 版本化不可变样式表（Odyssey 基础 + `service.css`；改 CSS 时改日期） |
| `GET /feed.xml` | 公开 | 仅含公开受影响组件的 incident RSS |
| `POST /subscriptions` | 公开、feature-gated | Webhook 注册；默认 404，开启后执行 public-egress 校验与 double opt-in |
| `GET /admin` | SSO | 运维仪表盘：检查项列表 + 发布事件表单（读 `X-Auth-Email`） |
| `POST /admin/incidents` | SSO | 发布手动事件 `{title, status, body}`，随后出现在公开状态页 |
| `GET /api/status`（8401） | trusted internal network | 全量 raw check 状态，供 Portal server-side join；不经过 Sluice |

兼容字段 `components[].uptime_90d` 保留原 JSON 名称，避免破坏 Portal 等既有客户端；其值始终按同一响应的 `history_days` 计算。公网 read model 为 30 天，internal/operator read model 为 90 天，客户端不得再从字段名推断时间窗口。

> 网关路由（部署期由 Sluice 添加）：`/status` 与 `/api/status` → `auth=public`；`/admin`（即「Beacon 管理端 / beacon」区域，前缀已覆盖 `/admin/incidents`）→ `auth=sso`。Sluice 不剥前缀，会把完整路径转发给上游，故服务内路径与网关前缀一致。

## 公开状态页 `GET /status`（v2 · Status）

设计稿在 Figma「Status」文件夹（fileKey `UTPp9yfkCnQs9XJ0NI8Pmy`）。词汇规则：页面上每个字符串都是**名字、数值或动作**——没有「System status」标题、没有「N monitored / N components」计数、全绿页面不重复 Operational。

结构（`templates/status.html` + `handlers/status.rs`，样式全部落在 `service.css` 的 `.page-status` 作用域，管理台 `.bc-desk` 不受影响）：

- **顶栏**：品牌、UTC 时钟（`<time data-clock>`，`static/status-page.js` 每 15s 校正）、语言/主题切换、「Get updates」原生 `<details>` 弹层（RSS / JSON / 启用时的 Webhook）。
- **Masthead**：`overall` 状态即 `<h1 id="status-title">`（`status.hero.*.title`），形状+颜色双编码的 `.mark`（圆=正常、菱形=降级、方块=中断、环=维护、短横=待首检），meta 行只放数值与动作：30 天平均可用率、受影响数（仅非正常时）、更新时间、Refresh（Odyssey Wire，无 JS 时整页导航）。
- **Estate strip**：30 个 UTC 日格，取所有公开组件当日最差状态（down > warn > ok，全无数据才 unknown），`data-date/data-uptime/data-inc` 供悬停提示；轴标只有起止日期。
- **Active incidents / Maintenance**：事故卡用 StageTrack（Investigating → Identified → Monitoring → Resolved，`aria-current="step"`）替代状态药丸，受影响组件是带当前状态标记的 Chip；维护卡用等宽倒计时数值（`starts in / ends in`）+ UTC 窗口。
- **Catalog**：按 catalog 分组渲染 `<section class="group group--{rollup}">`，成员是 `<details class="tile tile--{state}" name="tile">` 瓷砖（名字、标记、30 天迷你格、可用率、24h 平均延迟；只有非正常瓷砖才出现状态词），展开即 TileDetail 弹层（30d/24h/7d 可用率、带日期的完整日格、24h 延迟曲线、监控起始日）。`name="tile"` 保证同一时间只开一个；窄屏变成底部面板。
- **Infrastructure**（有 Vitals 时）：三个容量仪表（最差主机）+ 24 小时热条。
- **Past incidents**：最近 14 天内**已解决**的公开事故（进行中的在上方账本），空态一行。
- **Channels / Footer**：RSS feed、JSON API 两个动作按钮（启用 webhook 时加 URL 表单），页脚三个动作 + 更新时间。

`#status-live` 仍是唯一可替换的 Wire 区域：完整 SSR 与 `X-Wire: 1` 片段调用同一个渲染函数，页面无 JS 也完整可读（`<meta http-equiv="refresh" content="300">` 兜底）。

## 探针

每隔 `CHECK_INTERVAL`（默认 30s）对所有 **启用** 的检查项并发探测一次，记录 `ok` + 延迟（毫秒）+ 时间戳：

- `kind=http`：对 `target`（`http(s)://host[:port]/path`）发 `GET`，**2xx 即为 up**。`https` 用 rustls（`ring` + Mozilla 根证书 `webpki-roots`）。
- `kind=tcp`：对 `target`（`host:port`）发起 TCP 连接，**连接成功即为 up**。

**状态判定**：最近一次失败 → `down`；最近一次成功但 24h 可用率 < 99% → `degraded`（近期抖动）；否则 `operational`。一个公开组件聚合多个 raw checks 时，当前状态取最差成员、可用率按 probe samples 汇总、延迟展示最差最新值；总览只由公开组件和公开 incident/maintenance 决定。

Incident/maintenance 的 `affected` 仍由管理员填写 raw check 名，但公开读取时会映射到 catalog `name`、去重并移除内部成员。没有命中任何公开组件的记录 fail closed：不会出现在公开 HTML、JSON 或 RSS；内部 listener 仍保留完整记录。

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
| `BEACON_INTERNAL_BIND_ADDR` | 未启用 | trusted internal listener，例如 `0.0.0.0:8401` |
| `BEACON_STORE` | `memory` | `memory`（无数据库）或 `postgres` |
| `DATABASE_URL` | — | `BEACON_STORE=postgres` 时必填（共用栈内 DSN） |
| `CHECK_INTERVAL` | `30` | 探测扫描间隔（秒） |
| `PROBE_TIMEOUT` | `5` | 单次探针连接/响应超时（秒） |
| `BEACON_SEED` | 内置默认 | 检查项种子 JSON 数组；**仅在 checks 表为空时** 写入 |
| `BEACON_PUBLIC_CATALOG` | Gateway + Identity | 显式公开组件 JSON；设置为 `[]` 或非法 JSON 时 fail closed，不公开组件 |
| `BEACON_PUBLIC_WEBHOOKS_ENABLED` | `false` | 是否开放公网 webhook 注册与 incident fan-out |

`BEACON_SEED` 形如：

```json
[
  {"name":"Gateway","kind":"http","target":"https://id.w33d.xyz/healthz"},
  {"name":"Identity","kind":"tcp","target":"keystone:8443"},
  {"name":"CA","kind":"http","target":"http://keyward:8200/healthz","enabled":true}
]
```

不设置时使用内置默认种子（Gateway / Identity / CA，targets 在 `holdfast` 网络内可解析）。`Gateway` 默认探测公网 `https://id.w33d.xyz/healthz`，需容器能经 hairpin 回到本机 `:443`；若网络不支持，改为 `tcp sluice:443` 即可。

`BEACON_PUBLIC_CATALOG` 形如：

```json
[
  {"name":"Gateway","group":"Core","checks":["Gateway"]},
  {"name":"Identity","group":"Core","checks":["Identity"]},
  {"name":"AI Gateway","group":"AI","checks":["OpenAI primary","OpenAI fallback"]}
]
```

`name` 是公开 API 的稳定 join key，应与 Manifest `statusComponent` 一致；`group` 仅用于公开页面分组；`checks` 是不会公开的 raw probe 名。显式配置但没有 enabled member 的 component 会被省略，避免把没有证据的服务误报为 100%。环境变量存在但 JSON 非法时 catalog 置空，不回退全量 checks。

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

测试覆盖：HTTP/TCP 探针对 up/down 服务器记录正确的 `ok`+延迟；可用率百分比数学；public catalog 聚合与 raw-name 防泄漏；internal/public router 隔离；incident/maintenance/RSS 投影；30 天公开 payload；webhook 默认关闭与 SSRF 地址分类；管理端 POST 需网关身份（401）。

## Docker

多阶段、非 root（uid 10001）、`ring` 后端（无 OpenSSL）、内置 `beacon healthcheck` 子命令、`EXPOSE 8400`。

```bash
docker build -t steadholme/beacon:dev .
docker run -d --name beacon -p 127.0.0.1:8400:8400 \
  -e BEACON_STORE=postgres -e DATABASE_URL=$DATABASE_URL \
  steadholme/beacon:dev
curl -fsS http://127.0.0.1:8400/healthz      # ok
curl -fsS http://127.0.0.1:8400/status       # 公开状态页
```

## 部署接线（交给 deploy）

- 在 `holdfast` 网络内新增 `beacon` 服务，`BEACON_STORE=postgres` + 共用 `DATABASE_URL`，**不发布 host 端口**。生产显式设置 `BEACON_PUBLIC_CATALOG` 与 `BEACON_INTERNAL_BIND_ADDR=0.0.0.0:8401`。
- Sluice 路由表新增：`/status` 与 `/api/status` → `auth=public`、上游 `http://beacon:8400`；`/admin` → `auth=sso`、上游 `http://beacon:8400`。
- Portal 等 server-only 消费者通过 Docker `holdfast` 网络读取 `http://beacon:8401/api/status`。不得将 `8401` 加入 Sluice route 或 host port mapping。
- 可选：通过 `BEACON_SEED` 注入真实组件种子；这不会自动扩张 public catalog。
