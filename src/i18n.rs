//! Beacon-owned status-page catalog layered over Odyssey i18n.

use odyssey::Locale;

pub static EN: &[(&str, &str)] = &[
    ("infra.band.elevated", "elevated"),
    ("infra.band.high", "high"),
    ("infra.band.ok", "ok"),
    ("infra.band.unknown", "unknown"),
    ("infra.metric.cpu", "CPU"),
    ("infra.metric.disk", "Disk"),
    ("infra.metric.memory", "Memory"),
    (
        "infra.note",
        "Capacity signals are aggregated and leak-safe.",
    ),
    ("infra.state.elevated", "Elevated"),
    ("infra.state.high", "High"),
    ("infra.state.ok", "OK"),
    ("infra.state.unknown", "Unknown"),
    ("infra.title", "Infrastructure"),
    ("infra.trend", "Last 24 hours"),
    ("status.active", "Active incidents"),
    ("status.affects", "Affects"),
    ("status.bar.no_data", "no data"),
    (
        "status.bar.no_data_since",
        "no data — monitoring began {date}",
    ),
    (
        "status.channels.body",
        "Follow public incidents without an account.",
    ),
    (
        "status.channels.webhook_unavailable",
        "Webhook registration is currently unavailable. Existing public reads are unaffected.",
    ),
    ("status.components", "Components"),
    ("status.components.monitored", "{n} monitored"),
    ("status.footer", "Steadholme · Sovereign infrastructure"),
    ("status.get_updates", "Get updates"),
    ("status.group.count", "{n} components"),
    ("status.group.other", "Other"),
    (
        "status.hero.affected",
        "{affected} of {total} components affected",
    ),
    (
        "status.hero.down.sub",
        "One or more components are down. We are on it.",
    ),
    ("status.hero.down.title", "Service disruption"),
    (
        "status.hero.maint.sub",
        "Planned maintenance is in progress; affected components may be briefly unavailable.",
    ),
    ("status.hero.maint.title", "Scheduled maintenance underway"),
    (
        "status.hero.ok.sub",
        "Every monitored component is up and healthy.",
    ),
    ("status.hero.ok.title", "All systems operational"),
    (
        "status.hero.warn.sub",
        "Some components are degraded; service may be slower than usual.",
    ),
    ("status.hero.warn.title", "Partial degradation"),
    (
        "status.history_note",
        "Incident history from the last 14 days.",
    ),
    ("status.incident.identified", "Identified"),
    ("status.incident.investigating", "Investigating"),
    ("status.incident.lasted", "· lasted {duration}"),
    ("status.incident.monitoring", "Monitoring"),
    ("status.incident.opened_at", "Opened {ago}"),
    (
        "status.incident.opened_resolved",
        "Opened {opened} · Resolved {resolved}",
    ),
    (
        "status.incident.opened_updated",
        "Opened {opened} · Last update {updated}",
    ),
    ("status.incident.reported", "reported"),
    ("status.incident.resolved", "Resolved"),
    ("status.json_api", "JSON API"),
    ("status.lang_label", "Language"),
    ("status.live_region", "Live system status"),
    ("status.maint.ends_in", "ends in {t}"),
    ("status.maint.in_progress", "in progress"),
    ("status.maint.scheduled", "scheduled"),
    ("status.maint.starts_in", "starts in {t}"),
    ("status.maintenance", "Maintenance"),
    (
        "status.no_components",
        "No components are being monitored yet.",
    ),
    ("status.none_day", "No incidents reported."),
    (
        "status.none_history",
        "No incidents reported in the last 14 days.",
    ),
    ("status.past", "Past incidents"),
    ("status.public_read_only", "Public · read only"),
    ("status.refresh", "Refresh status"),
    ("status.refresh_busy", "Refreshing…"),
    (
        "status.refresh_error",
        "Live refresh failed — use the link to reload.",
    ),
    ("status.refresh_success", "Live status refreshed."),
    ("status.row.awaiting_first_check", "awaiting first check"),
    ("status.row.latency_title", "24h average · latest {latest}"),
    ("status.row.monitoring_since", "monitoring since {date}"),
    ("status.rss", "RSS"),
    ("status.rss_feed", "RSS feed"),
    ("status.severity.critical", "critical"),
    ("status.severity.major", "major"),
    ("status.severity.minor", "minor"),
    ("status.skip_to_content", "Skip to system status"),
    ("status.snapshot.evidence", "days of evidence"),
    ("status.snapshot.incidents", "active incidents"),
    ("status.snapshot.label", "Operational snapshot"),
    ("status.snapshot.maintenance", "maintenance windows"),
    ("status.snapshot.services", "public services"),
    ("status.state.degraded", "Degraded"),
    ("status.state.down", "Down"),
    ("status.state.maintenance", "Maintenance"),
    ("status.state.operational", "Operational"),
    (
        "status.sub",
        "Live availability of the Steadholme sovereign infrastructure.",
    ),
    (
        "status.subscribe.body",
        "Get a signed JSON webhook POST whenever an incident is opened or updated.",
    ),
    ("status.subscribe.button", "Subscribe"),
    ("status.subscribe.field", "Webhook URL"),
    (
        "status.subscribe.note",
        "Webhook-only — Beacon has no outbound mail path. You will confirm before any delivery.",
    ),
    ("status.subscribe.title", "Subscribe to updates"),
    ("status.timeline", "Timeline ({n})"),
    ("status.title", "System status"),
    ("status.topbar", "System Status"),
    ("status.updated", "Updated {time}"),
    (
        "status.uptime.average_all",
        "Average public-service uptime over {days} days",
    ),
    (
        "status.uptime.average_group",
        "Average group uptime over {days} days",
    ),
    ("status.uptime.component_title", "Uptime over {days} days"),
    ("status.uptime.days_ago", "{days} days ago"),
    (
        "status.uptime.legend_empty",
        "No uptime data yet · All times UTC",
    ),
    (
        "status.uptime.legend_summary",
        "{uptime}% uptime · All times UTC",
    ),
    (
        "status.uptime.monitoring_since",
        " · Monitoring since {date}",
    ),
    ("status.uptime.today", "Today"),
    ("status.uptime.window", "uptime · {days} days"),
    ("status.webhook", "Webhook notifications"),
    ("time.d_ago", "{n}d ago"),
    ("time.duration.dh", "{d}d {h}h"),
    ("time.duration.hm", "{h}h {m}m"),
    ("time.duration.m", "{m}m"),
    ("time.h_ago", "{n}h ago"),
    ("time.just_now", "just now"),
    ("time.m_ago", "{n}m ago"),
    ("time.now", "now"),
    ("time.s_ago", "{n}s ago"),
    ("time.under_minute", "under a minute"),
];

