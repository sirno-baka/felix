use getrandom::register_custom_getrandom;
use rustls::{ClientConfig, RootCertStore};
use std::collections::BTreeSet;
use std::error::Error;
use std::net::ToSocketAddrs;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

#[cfg(target_os = "popugos")]
const SYS_GETRANDOM: u32 = 355;
#[cfg(target_os = "popugos")]
const SYS_CLOCK_GETTIME: u32 = 265;
static WORKERS_STARTED: AtomicUsize = AtomicUsize::new(0);

#[cfg(target_os = "popugos")]
#[repr(C)]
#[derive(Default)]
struct TimeSpec {
    tv_sec: i32,
    tv_nsec: i32,
}

#[cfg(target_os = "popugos")]
fn monotonic_ms() -> u64 {
    let mut ts = TimeSpec::default();
    let ret: i32;
    unsafe {
        core::arch::asm!(
            "int 0x80",
            inlateout("eax") SYS_CLOCK_GETTIME => ret,
            in("ebx") 1u32,
            in("ecx") &mut ts as *mut TimeSpec,
            options(nostack, preserves_flags)
        );
    }
    if ret < 0 {
        return 0;
    }
    (ts.tv_sec.max(0) as u64)
        .saturating_mul(1000)
        .saturating_add((ts.tv_nsec.max(0) as u64) / 1_000_000)
}

#[cfg(not(target_os = "popugos"))]
fn monotonic_ms() -> u64 {
    static START: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
    START.get_or_init(Instant::now).elapsed().as_millis() as u64
}

macro_rules! bench_log {
    ($origin:expr, $($arg:tt)*) => {{
        println!(
            "[up={:>8}ms +{:>8}ms] {}",
            monotonic_ms(),
            $origin.elapsed().as_millis(),
            format_args!($($arg)*)
        );
    }};
}

#[cfg(target_os = "popugos")]
fn popugos_getrandom(dest: &mut [u8]) -> Result<(), getrandom::Error> {
    let mut offset = 0usize;
    while offset < dest.len() {
        let ret: i32;
        unsafe {
            core::arch::asm!(
                "int 0x80",
                inlateout("eax") SYS_GETRANDOM => ret,
                in("ebx") dest.as_mut_ptr().add(offset),
                in("ecx") dest.len() - offset,
                in("edx") 0u32,
                options(nostack, preserves_flags)
            );
        }
        if ret <= 0 {
            return Err(getrandom::Error::UNSUPPORTED);
        }
        offset += ret as usize;
    }
    Ok(())
}

#[cfg(not(target_os = "popugos"))]
fn popugos_getrandom(_dest: &mut [u8]) -> Result<(), getrandom::Error> {
    Err(getrandom::Error::UNSUPPORTED)
}

register_custom_getrandom!(popugos_getrandom);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BodyMode {
    Bytes,
    Chunks,
}

impl BodyMode {
    fn parse(value: &str) -> Result<Self, String> {
        match value {
            "bytes" => Ok(Self::Bytes),
            "chunks" | "chunk" => Ok(Self::Chunks),
            _ => Err(format!("invalid --body {value:?}; expected bytes|chunks")),
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Bytes => "bytes",
            Self::Chunks => "chunks",
        }
    }
}

#[derive(Clone, Debug)]
struct Config {
    url: String,
    threads: usize,
    concurrency: usize,
    requests: usize,
    body: BodyMode,
    connect_timeout: Duration,
    read_timeout: Duration,
    timeout: Duration,
    pool_idle_timeout: Duration,
    pool_max_idle: usize,
    tls_fragment: Option<usize>,
    close: bool,
    dns_preflight: bool,
    chunk_log_every: usize,
    chunk_delay: Duration,
    yield_chunks: bool,
    limit: Option<usize>,
    quiet: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            url: String::new(),
            threads: 1,
            concurrency: 1,
            requests: 1,
            body: BodyMode::Chunks,
            connect_timeout: Duration::from_secs(10),
            read_timeout: Duration::from_secs(10),
            timeout: Duration::from_secs(30),
            pool_idle_timeout: Duration::from_secs(5),
            pool_max_idle: 1,
            tls_fragment: None,
            close: false,
            dns_preflight: false,
            chunk_log_every: 0,
            chunk_delay: Duration::ZERO,
            yield_chunks: false,
            limit: None,
            quiet: false,
        }
    }
}

