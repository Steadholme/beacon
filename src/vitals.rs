//! Beacon-side client/cache for the internal vitals service.
//!
//! The public surface is deliberately type-limited: [`InfraPublic`] contains only aggregate
//! percentages and band words. Host ids, byte counters, and anomalies stay on [`VitalsSnapshot`]
//! for the admin renderer.

use std::collections::{BTreeMap, HashMap};
use std::io;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpStream;

use crate::probe::{parse_http_url, parse_status_line, tls_connector};

pub const STALE_HOST_SECS: i64 = 90;
pub const SNAPSHOT_TTL_SECS: i64 = 180;
pub const BAND_ELEVATED_PCT: f64 = 70.0;
pub const BAND_HIGH_PCT: f64 = 90.0;
pub const TREND_BUCKET_SECS: i64 = 3_600;
pub const TREND_BUCKETS: usize = 24;
pub const FETCH_OVERLAP_SECS: i64 = 120;
pub const BODY_CAP: usize = 4 * 1024 * 1024;

const WINDOW_SECS: i64 = TREND_BUCKET_SECS * TREND_BUCKETS as i64;
const CPU: &str = "cpu_pct";
const MEM: &str = "mem_pct";
const DISK: &str = "disk_pct";

#[derive(Clone, Debug)]
pub struct VitalsSnapshot {
    pub fetched_at: i64,
    pub overall: &'static str,
    pub bands: [BandPublic; 3],
    pub trend: Vec<&'static str>,
    pub hosts: Vec<HostVitals>,
    pub anomalies: Vec<AnomalyRow>,
}

#[derive(Clone, Copy, Debug, Serialize)]
pub struct BandPublic {
    pub metric: &'static str,
    pub band: &'static str,
    pub worst_pct: Option<f64>,
}

#[derive(Clone, Debug)]
pub struct HostVitals {
    pub host: String,
    pub last_ts: i64,
    pub stale: bool,
    pub cpu_pct: Option<f64>,
    pub mem_pct: Option<f64>,
    pub disk_pct: Option<f64>,
    pub load1: Option<f64>,
    pub load5: Option<f64>,
    pub load15: Option<f64>,
    pub net_rx_bps: Option<f64>,
    pub net_tx_bps: Option<f64>,
    pub mem_used_bytes: Option<f64>,
    pub mem_total_bytes: Option<f64>,
    pub disk_used_bytes: Option<f64>,
    pub disk_total_bytes: Option<f64>,
    pub uptime_secs: Option<f64>,
    pub cpu_series: Vec<f64>,
    pub anomalies_24h: usize,
}

#[derive(Clone, Debug)]
pub struct AnomalyRow {
    pub host: String,
    pub metric: String,
    pub ts: i64,
    pub value: f64,
    pub score: f64,
    pub note: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct InfraPublic {
    pub overall: &'static str,
    pub bands: Vec<BandPublic>,
    pub trend: Vec<&'static str>,
}

pub struct VitalsHandle {
    base_url: String,
    inner: Mutex<Inner>,
}

#[derive(Default)]
struct Inner {
    latest: HashMap<String, HashMap<String, (i64, f64)>>,
    rings: HashMap<String, HostRings>,
    anomalies: Vec<AnomalyRow>,
    snapshot: Option<Arc<VitalsSnapshot>>,
    backfilled: bool,
}

#[derive(Default)]
struct HostRings {
    cpu: BTreeMap<i64, f64>,
    mem: BTreeMap<i64, f64>,
    disk: BTreeMap<i64, f64>,
}

#[derive(Debug, Deserialize)]
struct MetricsResp {
    #[serde(default)]
    samples: Vec<SampleIn>,
}

#[derive(Debug, Deserialize)]
struct SampleIn {
    host: String,
    metric: String,
    value: f64,
    ts: i64,
}

#[derive(Debug, Deserialize)]
struct AnomaliesResp {
    #[serde(default)]
    anomalies: Vec<AnomalyIn>,
}

#[derive(Debug, Deserialize)]
struct AnomalyIn {
    host: String,
    metric: String,
    ts: i64,
    value: f64,
    score: f64,
    #[serde(default)]
    note: String,
}

impl VitalsHandle {
    pub fn new(base_url: String) -> Self {
        Self {
            base_url: base_url.trim_end_matches('/').to_string(),
            inner: Mutex::new(Inner::default()),
        }
    }