pub static ZH: &[(&str, &str)] = &[
    ("infra.band.elevated", "偏高"),
    ("infra.band.high", "高"),
    ("infra.band.ok", "正常"),
    ("infra.band.unknown", "未知"),
    ("infra.metric.cpu", "CPU"),
    ("infra.metric.disk", "磁盘"),
    ("infra.metric.memory", "内存"),
    ("infra.note", "容量信号仅展示泄漏安全的聚合结果。"),
    ("infra.state.elevated", "偏高"),
    ("infra.state.high", "高"),
    ("infra.state.ok", "正常"),
    ("infra.state.unknown", "未知"),
    ("infra.title", "基础设施"),
    ("infra.trend", "最近 24 小时"),
    ("status.active", "当前事故"),
    ("status.affects", "影响"),
    ("status.bar.no_data", "暂无数据"),
    ("status.bar.no_data_since", "暂无数据 — 监控始于 {date}"),
    ("status.channels.body", "无需账户即可关注公开事故。"),
    (
        "status.channels.webhook_unavailable",
        "Webhook 注册目前不可用；公开读取渠道不受影响。",
    ),
    ("status.components", "组件"),
    ("status.components.monitored", "监控 {n} 个组件"),
    ("status.footer", "Steadholme · 主权基础设施"),
    ("status.get_updates", "获取更新"),
    ("status.group.count", "{n} 个组件"),
    ("status.group.other", "其他"),
    (
        "status.hero.affected",
        "{total} 个组件中有 {affected} 个受影响",
    ),
    (
        "status.hero.down.sub",
        "一个或多个组件不可用。我们正在处理。",
    ),
    ("status.hero.down.title", "服务中断"),
    (
        "status.hero.maint.sub",
        "计划维护正在进行，受影响组件可能短暂不可用。",
    ),
    ("status.hero.maint.title", "计划维护进行中"),
    ("status.hero.ok.sub", "所有监控组件都在线且健康。"),
    ("status.hero.ok.title", "所有系统运行正常"),
    ("status.hero.warn.sub", "部分组件降级，服务可能比平时更慢。"),
    ("status.hero.warn.title", "部分降级"),
    ("status.history_note", "最近 14 天的事故历史。"),
    ("status.incident.identified", "已定位"),
    ("status.incident.investigating", "调查中"),
    ("status.incident.lasted", "· 持续 {duration}"),
    ("status.incident.monitoring", "监控中"),
    ("status.incident.opened_at", "创建于 {ago}"),
    (
        "status.incident.opened_resolved",
        "创建于 {opened} · 解决于 {resolved}",
    ),
    (
        "status.incident.opened_updated",
        "创建于 {opened} · 最后更新 {updated}",
    ),
    ("status.incident.reported", "已报告"),
    ("status.incident.resolved", "已解决"),
    ("status.json_api", "JSON API"),
    ("status.lang_label", "语言"),
    ("status.live_region", "实时系统状态"),
    ("status.maint.ends_in", "{t} 后结束"),
    ("status.maint.in_progress", "进行中"),
    ("status.maint.scheduled", "已排期"),
    ("status.maint.starts_in", "{t} 后开始"),
    ("status.maintenance", "维护"),
    ("status.no_components", "还没有监控组件。"),
    ("status.none_day", "未报告事故。"),
    ("status.none_history", "最近 14 天未报告事故。"),
    ("status.past", "历史事故"),
    ("status.public_read_only", "公开 · 只读"),
    ("status.refresh", "刷新状态"),
    ("status.refresh_busy", "正在刷新…"),
    ("status.refresh_error", "实时刷新失败，请使用链接重新加载。"),
    ("status.refresh_success", "实时状态已刷新。"),
    ("status.row.awaiting_first_check", "等待首次检查"),
    ("status.row.latency_title", "24 小时平均 · 最新 {latest}"),
    ("status.row.monitoring_since", "监控始于 {date}"),
    ("status.rss", "RSS"),
    ("status.rss_feed", "RSS feed"),
    ("status.severity.critical", "严重"),
    ("status.severity.major", "主要"),
    ("status.severity.minor", "轻微"),
    ("status.skip_to_content", "跳到系统状态"),
    ("status.snapshot.evidence", "天证据窗口"),
    ("status.snapshot.incidents", "当前事故"),
    ("status.snapshot.label", "运行快照"),
    ("status.snapshot.maintenance", "维护窗口"),
    ("status.snapshot.services", "公开服务"),
    ("status.state.degraded", "降级"),
    ("status.state.down", "中断"),
    ("status.state.maintenance", "维护"),
    ("status.state.operational", "正常"),
    ("status.sub", "Steadholme 主权基础设施的实时可用性。"),
    (
        "status.subscribe.body",
        "当事故创建或更新时，接收带签名的 JSON webhook POST。",
    ),
    ("status.subscribe.button", "订阅"),
    ("status.subscribe.field", "Webhook URL"),
    (
        "status.subscribe.note",
        "仅支持 webhook；Beacon 没有出站邮件路径。投递前需要确认。",
    ),
    ("status.subscribe.title", "订阅更新"),
    ("status.timeline", "时间线 ({n})"),
    ("status.title", "系统状态"),
    ("status.topbar", "系统状态"),
    ("status.updated", "更新于 {time}"),
    (
        "status.uptime.average_all",
        "最近 {days} 天所有公开服务的平均可用率",
    ),
    (
        "status.uptime.average_group",
        "最近 {days} 天该分组的平均可用率",
    ),
    ("status.uptime.component_title", "最近 {days} 天可用率"),
    ("status.uptime.days_ago", "{days} 天前"),
    (
        "status.uptime.legend_empty",
        "暂无可用率数据 · 时间均为 UTC",
    ),
    (
        "status.uptime.legend_summary",
        "{uptime}% 可用率 · 时间均为 UTC",
    ),
    ("status.uptime.monitoring_since", " · 监控始于 {date}"),
    ("status.uptime.today", "今天"),
    ("status.uptime.window", "可用率 · {days} 天"),
    ("status.webhook", "Webhook 通知"),
    ("time.d_ago", "{n} 天前"),
    ("time.duration.dh", "{d} 天 {h} 小时"),
    ("time.duration.hm", "{h} 小时 {m} 分钟"),
    ("time.duration.m", "{m} 分钟"),
    ("time.h_ago", "{n} 小时前"),
    ("time.just_now", "刚刚"),
    ("time.m_ago", "{n} 分钟前"),
    ("time.now", "现在"),
    ("time.s_ago", "{n} 秒前"),
    ("time.under_minute", "不到一分钟"),
];