#[derive(Debug)]
struct RequestStat {
    id: usize,
    ok: bool,
    status: Option<u16>,
    bytes: u64,
    chunks: u64,
    started_up_ms: u64,
    headers_up_ms: Option<u64>,
    first_body_up_ms: Option<u64>,
    finished_up_ms: u64,
    headers_ms: u128,
    first_body_ms: Option<u128>,
    total_ms: u128,
    start_thread: String,
    end_thread: String,
    error: Option<String>,
}

fn usage() {
    println!(
        "netbench - Tokio/reqwest/NIC stress tester\n\
\n\
USAGE:\n\
  netbench [options] URL\n\
\n\
URL must be the last argument.\n\
\n\
OPTIONS:\n\
  --threads N             Tokio runtime threads; 1 = current-thread [1]\n\
  --concurrency N         Number of concurrent request futures [1]\n\
  --requests N            Total requests [1]\n\
  --body chunks|bytes     Stream with response.chunk() or response.bytes() [chunks]\n\
  --dns                   Resolve URL host with ToSocketAddrs before the test\n\
  --close                 Send Connection: close (disable reuse for requests)\n\
  --pool-max-idle N       Reqwest idle connections per host [1]\n\
  --pool-idle-ms N        Reqwest idle connection timeout [5000]\n\
  --connect-ms N          Connect timeout [10000]\n\
  --read-ms N             Read timeout [10000]\n\
  --timeout-ms N          Whole request timeout [30000]\n\
  --tls-fragment N        rustls max_fragment_size; 0 = default\n\
  --limit BYTES           In chunks mode stop after at least this many body bytes\n\
  --chunk-log N           In chunks mode print every Nth received chunk [0=off]\n\
  --chunk-delay-ms N      Sleep after every received chunk [0]\n\
  --yield                 Tokio yield_now() after every received chunk\n\
  --quiet                 Only config/errors/final summary\n\
  -h, --help              Show this help\n\
\n\
EXAMPLES:\n\
  netbench --dns --body bytes https://example.com/file.bin\n\
  netbench --threads 4 --concurrency 8 --requests 32 --body chunks https://example.com/file.bin\n\
  netbench --threads 4 --concurrency 16 --requests 64 --tls-fragment 512 https://example.com/file.bin\n\
  netbench --threads 2 --concurrency 8 --requests 32 --close --body chunks https://example.com/file.bin"
    );
}

fn parse_usize(name: &str, value: &str) -> Result<usize, String> {
    value
        .parse::<usize>()
        .map_err(|_| format!("invalid {name} value {value:?}"))
}

