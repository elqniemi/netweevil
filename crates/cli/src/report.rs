use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use clap::Subcommand;
use netweevil_persist::read_run_manifest;
use netweevil_report::{load_run_result_summary, render_run_html, render_run_markdown};

#[derive(Subcommand, Debug)]
pub(crate) enum ReportCommand {
    Render {
        manifest: PathBuf,
        #[arg(long)]
        out: Option<PathBuf>,
    },
}

pub(crate) fn report_render(manifest_path: &Path, out: Option<&Path>) -> Result<()> {
    let manifest = read_run_manifest(manifest_path)?;
    let result_summary = load_run_result_summary(&manifest)?;
    let markdown = render_run_markdown(&manifest, result_summary.as_ref());
    let html = render_run_html(&manifest, result_summary.as_ref());
    match out {
        None => println!("{markdown}"),
        Some(out) if markdown_output_path(out) => {
            fs::write(out, markdown).with_context(|| format!("writing {}", out.display()))?;
            println!("report written to {}", out.display());
        }
        Some(out) if html_output_path(out) => {
            fs::write(out, html).with_context(|| format!("writing {}", out.display()))?;
            println!("report written to {}", out.display());
        }
        Some(out) => {
            write_report_bundle(
                out,
                manifest_path,
                manifest.result_path.as_deref(),
                &markdown,
                &html,
            )?;
            println!("report bundle written to {}", out.display());
        }
    }
    Ok(())
}

fn markdown_output_path(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("md") || ext.eq_ignore_ascii_case("markdown"))
}

fn html_output_path(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("html") || ext.eq_ignore_ascii_case("htm"))
}

fn write_report_bundle(
    out_dir: &Path,
    manifest_path: &Path,
    result_path: Option<&str>,
    markdown: &str,
    html: &str,
) -> Result<()> {
    fs::create_dir_all(out_dir).with_context(|| format!("creating {}", out_dir.display()))?;
    let markdown_path = out_dir.join("index.md");
    let html_path = out_dir.join("index.html");
    let manifest_copy_path = out_dir.join("run-manifest.json");
    fs::write(&markdown_path, markdown)
        .with_context(|| format!("writing {}", markdown_path.display()))?;
    fs::write(&html_path, html).with_context(|| format!("writing {}", html_path.display()))?;
    fs::copy(manifest_path, &manifest_copy_path).with_context(|| {
        format!(
            "copying run manifest {} to {}",
            manifest_path.display(),
            manifest_copy_path.display()
        )
    })?;
    if let Some(result_path) = result_path {
        let source = Path::new(result_path);
        if source.exists() {
            let result_copy_path = out_dir.join(
                source
                    .file_name()
                    .unwrap_or_else(|| std::ffi::OsStr::new("result.json")),
            );
            fs::copy(source, &result_copy_path).with_context(|| {
                format!(
                    "copying result {} to {}",
                    source.display(),
                    result_copy_path.display()
                )
            })?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{html_output_path, markdown_output_path, write_report_bundle};
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn detects_report_output_formats() {
        assert!(markdown_output_path(Path::new("report.md")));
        assert!(html_output_path(Path::new("report.html")));
        assert!(!markdown_output_path(Path::new("report.bundle")));
    }

    #[test]
    fn writes_report_bundle_with_manifest_and_result() {
        let temp_dir = temp_dir("bundle");
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

        assert!(out_dir.join("index.md").exists());
        assert!(out_dir.join("index.html").exists());
        assert!(out_dir.join("run-manifest.json").exists());
        assert!(out_dir.join("result.json").exists());
        fs::remove_dir_all(temp_dir).ok();
    }

    fn temp_dir(label: &str) -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time works")
            .as_nanos();
        let path = std::env::temp_dir().join(format!("netweevil-cli-{label}-{unique}"));
        fs::create_dir_all(&path).expect("temp dir created");
        path
    }
}
