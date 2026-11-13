//! `netweevil dataset fetch`: download an OSM extract from Geofabrik (or any
//! `.osm.pbf` URL) with resume and md5 verification, then optionally import it.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use clap::Args;
use netweevil_persist::WorkspacePaths;
use serde::Deserialize;
use ureq::ResponseExt;

use super::{DatasetImportArgs, dataset_import};

const GEOFABRIK_BASE: &str = "https://download.geofabrik.de";
const GEOFABRIK_INDEX: &str = "https://download.geofabrik.de/index-v1-nogeom.json";
/// Top-level Geofabrik regions; a slash path starting with one of these maps
/// straight to a download URL.
const GEOFABRIK_ROOTS: &[&str] = &[
    "africa",
    "antarctica",
    "asia",
    "australia-oceania",
    "central-america",
    "europe",
    "north-america",
    "russia",
    "south-america",
];
const USER_AGENT: &str = concat!("netweevil/", env!("CARGO_PKG_VERSION"));
const PROGRESS_INTERVAL: Duration = Duration::from_secs(2);
const COPY_BUFFER_BYTES: usize = 1 << 20;

#[derive(Args, Debug)]
#[command(group(
    clap::ArgGroup::new("target").required(true).args(["region", "url", "list"])
))]
pub(crate) struct DatasetFetchArgs {
    /// Geofabrik region: a path such as `europe/netherlands` or
    /// `europe/germany/berlin`, or a region id or name such as `netherlands`
    /// looked up in the Geofabrik index.
    #[arg(conflicts_with_all = ["url", "list"])]
    region: Option<String>,
    /// Direct `.osm.pbf` URL to download instead of a Geofabrik region.
    #[arg(long, conflicts_with = "list")]
    url: Option<String>,
    /// Print the Geofabrik region ids (id, name, parent), optionally
    /// filtered by a substring of the id or name.
    #[arg(long, value_name = "FILTER", num_args = 0..=1, default_missing_value = "")]
    list: Option<String>,
    /// Directory for the downloaded file.
    #[arg(long, default_value = "datasets")]
    out: PathBuf,
    /// Import the downloaded file as a dataset.
    #[arg(long, conflicts_with = "list")]
    import: bool,
    /// Id of the imported dataset; derived from the region or file name when
    /// omitted.
    #[arg(long, requires = "import")]
    name: Option<String>,
    /// Download again even when the local file is current.
    #[arg(long, conflicts_with = "list")]
    force: bool,
}

pub(crate) fn dataset_fetch(paths: &WorkspacePaths, args: DatasetFetchArgs) -> Result<()> {
    let agent = http_agent();
    if let Some(filter) = args.list.as_deref() {
        let regions = load_index(&agent)?;
        for region in filter_regions(&regions, filter) {
            println!(
                "{}\t{}\t{}",
                region.id,
                region.name,
                region.parent.as_deref().unwrap_or("")
            );
        }
        return Ok(());
    }

    let target = match (&args.region, &args.url) {
        (_, Some(url)) => FetchTarget::from_url(url)?,
        (Some(region), None) => {
            let query = region.trim().trim_matches('/').to_ascii_lowercase();
            if is_region_path(&query) {
                FetchTarget::from_region_path(&query)
            } else {
                resolve_region_name(&load_index(&agent)?, &query)?
            }
        }
        (None, None) => bail!("give a region, --url or --list"),
    };

    let outcome = fetch_file(&agent, &target.url, &args.out, args.force, true)?;
    match outcome.status {
        FetchStatus::Current => eprintln!("{} is current", outcome.path.display()),
        FetchStatus::Downloaded => eprintln!("downloaded {}", outcome.path.display()),
    }
    println!("{}", outcome.path.display());

    if args.import {
        let name = args.name.unwrap_or(target.dataset_name);
        dataset_import(
            paths,
            DatasetImportArgs {
                sources: vec![outcome.path],
                name,
                format: None,
                mapping: None,
                admin_boundaries: None,
                dem: None,
                dem_crs: None,
                dem_vertical_datum: None,
            },
        )?;
    }
    Ok(())
}

fn http_agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .user_agent(USER_AGENT)
        .http_status_as_error(false)
        .timeout_connect(Some(Duration::from_secs(30)))
        .timeout_recv_response(Some(Duration::from_secs(60)))
        .build()
        .into()
}

/// What to download and the default dataset id for it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct FetchTarget {
    url: String,
    dataset_name: String,
}

impl FetchTarget {
    fn from_region_path(path: &str) -> Self {
        Self {
            url: region_path_url(path),
            dataset_name: last_segment(path).to_string(),
        }
    }