fn parse_args() -> Result<Option<Config>, String> {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() {
        usage();
        return Ok(None);
    }
    if args.iter().any(|arg| arg == "-h" || arg == "--help") {
        usage();
        return Ok(None);
    }

    let url = args
        .pop()
        .ok_or_else(|| "missing URL".to_string())?;
    if url.starts_with('-') {
        return Err("URL must be the final argument".to_string());
    }

    let mut cfg = Config {
        url,
        ..Config::default()
    };

    let mut i = 0usize;
    while i < args.len() {
        let arg = &args[i];
        let (name, inline_value) = match arg.split_once('=') {
            Some((name, value)) => (name, Some(value)),
            None => (arg.as_str(), None),
        };

        macro_rules! value {
            () => {{
                if let Some(value) = inline_value {
                    value
                } else {
                    i += 1;
                    args.get(i)
                        .map(String::as_str)
                        .ok_or_else(|| format!("missing value for {name}"))?
                }
            }};
        }

        match name {
            "--threads" => cfg.threads = parse_usize(name, value!())?,
            "--concurrency" => cfg.concurrency = parse_usize(name, value!())?,
            "--requests" => cfg.requests = parse_usize(name, value!())?,
            "--body" => cfg.body = BodyMode::parse(value!())?,
            "--connect-ms" => {
                cfg.connect_timeout = Duration::from_millis(parse_usize(name, value!())? as u64)
            }
            "--read-ms" => {
                cfg.read_timeout = Duration::from_millis(parse_usize(name, value!())? as u64)
            }
            "--timeout-ms" => {
                cfg.timeout = Duration::from_millis(parse_usize(name, value!())? as u64)
            }
            "--pool-idle-ms" => {
                cfg.pool_idle_timeout =
                    Duration::from_millis(parse_usize(name, value!())? as u64)
            }
            "--pool-max-idle" => cfg.pool_max_idle = parse_usize(name, value!())?,
            "--tls-fragment" => {
                let size = parse_usize(name, value!())?;
                cfg.tls_fragment = (size != 0).then_some(size);
            }
            "--chunk-log" => cfg.chunk_log_every = parse_usize(name, value!())?,
            "--chunk-delay-ms" => {
                cfg.chunk_delay = Duration::from_millis(parse_usize(name, value!())? as u64)
            }
            "--limit" => cfg.limit = Some(parse_usize(name, value!())?),
            "--dns" => cfg.dns_preflight = true,
            "--close" => cfg.close = true,
            "--yield" => cfg.yield_chunks = true,
            "--quiet" => cfg.quiet = true,
            _ => return Err(format!("unknown option {name:?}; use --help")),
        }

        i += 1;
    }

    if cfg.threads == 0 {
        return Err("--threads must be >= 1".to_string());
    }
    if cfg.concurrency == 0 {
        return Err("--concurrency must be >= 1".to_string());
    }
    if cfg.requests == 0 {
        return Err("--requests must be >= 1".to_string());
    }
    cfg.concurrency = cfg.concurrency.min(cfg.requests);

    let parsed = reqwest::Url::parse(&cfg.url).map_err(|error| format!("invalid URL: {error}"))?;
    if parsed.scheme() != "http" && parsed.scheme() != "https" {
        return Err("URL scheme must be http or https".to_string());
    }

    Ok(Some(cfg))
}

fn tls_config(fragment: Option<usize>) -> Result<ClientConfig, Box<dyn Error>> {
    let mut roots = RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());

    let provider = Arc::new(rustls_rustcrypto::provider());
    let mut config = ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()?
        .with_root_certificates(roots)
        .with_no_client_auth();

    config.alpn_protocols = vec![b"http/1.1".to_vec()];
    config.max_fragment_size = fragment;
    Ok(config)
}

fn make_client(cfg: &Config) -> Result<reqwest::Client, Box<dyn Error>> {
    Ok(reqwest::Client::builder()
        .use_preconfigured_tls(tls_config(cfg.tls_fragment)?)
        .pool_max_idle_per_host(cfg.pool_max_idle)
        .pool_idle_timeout(cfg.pool_idle_timeout)
        .connect_timeout(cfg.connect_timeout)
        .read_timeout(cfg.read_timeout)
        .timeout(cfg.timeout)
        .build()?)
}

fn dns_preflight(origin: Instant, url: &str) -> Result<(), Box<dyn Error>> {
    let parsed = reqwest::Url::parse(url)?;
    let host = parsed
        .host_str()
        .ok_or_else(|| "URL has no host".to_string())?;
    let port = parsed.port_or_known_default().unwrap_or(443);
    let started = Instant::now();
    let addresses: Vec<_> = (host, port).to_socket_addrs()?.collect();
    bench_log!(
        origin,
        "[dns] {host}:{port} -> {addresses:?} in {}ms",
        started.elapsed().as_millis()
    );
    Ok(())
}