pub static JA: &[(&str, &str)] = &[
    ("infra.band.elevated", "上昇"),
    ("infra.band.high", "高"),
    ("infra.band.ok", "正常"),
    ("infra.band.unknown", "不明"),
    ("infra.metric.cpu", "CPU"),
    ("infra.metric.disk", "ディスク"),
    ("infra.metric.memory", "メモリ"),
    (
        "infra.note",
        "容量シグナルは漏えいを避けた集計のみを表示します。",
    ),
    ("infra.state.elevated", "上昇"),
    ("infra.state.high", "高"),
    ("infra.state.ok", "正常"),
    ("infra.state.unknown", "不明"),
    ("infra.title", "インフラ"),
    ("infra.trend", "過去 24 時間"),
    ("status.active", "進行中のインシデント"),
    ("status.affects", "影響"),
    ("status.bar.no_data", "データなし"),
    ("status.bar.no_data_since", "データなし — 監視開始 {date}"),
    (
        "status.channels.body",
        "アカウントなしで公開インシデントを確認できます。",
    ),
    (
        "status.channels.webhook_unavailable",
        "Webhook 登録は現在利用できません。公開読み取り機能には影響しません。",
    ),
    ("status.components", "コンポーネント"),
    ("status.components.monitored", "{n} 件を監視中"),
    ("status.footer", "Steadholme · 主権インフラ"),
    ("status.get_updates", "更新を受け取る"),
    ("status.group.count", "コンポーネント {n} 件"),
    ("status.group.other", "その他"),
    (
        "status.hero.affected",
        "{total} 個中 {affected} 個のコンポーネントが影響を受けています",
    ),
    (
        "status.hero.down.sub",
        "一部のコンポーネントが停止しています。対応中です。",
    ),
    ("status.hero.down.title", "サービス停止"),
    (
        "status.hero.maint.sub",
        "計画メンテナンス中です。対象コンポーネントは一時的に利用できない場合があります。",
    ),
    ("status.hero.maint.title", "計画メンテナンス中"),
    (
        "status.hero.ok.sub",
        "すべての監視コンポーネントは正常です。",
    ),
    ("status.hero.ok.title", "すべてのシステムは正常です"),
    (
        "status.hero.warn.sub",
        "一部のコンポーネントが低下しています。通常より遅い可能性があります。",
    ),
    ("status.hero.warn.title", "一部低下"),
    ("status.history_note", "過去 14 日間のインシデント履歴。"),
    ("status.incident.identified", "原因特定"),
    ("status.incident.investigating", "調査中"),
    ("status.incident.lasted", "· 継続時間 {duration}"),
    ("status.incident.monitoring", "監視中"),
    ("status.incident.opened_at", "作成 {ago}"),
    (
        "status.incident.opened_resolved",
        "作成 {opened} · 解決 {resolved}",
    ),
    (
        "status.incident.opened_updated",
        "作成 {opened} · 最終更新 {updated}",
    ),
    ("status.incident.reported", "報告"),
    ("status.incident.resolved", "解決済み"),
    ("status.json_api", "JSON API"),
    ("status.lang_label", "言語"),
    ("status.live_region", "ライブシステムステータス"),
    ("status.maint.ends_in", "終了まで {t}"),
    ("status.maint.in_progress", "実施中"),
    ("status.maint.scheduled", "予定"),
    ("status.maint.starts_in", "開始まで {t}"),
    ("status.maintenance", "メンテナンス"),
    (
        "status.no_components",
        "監視対象コンポーネントはまだありません。",
    ),
    ("status.none_day", "インシデントは報告されていません。"),
    (
        "status.none_history",
        "過去 14 日間にインシデントは報告されていません。",
    ),
    ("status.past", "過去のインシデント"),
    ("status.public_read_only", "公開 · 読み取り専用"),
    ("status.refresh", "ステータスを更新"),
    ("status.refresh_busy", "更新中…"),
    (
        "status.refresh_error",
        "ライブ更新に失敗しました。リンクから再読み込みしてください。",
    ),
    ("status.refresh_success", "ライブステータスを更新しました。"),
    ("status.row.awaiting_first_check", "初回チェック待ち"),
    ("status.row.latency_title", "24 時間平均 · 最新 {latest}"),
    ("status.row.monitoring_since", "監視開始 {date}"),
    ("status.rss", "RSS"),
    ("status.rss_feed", "RSS feed"),
    ("status.severity.critical", "重大"),
    ("status.severity.major", "主要"),
    ("status.severity.minor", "軽微"),
    ("status.skip_to_content", "システムステータスへ移動"),
    ("status.snapshot.evidence", "日間の履歴"),
    ("status.snapshot.incidents", "進行中のインシデント"),
    ("status.snapshot.label", "稼働スナップショット"),
    ("status.snapshot.maintenance", "メンテナンス予定"),
    ("status.snapshot.services", "公開サービス"),
    ("status.state.degraded", "低下"),
    ("status.state.down", "停止"),
    ("status.state.maintenance", "メンテナンス"),
    ("status.state.operational", "正常"),
    ("status.sub", "Steadholme 主権インフラのライブ可用性。"),
    (
        "status.subscribe.body",
        "インシデントの作成または更新時に、署名付き JSON webhook POST を受け取ります。",
    ),
    ("status.subscribe.button", "購読"),
    ("status.subscribe.field", "Webhook URL"),
    (
        "status.subscribe.note",
        "Webhook のみです。Beacon には送信メール経路がありません。配信前に確認します。",
    ),
    ("status.subscribe.title", "更新を購読"),
    ("status.timeline", "タイムライン ({n})"),
    ("status.title", "システムステータス"),
    ("status.topbar", "システムステータス"),
    ("status.updated", "{time} に更新"),
    (
        "status.uptime.average_all",
        "過去 {days} 日間の公開サービス平均稼働率",
    ),
    (
        "status.uptime.average_group",
        "過去 {days} 日間のグループ平均稼働率",
    ),
    ("status.uptime.component_title", "過去 {days} 日間の稼働率"),
    ("status.uptime.days_ago", "{days} 日前"),
    (
        "status.uptime.legend_empty",
        "稼働率データはまだありません · 時刻は UTC",
    ),
    (
        "status.uptime.legend_summary",
        "稼働率 {uptime}% · 時刻は UTC",
    ),
    ("status.uptime.monitoring_since", " · 監視開始 {date}"),
    ("status.uptime.today", "今日"),
    ("status.uptime.window", "稼働率 · {days} 日間"),
    ("status.webhook", "Webhook 通知"),
    ("time.d_ago", "{n}日前"),
    ("time.duration.dh", "{d}日{h}時間"),
    ("time.duration.hm", "{h}時間{m}分"),
    ("time.duration.m", "{m}分"),
    ("time.h_ago", "{n}時間前"),
    ("time.just_now", "たった今"),
    ("time.m_ago", "{n}分前"),
    ("time.now", "今"),
    ("time.s_ago", "{n}秒前"),
    ("time.under_minute", "1 分未満"),
];