    pub async fn refresh(&self, timeout: Duration, now: i64) {
        let backfilled = self.inner.lock().expect("vitals lock poisoned").backfilled;
        let anomalies_url = self.url("/api/anomalies");

        let (samples, metrics_ok, anomalies) = if backfilled {
            let metrics_url = self.url(&format!("/api/metrics?since={}", now - FETCH_OVERLAP_SECS));
            let (metrics, anomalies) = tokio::join!(
                fetch_metrics(&metrics_url, timeout),
                fetch_anomalies(&anomalies_url, timeout)
            );
            let (samples, ok) = collect_metrics([metrics]);
            (samples, ok, collect_anomalies(anomalies))
        } else {
            let since = now - WINDOW_SECS;
            let cpu_url = self.url(&format!("/api/metrics?metric={CPU}&since={since}"));
            let mem_url = self.url(&format!("/api/metrics?metric={MEM}&since={since}"));
            let disk_url = self.url(&format!("/api/metrics?metric={DISK}&since={since}"));
            let latest_url = self.url(&format!("/api/metrics?since={}", now - FETCH_OVERLAP_SECS));
            let (cpu, mem, disk, latest, anomalies) = tokio::join!(
                fetch_metrics(&cpu_url, timeout),
                fetch_metrics(&mem_url, timeout),
                fetch_metrics(&disk_url, timeout),
                fetch_metrics(&latest_url, timeout),
                fetch_anomalies(&anomalies_url, timeout)
            );
            let (samples, ok) = collect_metrics([cpu, mem, disk, latest]);
            (samples, ok, collect_anomalies(anomalies))
        };

        if !metrics_ok && anomalies.is_none() {
            return;
        }
        let mut inner = self.inner.lock().expect("vitals lock poisoned");
        merge_samples(&mut inner, samples, now);
        if let Some(mut rows) = anomalies {
            rows.sort_by_key(|a| std::cmp::Reverse(a.ts));
            rows.truncate(50);
            inner.anomalies = rows;
        }
        if metrics_ok {
            inner.backfilled = true;
        }
        inner.snapshot = Some(Arc::new(compute_snapshot(&inner, now)));
    }

    pub fn snapshot(&self) -> Option<Arc<VitalsSnapshot>> {
        self.inner
            .lock()
            .expect("vitals lock poisoned")
            .snapshot
            .clone()
    }

    fn url(&self, path: &str) -> String {
        format!("{}{path}", self.base_url)
    }
}

fn collect_metrics<const N: usize>(results: [io::Result<MetricsResp>; N]) -> (Vec<SampleIn>, bool) {
    let mut samples = Vec::new();
    let mut ok = false;
    for res in results {
        match res {
            Ok(resp) => {
                ok = true;
                samples.extend(resp.samples);
            }
            Err(e) => tracing::debug!(error = %e, "vitals metrics fetch failed"),
        }
    }
    (samples, ok)
}

fn collect_anomalies(result: io::Result<AnomaliesResp>) -> Option<Vec<AnomalyRow>> {
    match result {
        Ok(resp) => Some(
            resp.anomalies
                .into_iter()
                .map(|a| AnomalyRow {
                    host: a.host,
                    metric: a.metric,
                    ts: a.ts,
                    value: a.value,
                    score: a.score,
                    note: a.note,
                })
                .collect(),
        ),
        Err(e) => {
            tracing::debug!(error = %e, "vitals anomalies fetch failed");
            None
        }
    }
}

async fn fetch_metrics(url: &str, timeout: Duration) -> io::Result<MetricsResp> {
    let body = get_body(url, timeout).await?;
    serde_json::from_str(&body).map_err(|e| io_err(&format!("invalid vitals metrics JSON: {e}")))
}

async fn fetch_anomalies(url: &str, timeout: Duration) -> io::Result<AnomaliesResp> {
    let body = get_body(url, timeout).await?;
    serde_json::from_str(&body).map_err(|e| io_err(&format!("invalid vitals anomalies JSON: {e}")))
}

async fn get_body(url: &str, timeout: Duration) -> io::Result<String> {
    match tokio::time::timeout(timeout, get_body_inner(url)).await {
        Ok(res) => res,
        Err(_) => Err(io_err("vitals request timed out")),
    }
}

async fn get_body_inner(url: &str) -> io::Result<String> {
    let (tls, host, port, path) =
        parse_http_url(url).ok_or_else(|| io_err("invalid vitals URL"))?;
    let tcp = TcpStream::connect((host.as_str(), port)).await?;
    if tls {
        let server_name = rustls::pki_types::ServerName::try_from(host.clone())
            .map_err(|_| io_err("invalid TLS server name"))?;
        let stream = tls_connector().connect(server_name, tcp).await?;
        send_recv_body(stream, &host, &path).await
    } else {
        send_recv_body(tcp, &host, &path).await
    }
}

async fn send_recv_body<S>(mut stream: S, host: &str, path: &str) -> io::Result<String>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let request = format!(
        "GET {path} HTTP/1.0\r\nHost:{host}\r\nUser-Agent:beacon/0.1\r\nAccept:application/json\r\nConnection:close\r\n\r\n"
    );
    stream.write_all(request.as_bytes()).await?;
    stream.flush().await?;