async fn one_request(
    origin: Instant,
    id: usize,
    client: reqwest::Client,
    cfg: Arc<Config>,
) -> RequestStat {
    let started = Instant::now();
    let started_up_ms = monotonic_ms();
    let start_thread = format!("{:?}", std::thread::current().id());
    if !cfg.quiet {
        bench_log!(origin, "[req #{id:03}] start tid={start_thread}");
    }

    let mut request = client.get(&cfg.url);
    if cfg.close {
        request = request.header(reqwest::header::CONNECTION, "close");
    }

    let response = match request.send().await {
        Ok(response) => response,
        Err(error) => {
            let total_ms = started.elapsed().as_millis();
            let finished_up_ms = monotonic_ms();
            let end_thread = format!("{:?}", std::thread::current().id());
            bench_log!(
                origin,
                "[req #{id:03}] send ERROR after={total_ms}ms tid={start_thread}->{end_thread}: {error}"
            );
            return RequestStat {
                id,
                ok: false,
                status: None,
                bytes: 0,
                chunks: 0,
                started_up_ms,
                headers_up_ms: None,
                first_body_up_ms: None,
                finished_up_ms,
                headers_ms: total_ms,
                first_body_ms: None,
                total_ms,
                start_thread,
                end_thread,
                error: Some(error.to_string()),
            };
        }
    };

    let headers_ms = started.elapsed().as_millis();
    let headers_up_ms = monotonic_ms();
    let status = response.status().as_u16();
    if !cfg.quiet {
        bench_log!(
            origin,
            "[req #{id:03}] headers status={status} after={headers_ms}ms tid={:?}",
            std::thread::current().id()
        );
    }
    let mut bytes = 0u64;
    let mut chunks = 0u64;
    let mut first_body_ms = None;
    let mut first_body_up_ms = None;
    let mut error = None;

    match cfg.body {
        BodyMode::Bytes => match response.bytes().await {
            Ok(body) => {
                bytes = body.len() as u64;
                chunks = if body.is_empty() { 0 } else { 1 };
                if !body.is_empty() {
                    let first_ms = started.elapsed().as_millis();
                    first_body_ms = Some(first_ms);
                    first_body_up_ms = Some(monotonic_ms());
                    if !cfg.quiet {
                        bench_log!(
                            origin,
                            "[req #{id:03}] body bytes={} first/complete after={}ms tid={:?}",
                            body.len(),
                            first_ms,
                            std::thread::current().id()
                        );
                    }
                }
            }
            Err(body_error) => error = Some(body_error.to_string()),
        },
        BodyMode::Chunks => {
            let mut response = response;
            loop {
                match response.chunk().await {
                    Ok(Some(chunk)) => {
                        if first_body_ms.is_none() {
                            let first_ms = started.elapsed().as_millis();
                            first_body_ms = Some(first_ms);
                            first_body_up_ms = Some(monotonic_ms());
                            if !cfg.quiet {
                                bench_log!(
                                    origin,
                                    "[req #{id:03}] first-body +{}B after={}ms tid={:?}",
                                    chunk.len(),
                                    first_ms,
                                    std::thread::current().id()
                                );
                            }
                        }
                        chunks = chunks.saturating_add(1);
                        bytes = bytes.saturating_add(chunk.len() as u64);

                        if cfg.chunk_log_every != 0
                            && chunks % cfg.chunk_log_every as u64 == 0
                        {
                            bench_log!(
                                origin,
                                "[chunk #{id}] n={chunks} +{} total={} request_t={}ms tid={:?}",
                                chunk.len(),
                                bytes,
                                started.elapsed().as_millis(),
                                std::thread::current().id()
                            );
                        }

                        if cfg.yield_chunks {
                            tokio::task::yield_now().await;
                        }
                        if !cfg.chunk_delay.is_zero() {
                            tokio::time::sleep(cfg.chunk_delay).await;
                        }
                        if cfg.limit.is_some_and(|limit| bytes >= limit as u64) {
                            break;
                        }
                    }
                    Ok(None) => break,
                    Err(body_error) => {
                        error = Some(body_error.to_string());
                        break;
                    }
                }
            }
        }
    }

    let total_ms = started.elapsed().as_millis();
    let finished_up_ms = monotonic_ms();
    let end_thread = format!("{:?}", std::thread::current().id());
    let ok = error.is_none() && (200..400).contains(&status);
    if !cfg.quiet || !ok {
        bench_log!(
            origin,
            "[req #{id:03}] end status={status} bytes={bytes} chunks={chunks} total={total_ms}ms tid={start_thread}->{end_thread} error={:?}",
            error
        );
    }

    RequestStat {
        id,
        ok,
        status: Some(status),
        bytes,
        chunks,
        started_up_ms,
        headers_up_ms: Some(headers_up_ms),
        first_body_up_ms,
        finished_up_ms,
        headers_ms,
        first_body_ms,
        total_ms,
        start_thread,
        end_thread,
        error,
    }
}

