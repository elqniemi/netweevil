use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::thread;

use super::*;

const SAMPLE_INDEX: &str = r#"{ "type": "FeatureCollection", "features": [
  { "type": "Feature", "properties": { "id": "europe", "name": "Europe",
    "urls": { "pbf": "https://download.geofabrik.de/europe-latest.osm.pbf" } } },
  { "type": "Feature", "properties": { "id": "netherlands", "parent": "europe",
    "iso3166-1:alpha2": [ "NL" ], "name": "Netherlands",
    "urls": { "pbf": "https://download.geofabrik.de/europe/netherlands-latest.osm.pbf",
              "shp": "https://download.geofabrik.de/europe/netherlands-latest-free.shp.zip" } } },
  { "type": "Feature", "properties": { "id": "finland", "parent": "europe", "name": "Finland",
    "urls": { "pbf": "https://download.geofabrik.de/europe/finland-latest.osm.pbf" } } },
  { "type": "Feature", "properties": { "id": "germany", "parent": "europe", "name": "Germany",
    "urls": { "pbf": "https://download.geofabrik.de/europe/germany-latest.osm.pbf" } } },
  { "type": "Feature", "properties": { "id": "berlin", "parent": "germany", "name": "Berlin",
    "urls": { "pbf": "https://download.geofabrik.de/europe/germany/berlin-latest.osm.pbf" } } },
  { "type": "Feature", "properties": { "id": "georgia", "parent": "europe", "name": "Georgia",
    "urls": { "pbf": "https://download.geofabrik.de/europe/georgia-latest.osm.pbf" } } },
  { "type": "Feature", "properties": { "id": "us/georgia", "parent": "north-america", "name": "Georgia",
    "urls": { "pbf": "https://download.geofabrik.de/north-america/us/georgia-latest.osm.pbf" } } },
  { "type": "Feature", "properties": { "id": "great-britain", "parent": "europe", "name": "Great Britain",
    "urls": { "pbf": "https://download.geofabrik.de/europe/great-britain-latest.osm.pbf" } } }
] }"#;

fn sample_regions() -> Vec<Region> {
    parse_index(SAMPLE_INDEX).expect("sample index parses")
}

#[test]
fn region_paths_resolve_without_the_index() {
    assert!(is_region_path("europe"));
    assert!(is_region_path("europe/netherlands"));
    assert!(is_region_path("europe/germany/berlin"));
    assert!(!is_region_path("netherlands"));
    assert!(!is_region_path("us/georgia"));
    assert_eq!(
        FetchTarget::from_region_path("europe/germany/berlin"),
        FetchTarget {
            url: "https://download.geofabrik.de/europe/germany/berlin-latest.osm.pbf".into(),
            dataset_name: "berlin".into(),
        }
    );
    assert_eq!(
        FetchTarget::from_region_path("europe").url,
        "https://download.geofabrik.de/europe-latest.osm.pbf"
    );
}

#[test]
fn region_names_resolve_through_the_index() {
    let regions = sample_regions();
    let target = resolve_region_name(&regions, "Netherlands").expect("netherlands");
    assert_eq!(
        target.url,
        "https://download.geofabrik.de/europe/netherlands-latest.osm.pbf"
    );
    assert_eq!(target.dataset_name, "netherlands");
    let target = resolve_region_name(&regions, "great britain").expect("by name");
    assert_eq!(target.dataset_name, "great-britain");
    let target = resolve_region_name(&regions, "us/georgia").expect("by id");
    assert_eq!(target.dataset_name, "georgia");
    assert!(target.url.contains("north-america/us/georgia"));
}

#[test]
fn ambiguous_and_unknown_regions_list_candidates() {
    let regions = sample_regions();
    let error = resolve_region_name(&regions, "georgia")
        .expect_err("ambiguous")
        .to_string();
    assert!(error.contains("ambiguous"), "{error}");
    assert!(error.contains("us/georgia"), "{error}");
    assert!(error.contains("georgia (Georgia, in europe)"), "{error}");

    let error = resolve_region_name(&regions, "netherland")
        .expect_err("unknown")
        .to_string();
    assert!(error.contains("closest matches: netherlands"), "{error}");
    let error = resolve_region_name(&regions, "finlnd")
        .expect_err("unknown")
        .to_string();
    assert!(error.contains("finland"), "{error}");
    let error = resolve_region_name(&regions, "zzzzzzzzzz")
        .expect_err("unknown")
        .to_string();
    assert_eq!(error, "unknown region 'zzzzzzzzzz'");
}