    fn from_url(url: &str) -> Result<Self> {
        let file = file_name_from_url(url)?;
        Ok(Self {
            url: url.to_string(),
            dataset_name: dataset_name_from_file(&file),
        })
    }
}

#[derive(Debug, Clone, Deserialize)]
struct RegionIndex {
    features: Vec<RegionFeature>,
}

#[derive(Debug, Clone, Deserialize)]
struct RegionFeature {
    properties: Region,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
struct Region {
    id: String,
    name: String,
    #[serde(default)]
    parent: Option<String>,
    urls: RegionUrls,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
struct RegionUrls {
    #[serde(default)]
    pbf: Option<String>,
}

fn load_index(agent: &ureq::Agent) -> Result<Vec<Region>> {
    let mut response = agent
        .get(GEOFABRIK_INDEX)
        .call()
        .with_context(|| format!("fetching {GEOFABRIK_INDEX}"))?;
    if response.status().as_u16() != 200 {
        bail!("{GEOFABRIK_INDEX} returned HTTP {}", response.status());
    }
    let text = response
        .body_mut()
        .with_config()
        .limit(64 << 20)
        .read_to_string()
        .context("reading the Geofabrik region index")?;
    parse_index(&text)
}

fn parse_index(text: &str) -> Result<Vec<Region>> {
    let index: RegionIndex =
        serde_json::from_str(text).context("parsing the Geofabrik region index")?;
    Ok(index
        .features
        .into_iter()
        .map(|feature| feature.properties)
        .collect())
}

/// A slash path under a top-level Geofabrik region, or a top-level region
/// itself, resolves without the index.
fn is_region_path(query: &str) -> bool {
    let root = query.split('/').next().unwrap_or_default();
    GEOFABRIK_ROOTS.contains(&root) && (query.contains('/') || query == root)
}

fn region_path_url(path: &str) -> String {
    format!("{GEOFABRIK_BASE}/{path}-latest.osm.pbf")
}

fn last_segment(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

fn normalized_name(name: &str) -> String {
    name.trim().to_ascii_lowercase().replace(' ', "-")
}

fn region_matches(region: &Region, query: &str) -> bool {
    region.id == query
        || region.name.to_ascii_lowercase() == query
        || normalized_name(&region.name) == query
}

fn resolve_region_name(regions: &[Region], query: &str) -> Result<FetchTarget> {
    let query = query.trim().to_ascii_lowercase();
    let matches = regions
        .iter()
        .filter(|region| region_matches(region, &query))
        .collect::<Vec<_>>();
    match matches.as_slice() {
        [region] => {
            let url = region
                .urls
                .pbf
                .clone()
                .ok_or_else(|| anyhow!("region '{}' has no .osm.pbf download", region.id))?;
            Ok(FetchTarget {
                url,
                dataset_name: last_segment(&region.id).to_string(),
            })
        }
        [] => {
            let suggestions = closest_regions(regions, &query, 5);
            if suggestions.is_empty() {
                bail!("unknown region '{query}'");
            }
            bail!(
                "unknown region '{query}'; closest matches: {}",
                describe_regions(&suggestions)
            )
        }
        several => bail!(
            "region '{query}' is ambiguous; use one of: {}",
            describe_regions(several)
        ),
    }
}

fn describe_regions(regions: &[&Region]) -> String {
    regions
        .iter()
        .map(|region| match region.parent.as_deref() {
            Some(parent) => format!("{} ({}, in {parent})", region.id, region.name),
            None => format!("{} ({})", region.id, region.name),
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// Regions ranked by substring match first, then by edit distance of the id
/// or name to the query.
fn closest_regions<'a>(regions: &'a [Region], query: &str, limit: usize) -> Vec<&'a Region> {
    let mut scored = regions
        .iter()
        .map(|region| {
            let name = normalized_name(&region.name);
            let contains = region.id.contains(query) || name.contains(query);
            let distance = edit_distance(&region.id, query)
                .min(edit_distance(last_segment(&region.id), query))
                .min(edit_distance(&name, query));
            (!contains, distance, region)
        })
        .filter(|(not_contains, distance, _)| {
            !*not_contains || *distance <= query.chars().count().div_ceil(3).max(2)
        })
        .collect::<Vec<_>>();
    scored.sort_by(|a, b| (a.0, a.1, &a.2.id).cmp(&(b.0, b.1, &b.2.id)));
    scored
        .into_iter()
        .take(limit)
        .map(|(_, _, region)| region)
        .collect()
}

fn edit_distance(a: &str, b: &str) -> usize {
    let b = b.chars().collect::<Vec<_>>();
    let mut row = (0..=b.len()).collect::<Vec<_>>();
    for (i, ca) in a.chars().enumerate() {
        let mut diagonal = row[0];
        row[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let above = row[j + 1];
            row[j + 1] = (above + 1)
                .min(row[j] + 1)
                .min(diagonal + usize::from(ca != *cb));
            diagonal = above;
        }
    }
    row[b.len()]
}

fn filter_regions<'a>(regions: &'a [Region], filter: &str) -> Vec<&'a Region> {
    let filter = filter.trim().to_ascii_lowercase();
    let mut selected = regions
        .iter()
        .filter(|region| {
            filter.is_empty()
                || region.id.contains(&filter)
                || region.name.to_ascii_lowercase().contains(&filter)
        })
        .collect::<Vec<_>>();
    selected.sort_by(|a, b| a.id.cmp(&b.id));
    selected
}

fn file_name_from_url(url: &str) -> Result<String> {
    let path = url
        .split(['?', '#'])
        .next()
        .unwrap_or_default()
        .trim_end_matches('/');
    let (scheme, rest) = path
        .split_once("://")
        .ok_or_else(|| anyhow!("'{url}' is not an http(s) URL"))?;
    if scheme != "http" && scheme != "https" {
        bail!("'{url}' is not an http(s) URL");
    }
    match rest.split_once('/') {
        Some((_, file_path)) if !file_path.is_empty() => {
            let file = last_segment(file_path);
            if file.is_empty() || file == "." || file == ".." {
                bail!("'{url}' has no file name");
            }
            Ok(file.to_string())
        }
        _ => bail!("'{url}' has no file name"),
    }
}

fn dataset_name_from_file(file: &str) -> String {
    let stem = file
        .strip_suffix(".osm.pbf")
        .or_else(|| file.strip_suffix(".pbf"))
        .unwrap_or(file);
    stem.strip_suffix("-latest").unwrap_or(stem).to_string()
}

/// The md5 hex digest from a `.md5` sidecar (`<hex>  <file>`).
fn parse_md5_sidecar(text: &str) -> Option<String> {
    let token = text.split_whitespace().next()?.to_ascii_lowercase();
    (token.len() == 32 && token.bytes().all(|byte| byte.is_ascii_hexdigit())).then_some(token)
}

/// Parses `Content-Range: bytes <start>-<end>/<total|*>`.
fn parse_content_range(value: &str) -> Option<(u64, u64, Option<u64>)> {
    let range = value.trim().strip_prefix("bytes")?.trim_start();
    let (span, total) = range.split_once('/')?;
    let (start, end) = span.split_once('-')?;
    let total = match total.trim() {
        "*" => None,
        total => Some(total.parse().ok()?),
    };
    Some((start.trim().parse().ok()?, end.trim().parse().ok()?, total))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ResumePlan {
    /// Start from an empty `.part` file.
    Restart,
    /// Request the remaining bytes from this offset.
    Resume(u64),
    /// The `.part` file already holds the full length.
    Complete,
}

/// Decides how to continue from a `.part` file of `part_len` bytes that was
/// written from `part_source` (the resolved URL and length recorded next to
/// it).
fn resume_plan(
    part_len: u64,
    part_source: Option<&str>,
    source: &str,
    remote_len: Option<u64>,
) -> ResumePlan {
    if part_len == 0 || part_source != Some(source) {
        return ResumePlan::Restart;
    }
    match remote_len {
        Some(total) if part_len == total => ResumePlan::Complete,
        Some(total) if part_len > total => ResumePlan::Restart,
        _ => ResumePlan::Resume(part_len),
    }
}

fn source_identity(url: &str, remote_len: Option<u64>) -> String {
    match remote_len {
        Some(len) => format!("{url} {len}"),
        None => url.to_string(),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FetchStatus {
    Downloaded,
    Current,
}

#[derive(Debug)]
struct FetchOutcome {
    path: PathBuf,
    status: FetchStatus,
}

/// Remote facts about a download: the URL after redirects, its length and
/// its md5 when the server publishes a `.md5` sidecar.
#[derive(Debug)]
struct RemoteFile {
    url: String,
    len: Option<u64>,
    md5: Option<String>,
}

fn probe_remote(agent: &ureq::Agent, url: &str) -> Result<RemoteFile> {
    let response = agent
        .head(url)
        .call()
        .with_context(|| format!("requesting {url}"))?;
    let status = response.status().as_u16();
    if !(200..300).contains(&status) {
        bail!("{url} returned HTTP {status}");
    }
    let resolved = response.get_uri().to_string();
    let len = header_u64(&response, "content-length");
    let mut md5 = fetch_md5(agent, &format!("{resolved}.md5"))?;
    if md5.is_none() && resolved != url {
        md5 = fetch_md5(agent, &format!("{url}.md5"))?;
    }
    Ok(RemoteFile {
        url: resolved,
        len,
        md5,
    })
}

fn fetch_md5(agent: &ureq::Agent, url: &str) -> Result<Option<String>> {
    let mut response = agent
        .get(url)
        .call()
        .with_context(|| format!("requesting {url}"))?;
    if response.status().as_u16() != 200 {
        return Ok(None);
    }
    let text = response
        .body_mut()
        .with_config()
        .limit(4096)
        .read_to_string()
        .with_context(|| format!("reading {url}"))?;
    Ok(parse_md5_sidecar(&text))
}

fn header_u64<B>(response: &ureq::http::Response<B>, name: &str) -> Option<u64> {
    response
        .headers()
        .get(name)?
        .to_str()
        .ok()?
        .trim()
        .parse()
        .ok()
}

fn header_str<'a, B>(response: &'a ureq::http::Response<B>, name: &str) -> Option<&'a str> {
    response.headers().get(name)?.to_str().ok()
}

fn file_md5(path: &Path) -> Result<String> {
    let mut file = File::open(path).with_context(|| format!("opening {}", path.display()))?;
    let mut context = md5::Context::new();
    hash_reader(&mut file, &mut context).with_context(|| format!("reading {}", path.display()))?;
    Ok(format!("{:x}", context.finalize()))
}

fn hash_reader(reader: &mut impl Read, context: &mut md5::Context) -> io::Result<u64> {
    let mut buffer = vec![0_u8; COPY_BUFFER_BYTES];
    let mut total = 0_u64;
    loop {
        let read = reader.read(&mut buffer)?;
        if read == 0 {
            return Ok(total);
        }
        context.consume(&buffer[..read]);
        total += read as u64;
    }
}

fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_os_string();
    name.push(suffix);
    PathBuf::from(name)
}

fn fetch_file(
    agent: &ureq::Agent,
    url: &str,
    out_dir: &Path,
    force: bool,
    show_progress: bool,
) -> Result<FetchOutcome> {
    let file_name = file_name_from_url(url)?;
    let path = out_dir.join(&file_name);
    let part_path = with_suffix(&path, ".part");
    let source_path = with_suffix(&path, ".part.source");
    let remote = probe_remote(agent, url)?;

    if !force && path.is_file() && local_is_current(&path, &remote)? {
        return Ok(FetchOutcome {
            path,
            status: FetchStatus::Current,
        });
    }

    fs::create_dir_all(out_dir).with_context(|| format!("creating {}", out_dir.display()))?;
    let source = source_identity(&remote.url, remote.len);
    let part_len = if force {
        0
    } else {
        fs::metadata(&part_path).map(|meta| meta.len()).unwrap_or(0)
    };
    let part_source = fs::read_to_string(&source_path).ok();
    let plan = resume_plan(
        part_len,
        part_source.as_deref().map(str::trim),
        &source,
        remote.len,
    );
    fs::write(&source_path, &source)
        .with_context(|| format!("writing {}", source_path.display()))?;

    let mut context = md5::Context::new();
    let offset = match plan {
        ResumePlan::Restart => {
            File::create(&part_path)
                .with_context(|| format!("creating {}", part_path.display()))?;
            0
        }
        ResumePlan::Resume(_) | ResumePlan::Complete => {
            let mut part = File::open(&part_path)
                .with_context(|| format!("opening {}", part_path.display()))?;
            hash_reader(&mut part, &mut context)
                .with_context(|| format!("reading {}", part_path.display()))?
        }
    };

    let written = if plan == ResumePlan::Complete {
        offset
    } else {
        download_into(
            agent,
            &remote,
            &part_path,
            offset,
            &mut context,
            show_progress,
        )?
    };

    if let Some(total) = remote.len
        && written != total
    {
        bail!(
            "{} has {written} bytes, expected {total}; run the command again to resume",
            part_path.display()
        );
    }
    let digest = format!("{:x}", context.finalize());
    if let Some(expected) = remote.md5.as_deref()
        && digest != expected
    {
        let _ = fs::remove_file(&part_path);
        let _ = fs::remove_file(&source_path);
        bail!(
            "md5 mismatch for {}: expected {expected}, got {digest}",
            remote.url
        );
    }
    fs::rename(&part_path, &path)
        .with_context(|| format!("renaming {} to {}", part_path.display(), path.display()))?;
    let _ = fs::remove_file(&source_path);
    Ok(FetchOutcome {
        path,
        status: FetchStatus::Downloaded,
    })
}

fn local_is_current(path: &Path, remote: &RemoteFile) -> Result<bool> {
    let local_len = fs::metadata(path)
        .with_context(|| format!("reading {}", path.display()))?
        .len();
    if remote.len.is_some_and(|len| len != local_len) {
        return Ok(false);
    }
    match remote.md5.as_deref() {
        Some(expected) => Ok(file_md5(path)? == expected),
        None => Ok(remote.len.is_some()),
    }
}

/// Streams the remote body into `part_path` from `offset`, restarting from
/// zero when the server ignores the range. Returns the final file length.
fn download_into(
    agent: &ureq::Agent,
    remote: &RemoteFile,
    part_path: &Path,
    offset: u64,
    context: &mut md5::Context,
    show_progress: bool,
) -> Result<u64> {
    let mut request = agent.get(&remote.url);
    if offset > 0 {
        request = request.header("Range", format!("bytes={offset}-"));
    }
    let response = request
        .call()
        .with_context(|| format!("requesting {}", remote.url))?;
    let status = response.status().as_u16();
    let (start, mut file) = match status {
        206 => {
            let (start, _, _) = header_str(&response, "content-range")
                .and_then(parse_content_range)
                .ok_or_else(|| anyhow!("{} sent 206 without a Content-Range", remote.url))?;
            if start != offset {
                bail!("{} resumed at byte {start}, requested {offset}", remote.url);
            }
            let file = OpenOptions::new()
                .append(true)
                .open(part_path)
                .with_context(|| format!("opening {}", part_path.display()))?;
            (offset, file)
        }
        200 => {
            if offset > 0 {
                *context = md5::Context::new();
            }
            let file = File::create(part_path)
                .with_context(|| format!("creating {}", part_path.display()))?;
            (0, file)
        }
        416 => {
            let _ = fs::remove_file(part_path);
            bail!(
                "{} rejected the resume range; run the command again to restart",
                remote.url
            );
        }
        _ => bail!("{} returned HTTP {status}", remote.url),
    };

    let mut progress = Progress::new(start, remote.len, show_progress);
    let mut reader = response.into_body().into_reader();
    let mut buffer = vec![0_u8; COPY_BUFFER_BYTES];
    let mut written = start;
    loop {
        let read = reader
            .read(&mut buffer)
            .with_context(|| format!("reading {}", remote.url))?;
        if read == 0 {
            break;
        }
        file.write_all(&buffer[..read])
            .with_context(|| format!("writing {}", part_path.display()))?;
        context.consume(&buffer[..read]);
        written += read as u64;
        progress.update(written);
    }
    file.sync_all()
        .with_context(|| format!("writing {}", part_path.display()))?;
    progress.finish(written);
    Ok(written)
}

struct Progress {
    enabled: bool,
    start_bytes: u64,
    total: Option<u64>,
    started: Instant,
    last: Instant,
}

impl Progress {
    fn new(start_bytes: u64, total: Option<u64>, enabled: bool) -> Self {
        let now = Instant::now();
        Self {
            enabled,
            start_bytes,
            total,
            started: now,
            last: now,
        }
    }