    let mut acc = Vec::with_capacity(4096);
    let mut buf = [0u8; 8192];
    loop {
        let n = stream.read(&mut buf).await?;
        if n == 0 {
            break;
        }
        if acc.len().saturating_add(n) > BODY_CAP {
            return Err(io_err("vitals response body too large"));
        }
        acc.extend_from_slice(&buf[..n]);
    }

    let header_end = acc
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .map(|i| i + 4)
        .ok_or_else(|| io_err("vitals response missing headers"))?;
    let headers = &acc[..header_end];
    let status = parse_status_line(headers)?;
    if status != 200 {
        return Err(io_err(&format!("vitals response status {status}")));
    }
    let headers_text = String::from_utf8_lossy(headers).to_ascii_lowercase();
    if headers_text.contains("transfer-encoding: chunked") {
        return Err(io_err("vitals response used chunked transfer encoding"));
    }
    String::from_utf8(acc[header_end..].to_vec())
        .map_err(|e| io_err(&format!("vitals response body is not UTF-8: {e}")))
}

fn merge_samples(inner: &mut Inner, samples: Vec<SampleIn>, now: i64) {
    let min_ts = now - WINDOW_SECS;
    for s in samples {
        if s.host.is_empty() || s.metric.is_empty() || !s.value.is_finite() {
            continue;
        }
        let latest = inner
            .latest
            .entry(s.host.clone())
            .or_default()
            .entry(s.metric.clone())
            .or_insert((i64::MIN, 0.0));
        if s.ts >= latest.0 {
            *latest = (s.ts, s.value);
        }
        let ring = inner.rings.entry(s.host).or_default();
        match s.metric.as_str() {
            CPU => {
                ring.cpu.insert(s.ts, s.value);
                ring.cpu.retain(|ts, _| *ts >= min_ts);
            }
            MEM => {
                ring.mem.insert(s.ts, s.value);
                ring.mem.retain(|ts, _| *ts >= min_ts);
            }
            DISK => {
                ring.disk.insert(s.ts, s.value);
                ring.disk.retain(|ts, _| *ts >= min_ts);
            }
            _ => {}
        }
    }
}

fn compute_snapshot(inner: &Inner, now: i64) -> VitalsSnapshot {
    let cpu_band = metric_band(inner, CPU, "cpu", now);
    let mem_band = metric_band(inner, MEM, "memory", now);
    let disk_band = metric_band(inner, DISK, "disk", now);
    let bands = [cpu_band, mem_band, disk_band];
    let overall = worst_band(bands.iter().map(|b| b.band));
    let trend = compute_trend(&inner.rings, now);
    let mut hosts: Vec<HostVitals> = inner
        .latest
        .keys()
        .map(|host| host_vitals(host, inner, now))
        .collect();
    hosts.sort_by(|a, b| a.host.cmp(&b.host));
    let mut anomalies = inner.anomalies.clone();
    anomalies.sort_by_key(|a| std::cmp::Reverse(a.ts));
    anomalies.truncate(50);

    VitalsSnapshot {
        fetched_at: now,
        overall,
        bands,
        trend,
        hosts,
        anomalies,
    }
}