#[test]
fn list_filter_matches_id_or_name() {
    let regions = sample_regions();
    let ids = |filter: &str| {
        filter_regions(&regions, filter)
            .into_iter()
            .map(|region| region.id.as_str())
            .collect::<Vec<_>>()
    };
    assert_eq!(ids("").len(), regions.len());
    assert_eq!(ids("GEOR"), ["georgia", "us/georgia"]);
    assert_eq!(ids("britain"), ["great-britain"]);
}

#[test]
fn file_and_dataset_names_derive_from_urls() {
    assert_eq!(
        file_name_from_url("https://example.org/x/monaco-latest.osm.pbf?a=1").unwrap(),
        "monaco-latest.osm.pbf"
    );
    assert!(file_name_from_url("ftp://example.org/a.osm.pbf").is_err());
    assert!(file_name_from_url("https://example.org/").is_err());
    assert_eq!(dataset_name_from_file("monaco-latest.osm.pbf"), "monaco");
    assert_eq!(
        dataset_name_from_file("planet-251110.osm.pbf"),
        "planet-251110"
    );
    assert_eq!(
        FetchTarget::from_url("https://example.org/a/b/city.osm.pbf")
            .unwrap()
            .dataset_name,
        "city"
    );
}

#[test]
fn md5_sidecar_and_content_range_parse() {
    assert_eq!(
        parse_md5_sidecar("D41D8CD98F00B204E9800998ECF8427E  netherlands-latest.osm.pbf\n"),
        Some("d41d8cd98f00b204e9800998ecf8427e".into())
    );
    assert_eq!(parse_md5_sidecar("not-a-digest"), None);
    assert_eq!(parse_md5_sidecar(""), None);
    assert_eq!(
        parse_content_range("bytes 100-199/200"),
        Some((100, 199, Some(200)))
    );
    assert_eq!(parse_content_range("bytes 5-9/*"), Some((5, 9, None)));
    assert_eq!(parse_content_range("items 1-2/3"), None);
}

#[test]
fn resume_plan_follows_part_state() {
    let source = "http://h/f 100";
    assert_eq!(
        resume_plan(0, Some(source), source, Some(100)),
        ResumePlan::Restart
    );
    assert_eq!(
        resume_plan(40, Some(source), source, Some(100)),
        ResumePlan::Resume(40)
    );
    assert_eq!(
        resume_plan(100, Some(source), source, Some(100)),
        ResumePlan::Complete
    );
    assert_eq!(
        resume_plan(140, Some(source), source, Some(100)),
        ResumePlan::Restart
    );
    assert_eq!(
        resume_plan(40, Some("http://h/other 100"), source, Some(100)),
        ResumePlan::Restart
    );
    assert_eq!(
        resume_plan(40, None, source, Some(100)),
        ResumePlan::Restart
    );
    assert_eq!(
        resume_plan(40, Some("http://h/f"), "http://h/f", None),
        ResumePlan::Resume(40)
    );
}

#[test]
fn progress_lines_show_bytes_percent_and_rate() {
    assert_eq!(
        format_progress(512 << 20, Some(1 << 30), 2.5 * 1024.0 * 1024.0),
        "512.0 MiB of 1.0 GiB (50.0%) at 2.5 MiB/s   "
    );
    assert_eq!(format_progress(100, None, 10.0), "100 B at 10 B/s   ");
}

/// A minimal HTTP/1.1 server for one file, its `.md5` sidecar and an optional
/// redirect, recording each request line and Range header.
struct Stub {
    base: String,
    requests: Arc<Mutex<Vec<String>>>,
}

#[derive(Clone)]
struct StubConfig {
    body: Vec<u8>,
    md5: Option<String>,
    ranges: bool,
}

impl Stub {
    fn start(config: StubConfig) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind stub");
        let base = format!("http://{}", listener.local_addr().unwrap());
        let requests = Arc::new(Mutex::new(Vec::new()));
        let log = Arc::clone(&requests);
        thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else { break };
                let config = config.clone();
                let log = Arc::clone(&log);
                thread::spawn(move || serve(stream, &config, &log));
            }
        });
        Self { base, requests }
    }

    fn url(&self, path: &str) -> String {
        format!("{}{path}", self.base)
    }

    fn requests(&self) -> Vec<String> {
        self.requests.lock().unwrap().clone()
    }
}