    fn update(&mut self, bytes: u64) {
        if self.enabled && self.last.elapsed() >= PROGRESS_INTERVAL {
            self.last = Instant::now();
            eprint!("\r{}", self.line(bytes));
        }
    }

    fn finish(&self, bytes: u64) {
        if self.enabled {
            eprintln!("\r{}", self.line(bytes));
        }
    }

    fn line(&self, bytes: u64) -> String {
        let seconds = self.started.elapsed().as_secs_f64();
        let rate = (bytes - self.start_bytes) as f64 / seconds.max(1e-3);
        format_progress(bytes, self.total, rate)
    }
}

fn format_progress(bytes: u64, total: Option<u64>, rate_bytes_per_s: f64) -> String {
    match total {
        Some(total) if total > 0 => format!(
            "{} of {} ({:.1}%) at {}/s   ",
            format_bytes(bytes as f64),
            format_bytes(total as f64),
            bytes as f64 * 100.0 / total as f64,
            format_bytes(rate_bytes_per_s)
        ),
        _ => format!(
            "{} at {}/s   ",
            format_bytes(bytes as f64),
            format_bytes(rate_bytes_per_s)
        ),
    }
}

fn format_bytes(bytes: f64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut value = bytes;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{value:.0} {}", UNITS[unit])
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

#[cfg(test)]
mod tests;
