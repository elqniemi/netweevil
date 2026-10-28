use super::write_report_bundle;
use std::fs;

#[test]
fn writes_report_bundle_with_manifest_and_result() {
    let temp_dir = std::env::temp_dir().join(format!(
        "netweevil-cli-report-bundle-{}",
        std::process::id()
    ));
    fs::create_dir_all(&temp_dir).expect("temp dir created");
    let out_dir = temp_dir.join("report-bundle");
    let manifest_path = temp_dir.join("run.json");
    let result_path = temp_dir.join("result.json");
    fs::write(&manifest_path, "{\"run_id\":\"run-1\"}").expect("manifest written");
    fs::write(&result_path, "{\"route_id\":\"route-1\"}").expect("result written");

    write_report_bundle(
        &out_dir,
        &manifest_path,
        Some(result_path.to_str().expect("utf-8 path")),
        "# Report\n",
        "<html></html>",
    )
    .expect("bundle write succeeds");

    for file in ["index.md", "index.html", "run-manifest.json", "result.json"] {
        assert!(out_dir.join(file).exists(), "{file}");
    }
    fs::remove_dir_all(temp_dir).ok();
}