fn print_request(stat: &RequestStat) {
    if let Some(error) = &stat.error {
        println!(
            "[req #{:03}] ERROR status={:?} bytes={} chunks={} headers={}ms total={}ms up={} -> hdr={:?} first={:?} -> {} tid={} -> {} error={}",
            stat.id,
            stat.status,
            stat.bytes,
            stat.chunks,
            stat.headers_ms,
            stat.total_ms,
            stat.started_up_ms,
            stat.headers_up_ms,
            stat.first_body_up_ms,
            stat.finished_up_ms,
            stat.start_thread,
            stat.end_thread,
            error
        );
        return;
    }

    let mbps = if stat.total_ms > 0 {
        stat.bytes as f64 * 8.0 / (stat.total_ms as f64 * 1000.0)
    } else {
        0.0
    };
    println!(
        "[req #{:03}] status={} bytes={} chunks={} headers={}ms first={:?}ms total={}ms {:.2}Mbps up={} -> hdr={:?} first={:?} -> {} tid={} -> {}",
        stat.id,
        stat.status.unwrap_or(0),
        stat.bytes,
        stat.chunks,
        stat.headers_ms,
        stat.first_body_ms,
        stat.total_ms,
        mbps,
        stat.started_up_ms,
        stat.headers_up_ms,
        stat.first_body_up_ms,
        stat.finished_up_ms,
        stat.start_thread,
        stat.end_thread
    );
}

async fn run(origin: Instant, cfg: Arc<Config>, client: reqwest::Client) -> Vec<RequestStat> {
    let wall_started = Instant::now();
    let mut set = tokio::task::JoinSet::new();
    let mut next_id = 1usize;
    let mut stats = Vec::with_capacity(cfg.requests);

    while next_id <= cfg.requests && set.len() < cfg.concurrency {
        let task_cfg = cfg.clone();
        let task_client = client.clone();
        let id = next_id;
        next_id += 1;
        set.spawn(async move { one_request(origin, id, task_client, task_cfg).await });
    }

    loop {
        let Some(joined) = set.join_next().await else {
            break;
        };
        match joined {
            Ok(stat) => {
                if !cfg.quiet || !stat.ok {
                    print_request(&stat);
                }
                stats.push(stat);
            }
            Err(error) => {
                bench_log!(origin, "[task] join error: {error}");
            }
        }

        if next_id <= cfg.requests {
            let task_cfg = cfg.clone();
            let task_client = client.clone();
            let id = next_id;
            next_id += 1;
            set.spawn(async move { one_request(origin, id, task_client, task_cfg).await });
        }
    }

    let elapsed = wall_started.elapsed();
    print_summary(&cfg, &stats, elapsed);
    stats
}