fn serve(stream: TcpStream, config: &StubConfig, log: &Mutex<Vec<String>>) {
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    let mut stream = stream;
    loop {
        let mut request_line = String::new();
        if reader.read_line(&mut request_line).unwrap_or(0) == 0 {
            return;
        }
        let mut range = None;
        loop {
            let mut line = String::new();
            if reader.read_line(&mut line).unwrap_or(0) == 0 {
                return;
            }
            let line = line.trim_end();
            if line.is_empty() {
                break;
            }
            if let Some((name, value)) = line.split_once(':')
                && name.eq_ignore_ascii_case("range")
            {
                range = Some(value.trim().to_string());
            }
        }
        let mut parts = request_line.split_whitespace();
        let method = parts.next().unwrap_or_default().to_string();
        let path = parts.next().unwrap_or_default().to_string();
        log.lock().unwrap().push(match &range {
            Some(range) => format!("{method} {path} {range}"),
            None => format!("{method} {path}"),
        });

        let (status, headers, body): (&str, Vec<String>, Vec<u8>) = match path.as_str() {
            "/latest.osm.pbf" => (
                "307 Temporary Redirect",
                vec!["Location: /dated.osm.pbf".into()],
                Vec::new(),
            ),
            "/dated.osm.pbf.md5" => match &config.md5 {
                Some(md5) => (
                    "200 OK",
                    Vec::new(),
                    format!("{md5}  dated.osm.pbf\n").into_bytes(),
                ),
                None => ("404 Not Found", Vec::new(), Vec::new()),
            },
            "/dated.osm.pbf" => {
                let total = config.body.len();
                let start = range
                    .as_deref()
                    .filter(|_| config.ranges)
                    .and_then(|range| range.strip_prefix("bytes="))
                    .and_then(|range| range.strip_suffix('-'))
                    .and_then(|start| start.parse::<usize>().ok());
                match start {
                    Some(start) if start < total => (
                        "206 Partial Content",
                        vec![
                            format!("Content-Range: bytes {start}-{}/{total}", total - 1),
                            "Accept-Ranges: bytes".into(),
                        ],
                        config.body[start..].to_vec(),
                    ),
                    Some(_) => ("416 Range Not Satisfiable", Vec::new(), Vec::new()),
                    None => ("200 OK", Vec::new(), config.body.clone()),
                }
            }
            _ => ("404 Not Found", Vec::new(), Vec::new()),
        };
        let mut head = format!("HTTP/1.1 {status}\r\nContent-Length: {}\r\n", body.len());
        for header in headers {
            head.push_str(&header);
            head.push_str("\r\n");
        }
        head.push_str("\r\n");
        if stream.write_all(head.as_bytes()).is_err() {
            return;
        }
        if method != "HEAD" && stream.write_all(&body).is_err() {
            return;
        }
        let _ = stream.flush();
    }
}

fn test_body() -> Vec<u8> {
    (0..300_000_u32).map(|i| (i * 31 % 251) as u8).collect()
}

fn body_md5(body: &[u8]) -> String {
    format!("{:x}", md5::compute(body))
}

fn temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "netweevil-fetch-{name}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn test_agent() -> ureq::Agent {
    http_agent()
}