pub fn t(loc: Locale, key: &'static str) -> &'static str {
    find(table(loc), key)
        .or_else(|| find(EN, key))
        .unwrap_or_else(|| odyssey::t(loc, key))
}

pub fn tf(loc: Locale, key: &'static str, args: &[(&str, &str)]) -> String {
    let mut s = t(loc, key).to_string();
    for (name, value) in args {
        s = s.replace(&format!("{{{name}}}"), value);
    }
    s
}

fn table(loc: Locale) -> &'static [(&'static str, &'static str)] {
    match loc {
        Locale::En => EN,
        Locale::Zh => ZH,
        Locale::Ja => JA,
    }
}

fn find(table: &'static [(&'static str, &'static str)], key: &str) -> Option<&'static str> {
    table
        .binary_search_by(|(k, _)| (*k).cmp(key))
        .ok()
        .map(|i| table[i].1)
        .filter(|v| !v.is_empty())
}

#[cfg(test)]
mod tests {
    use super::{EN, JA, ZH};

    #[test]
    fn locale_catalogs_are_strictly_sorted_and_cover_the_english_keys() {
        for (name, table) in [("en", EN), ("zh", ZH), ("ja", JA)] {
            for pair in table.windows(2) {
                assert!(
                    pair[0].0 < pair[1].0,
                    "{name} catalog keys must be sorted and unique: {} then {}",
                    pair[0].0,
                    pair[1].0
                );
            }
        }

        let en_keys: Vec<_> = EN.iter().map(|(key, _)| *key).collect();
        for (name, table) in [("zh", ZH), ("ja", JA)] {
            let keys: Vec<_> = table.iter().map(|(key, _)| *key).collect();
            assert_eq!(keys, en_keys, "{name} catalog must cover every English key");
        }
    }
}