fn print_summary(cfg: &Config, stats: &[RequestStat], elapsed: Duration) {
    let ok = stats.iter().filter(|stat| stat.ok).count();
    let failed = stats.len().saturating_sub(ok);
    let bytes: u64 = stats.iter().map(|stat| stat.bytes).sum();
    let chunks: u64 = stats.iter().map(|stat| stat.chunks).sum();
    let total_request_ms: u128 = stats.iter().map(|stat| stat.total_ms).sum();
    let min_ms = stats.iter().map(|stat| stat.total_ms).min().unwrap_or(0);
    let max_ms = stats.iter().map(|stat| stat.total_ms).max().unwrap_or(0);
    let avg_ms = if stats.is_empty() {
        0.0
    } else {
        total_request_ms as f64 / stats.len() as f64
    };
    let wall_ms = elapsed.as_millis();
    let aggregate_mbps = if wall_ms > 0 {
        bytes as f64 * 8.0 / (wall_ms as f64 * 1000.0)
    } else {
        0.0
    };

    let mut threads = BTreeSet::new();
    for stat in stats {
        threads.insert(stat.start_thread.clone());
        threads.insert(stat.end_thread.clone());
    }

    println!("--- netbench summary ---");
    println!(
        "requests={} ok={} failed={} concurrency={} runtime_threads={} body={}",
        stats.len(),
        ok,
        failed,
        cfg.concurrency,
        cfg.threads,
        cfg.body.name()
    );
    println!(
        "bytes={} ({:.2}MiB) chunks={} wall={}ms aggregate={:.2}Mbps",
        bytes,
        bytes as f64 / (1024.0 * 1024.0),
        chunks,
        wall_ms,
        aggregate_mbps
    );
    println!(
        "request_time min={}ms avg={:.1}ms max={}ms worker_threads_seen={:?}",
        min_ms, avg_ms, max_ms, threads
    );
}

fn main() -> Result<(), Box<dyn Error>> {
    let Some(cfg) = parse_args().map_err(|error| {
        eprintln!("netbench: {error}");
        error
    })? else {
        return Ok(());
    };

    let origin = Instant::now();

    if cfg.dns_preflight {
        dns_preflight(origin, &cfg.url)?;
    }

    bench_log!(
        origin,
        "[cfg] threads={} concurrency={} requests={} body={} close={} pool_idle={}/host pool_timeout={}ms tls_fragment={:?} limit={:?}",
        cfg.threads,
        cfg.concurrency,
        cfg.requests,
        cfg.body.name(),
        cfg.close,
        cfg.pool_max_idle,
        cfg.pool_idle_timeout.as_millis(),
        cfg.tls_fragment,
        cfg.limit
    );
    bench_log!(origin, "[cfg] url={}", cfg.url);

    let client = make_client(&cfg)?;
    let cfg = Arc::new(cfg);

    if cfg.threads == 1 {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_io()
            .enable_time()
            .build()?;
        runtime.block_on(run(origin, cfg, client));
    } else {
        WORKERS_STARTED.store(0, Ordering::Release);
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(cfg.threads)
            .on_thread_start(move || {
                let worker = WORKERS_STARTED.fetch_add(1, Ordering::AcqRel) + 1;
                bench_log!(
                    origin,
                    "[runtime] worker #{worker} started tid={:?}",
                    std::thread::current().id()
                );
            })
            .on_thread_stop(move || {
                bench_log!(
                    origin,
                    "[runtime] worker stopped tid={:?}",
                    std::thread::current().id()
                );
            })
            .enable_io()
            .enable_time()
            .build()?;
        runtime.block_on(run(origin, cfg, client));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn body_mode_parser() {
        assert_eq!(BodyMode::parse("bytes").unwrap(), BodyMode::Bytes);
        assert_eq!(BodyMode::parse("chunks").unwrap(), BodyMode::Chunks);
        assert!(BodyMode::parse("wat").is_err());
    }
}