#[test]
fn downloads_follow_redirects_verify_md5_and_skip_current_files() {
    let body = test_body();
    let stub = Stub::start(StubConfig {
        md5: Some(body_md5(&body)),
        body: body.clone(),
        ranges: true,
    });
    let dir = temp_dir("fresh");
    let agent = test_agent();
    let url = stub.url("/latest.osm.pbf");

    let outcome = fetch_file(&agent, &url, &dir, false, false).expect("download");
    assert_eq!(outcome.status, FetchStatus::Downloaded);
    assert_eq!(outcome.path, dir.join("latest.osm.pbf"));
    assert_eq!(fs::read(&outcome.path).unwrap(), body);
    assert!(!dir.join("latest.osm.pbf.part").exists());
    assert!(!dir.join("latest.osm.pbf.part.source").exists());

    let outcome = fetch_file(&agent, &url, &dir, false, false).expect("current");
    assert_eq!(outcome.status, FetchStatus::Current);
    let gets = stub
        .requests()
        .iter()
        .filter(|line| line.starts_with("GET /dated.osm.pbf") && !line.contains(".md5"))
        .count();
    assert_eq!(gets, 1, "{:?}", stub.requests());

    let outcome = fetch_file(&agent, &url, &dir, true, false).expect("forced");
    assert_eq!(outcome.status, FetchStatus::Downloaded);
    assert_eq!(fs::read(&outcome.path).unwrap(), body);
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn partial_downloads_resume_with_a_range_request() {
    let body = test_body();
    let stub = Stub::start(StubConfig {
        md5: Some(body_md5(&body)),
        body: body.clone(),
        ranges: true,
    });
    let dir = temp_dir("resume");
    let source = source_identity(&stub.url("/dated.osm.pbf"), Some(body.len() as u64));
    fs::write(dir.join("latest.osm.pbf.part"), &body[..123_456]).unwrap();
    fs::write(dir.join("latest.osm.pbf.part.source"), &source).unwrap();

    let outcome = fetch_file(
        &test_agent(),
        &stub.url("/latest.osm.pbf"),
        &dir,
        false,
        false,
    )
    .expect("resume");
    assert_eq!(outcome.status, FetchStatus::Downloaded);
    assert_eq!(fs::read(&outcome.path).unwrap(), body);
    assert!(
        stub.requests()
            .contains(&"GET /dated.osm.pbf bytes=123456-".to_string()),
        "{:?}",
        stub.requests()
    );
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn resume_restarts_when_the_server_ignores_ranges_or_the_source_changed() {
    let body = test_body();
    let stub = Stub::start(StubConfig {
        md5: Some(body_md5(&body)),
        body: body.clone(),
        ranges: false,
    });
    let dir = temp_dir("norange");
    let source = source_identity(&stub.url("/dated.osm.pbf"), Some(body.len() as u64));
    fs::write(dir.join("latest.osm.pbf.part"), &body[..1000]).unwrap();
    fs::write(dir.join("latest.osm.pbf.part.source"), &source).unwrap();
    let outcome = fetch_file(
        &test_agent(),
        &stub.url("/latest.osm.pbf"),
        &dir,
        false,
        false,
    )
    .expect("full");
    assert_eq!(fs::read(&outcome.path).unwrap(), body);

    fs::remove_file(&outcome.path).unwrap();
    fs::write(dir.join("latest.osm.pbf.part"), b"stale bytes").unwrap();
    fs::write(
        dir.join("latest.osm.pbf.part.source"),
        "http://elsewhere/x 5",
    )
    .unwrap();
    let outcome = fetch_file(
        &test_agent(),
        &stub.url("/latest.osm.pbf"),
        &dir,
        false,
        false,
    )
    .expect("fresh");
    assert_eq!(fs::read(&outcome.path).unwrap(), body);
    let requests = stub.requests();
    assert_eq!(
        requests.last().map(String::as_str),
        Some("GET /dated.osm.pbf"),
        "{requests:?}"
    );
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn md5_mismatch_fails_without_renaming() {
    let body = test_body();
    let stub = Stub::start(StubConfig {
        md5: Some("0123456789abcdef0123456789abcdef".into()),
        body,
        ranges: true,
    });
    let dir = temp_dir("mismatch");
    let error = fetch_file(
        &test_agent(),
        &stub.url("/latest.osm.pbf"),
        &dir,
        false,
        false,
    )
    .expect_err("md5 mismatch")
    .to_string();
    assert!(error.contains("md5 mismatch"), "{error}");
    assert!(!dir.join("latest.osm.pbf").exists());
    assert!(!dir.join("latest.osm.pbf.part").exists());
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn downloads_without_an_md5_sidecar_check_the_length() {
    let body = test_body();
    let stub = Stub::start(StubConfig {
        md5: None,
        body: body.clone(),
        ranges: true,
    });
    let dir = temp_dir("nomd5");
    let agent = test_agent();
    let url = stub.url("/dated.osm.pbf");
    let outcome = fetch_file(&agent, &url, &dir, false, false).expect("download");
    assert_eq!(outcome.path, dir.join("dated.osm.pbf"));
    assert_eq!(fs::read(&outcome.path).unwrap(), body);
    let outcome = fetch_file(&agent, &url, &dir, false, false).expect("current");
    assert_eq!(outcome.status, FetchStatus::Current);
    assert!(fetch_file(&agent, &stub.url("/missing.osm.pbf"), &dir, false, false).is_err());
    fs::remove_dir_all(&dir).unwrap();
}