fn metric_band(
    inner: &Inner,
    metric: &'static str,
    public_metric: &'static str,
    now: i64,
) -> BandPublic {
    let worst = inner
        .latest
        .values()
        .filter_map(|metrics| fresh_metric(metrics, metric, now))
        .reduce(f64::max)
        .map(round1);
    BandPublic {
        metric: public_metric,
        band: worst.map(band_class).unwrap_or("unknown"),
        worst_pct: worst,
    }
}

fn fresh_metric(metrics: &HashMap<String, (i64, f64)>, metric: &str, now: i64) -> Option<f64> {
    metrics
        .get(metric)
        .filter(|(ts, value)| *ts >= now - STALE_HOST_SECS && value.is_finite())
        .map(|(_, value)| *value)
}

fn host_vitals(host: &str, inner: &Inner, now: i64) -> HostVitals {
    let metrics = inner.latest.get(host);
    let last_ts = metrics
        .map(|m| m.values().map(|(ts, _)| *ts).max().unwrap_or(0))
        .unwrap_or(0);
    let rings = inner.rings.get(host);
    let cpu_series = rings
        .map(|r| {
            r.cpu
                .range((now - TREND_BUCKET_SECS)..=now)
                .map(|(_, v)| round1(*v))
                .collect()
        })
        .unwrap_or_default();
    let anomalies_24h = inner
        .anomalies
        .iter()
        .filter(|a| a.host == host && a.ts >= now - 86_400)
        .count();
    HostVitals {
        host: host.to_string(),
        last_ts,
        stale: last_ts < now - STALE_HOST_SECS,
        cpu_pct: metric_value(metrics, CPU),
        mem_pct: metric_value(metrics, MEM),
        disk_pct: metric_value(metrics, DISK),
        load1: metric_value(metrics, "load1"),
        load5: metric_value(metrics, "load5"),
        load15: metric_value(metrics, "load15"),
        net_rx_bps: metric_value(metrics, "net_rx_bps"),
        net_tx_bps: metric_value(metrics, "net_tx_bps"),
        mem_used_bytes: metric_value(metrics, "mem_used_bytes"),
        mem_total_bytes: metric_value(metrics, "mem_total_bytes"),
        disk_used_bytes: metric_value(metrics, "disk_used_bytes"),
        disk_total_bytes: metric_value(metrics, "disk_total_bytes"),
        uptime_secs: metric_value(metrics, "uptime_secs"),
        cpu_series,
        anomalies_24h,
    }
}

fn metric_value(metrics: Option<&HashMap<String, (i64, f64)>>, metric: &str) -> Option<f64> {
    metrics
        .and_then(|m| m.get(metric))
        .map(|(_, value)| *value)
        .filter(|value| value.is_finite())
        .map(round1)
}

fn compute_trend(rings: &HashMap<String, HostRings>, now: i64) -> Vec<&'static str> {
    let current = now / TREND_BUCKET_SECS;
    let mut out = Vec::with_capacity(TREND_BUCKETS);
    for bucket in (current - TREND_BUCKETS as i64 + 1)..=current {
        let mut bands = Vec::new();
        for ring in rings.values() {
            push_bucket_bands(&ring.cpu, bucket, &mut bands);
            push_bucket_bands(&ring.mem, bucket, &mut bands);
            push_bucket_bands(&ring.disk, bucket, &mut bands);
        }
        out.push(worst_band(bands));
    }
    out
}

fn push_bucket_bands(series: &BTreeMap<i64, f64>, bucket: i64, bands: &mut Vec<&'static str>) {
    let start = bucket * TREND_BUCKET_SECS;
    let end = start + TREND_BUCKET_SECS;
    for (_, value) in series.range(start..end) {
        bands.push(band_class(*value));
    }
}

pub fn band_class(pct: f64) -> &'static str {
    if pct >= BAND_HIGH_PCT {
        "high"
    } else if pct >= BAND_ELEVATED_PCT {
        "elevated"
    } else {
        "ok"
    }
}

fn band_rank(band: &str) -> u8 {
    match band {
        "high" => 3,
        "elevated" => 2,
        "ok" => 1,
        _ => 0,
    }
}

fn worst_band<I>(bands: I) -> &'static str
where
    I: IntoIterator<Item = &'static str>,
{
    bands
        .into_iter()
        .max_by_key(|band| band_rank(band))
        .unwrap_or("unknown")
}

fn round1(value: f64) -> f64 {
    (value * 10.0).round() / 10.0
}

pub fn public_infra(snap: &VitalsSnapshot, now: i64) -> Option<InfraPublic> {
    if now.saturating_sub(snap.fetched_at) > SNAPSHOT_TTL_SECS {
        return None;
    }
    if snap.bands.iter().all(|b| b.band == "unknown") {
        return None;
    }
    Some(InfraPublic {
        overall: snap.overall,
        bands: snap.bands.to_vec(),
        trend: snap.trend.clone(),
    })
}

fn io_err(msg: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, msg.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn inner_with(host: &str, metric: &str, ts: i64, value: f64) -> Inner {
        let mut inner = Inner::default();
        merge_samples(
            &mut inner,
            vec![SampleIn {
                host: host.to_string(),
                metric: metric.to_string(),
                ts,
                value,
            }],
            ts,
        );
        inner
    }

    #[test]
    fn band_class_boundaries() {
        assert_eq!(band_class(69.9), "ok");
        assert_eq!(band_class(70.0), "elevated");
        assert_eq!(band_class(89.9), "elevated");
        assert_eq!(band_class(90.0), "high");
    }

    #[test]
    fn snapshot_uses_worst_band() {
        let mut inner = inner_with("h1", CPU, 100, 65.0);
        merge_samples(
            &mut inner,
            vec![SampleIn {
                host: "h2".to_string(),
                metric: MEM.to_string(),
                ts: 100,
                value: 91.0,
            }],
            100,
        );
        let snap = compute_snapshot(&inner, 100);
        assert_eq!(snap.bands[0].band, "ok");
        assert_eq!(snap.bands[1].band, "high");
        assert_eq!(snap.overall, "high");
    }

    #[test]
    fn unknown_never_outranks_real_band() {
        let inner = inner_with("h1", CPU, 100, 71.0);
        let snap = compute_snapshot(&inner, 100);
        assert_eq!(snap.bands[0].band, "elevated");
        assert_eq!(snap.bands[1].band, "unknown");
        assert_eq!(snap.overall, "elevated");
    }

    #[test]
    fn trend_buckets_take_worst_and_keep_gaps() {
        let now = TREND_BUCKET_SECS * 100 + 120;
        let mut inner = Inner::default();
        merge_samples(
            &mut inner,
            vec![
                SampleIn {
                    host: "h1".to_string(),
                    metric: CPU.to_string(),
                    ts: now - 10,
                    value: 20.0,
                },
                SampleIn {
                    host: "h2".to_string(),
                    metric: DISK.to_string(),
                    ts: now - 20,
                    value: 95.0,
                },
                SampleIn {
                    host: "h1".to_string(),
                    metric: MEM.to_string(),
                    ts: now - (TREND_BUCKET_SECS * 2),
                    value: 75.0,
                },
            ],
            now,
        );
        let snap = compute_snapshot(&inner, now);
        assert_eq!(snap.trend.len(), TREND_BUCKETS);
        assert_eq!(snap.trend[TREND_BUCKETS - 1], "high");
        assert_eq!(snap.trend[TREND_BUCKETS - 2], "unknown");
        assert_eq!(snap.trend[TREND_BUCKETS - 3], "elevated");
    }

    #[test]
    fn round1_rounds_to_one_decimal() {
        assert_eq!(round1(12.34), 12.3);
        assert_eq!(round1(12.35), 12.4);
    }

    #[test]
    fn public_infra_hides_stale_and_all_unknown() {
        let mut snap = compute_snapshot(&Inner::default(), 100);
        assert!(public_infra(&snap, 100).is_none());
        let inner = inner_with("h1", CPU, 100, 10.0);
        snap = compute_snapshot(&inner, 100);
        assert!(public_infra(&snap, 200).is_some());
        assert!(public_infra(&snap, 281).is_none());
    }
}
