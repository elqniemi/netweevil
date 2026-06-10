use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use clap::Subcommand;
use netweevil_core::{CacheBundleId, DatasetAccelerationBundle, TopologyBundle};
use netweevil_persist::{
    WorkspacePaths, read_acceleration_bundle, read_dataset_manifest, read_topology_bundle,
    write_compiled_profile_bundle, write_compiled_profile_manifest,
};
use netweevil_profile::{
    ProfileCompileProgress, ProfileCompileStage,
    compile_profile_bundle_with_acceleration_with_progress, load_profile,
};
use netweevil_report::{BundleRef, CompiledProfileManifest};

#[derive(Subcommand, Debug)]
pub(crate) enum ProfileCommand {
    Validate {
        profile: PathBuf,
    },
    Compile {
        #[arg(long)]
        dataset: String,
        #[arg(long)]
        profile: PathBuf,
    },
}

pub(crate) fn profile_validate(path: &Path) -> Result<()> {
    let profile = load_profile(path)?;
    profile.validate()?;
    println!(
        "profile '{}' is valid (hash {})",
        profile.profile.id,
        profile.fingerprint()?
    );
    Ok(())
}

pub(crate) fn profile_compile(
    paths: &WorkspacePaths,
    dataset: &str,
    profile_path: &Path,
) -> Result<()> {
    let mut progress_line_len = 0_usize;
    let profile = load_profile(profile_path)?;
    profile.validate()?;
    render_profile_compile_message(
        "Load Dataset",
        format!("Reading dataset manifest for '{dataset}'"),
        false,
        &mut progress_line_len,
    );
    let dataset_manifest = read_dataset_manifest(paths, dataset)
        .with_context(|| format!("reading dataset manifest for '{dataset}'"))?;
    let topology_ref = dataset_manifest
        .topology_bundle
        .clone()
        .context("dataset is missing a topology bundle; run `netweevil dataset import` first")?;
    render_profile_compile_message(
        "Load Dataset",
        format!("Reading topology bundle {}", topology_ref.path),
        false,
        &mut progress_line_len,
    );
    let topology: TopologyBundle = read_topology_bundle(&topology_ref.path)
        .with_context(|| format!("reading topology bundle {}", topology_ref.path))?;
    render_profile_compile_message(
        "Load Dataset",
        "Reading acceleration bundle metadata".to_string(),
        false,
        &mut progress_line_len,
    );
    let acceleration: Option<(DatasetAccelerationBundle, CacheBundleId)> = dataset_manifest
        .acceleration_bundle
        .as_ref()
        .map(|bundle_ref| {
            read_acceleration_bundle(&bundle_ref.path)
                .with_context(|| format!("reading acceleration bundle {}", bundle_ref.path))
                .map(|bundle| (bundle, bundle_ref.bundle_id.clone()))
        })
        .transpose()?;
    let compiled_bundle = compile_profile_bundle_with_acceleration_with_progress(
        &profile,
        &topology,
        topology_ref.bundle_id.clone(),
        acceleration
            .as_ref()
            .map(|(bundle, bundle_id)| (bundle, bundle_id.clone())),
        |event| render_profile_compile_progress(&event, &mut progress_line_len),
    )
    .with_context(|| {
        format!(
            "compiling profile '{}' for dataset '{dataset}'",
            profile.profile.id
        )
    })?;
    let profile_hash = profile.fingerprint()?;
    let compile_id = format!("{dataset}-{}", &profile_hash[..12]);
    let bundle_path = paths
        .metric_bundles_dir
        .join(format!("metric-{compile_id}.bin"));
    render_profile_compile_message(
        "Write Bundle",
        format!("Writing compiled profile bundle {}", bundle_path.display()),
        false,
        &mut progress_line_len,
    );
    write_compiled_profile_bundle(&bundle_path, &compiled_bundle)?;
    let manifest = CompiledProfileManifest {
        compile_id: compile_id.clone(),
        dataset_id: netweevil_core::DatasetId::new(dataset.to_string()),
        profile_id: profile.profile.id.clone(),
        profile_hash,
        defaults_pack: profile.profile.defaults_pack.clone(),
        mode: profile.profile.mode,
        created_at: netweevil_report::now_rfc3339()?,
        topology_bundle_id: Some(topology_ref.bundle_id),
        edge_count: Some(compiled_bundle.edge_metrics.len() as u64),
        bundle: BundleRef {
            bundle_id: CacheBundleId::new(format!("metric-{compile_id}")),
            path: bundle_path.display().to_string(),
        },
    };
    render_profile_compile_message(
        "Write Manifest",
        format!(
            "Writing compiled profile manifest for '{}'",
            profile.profile.id
        ),
        false,
        &mut progress_line_len,
    );
    let path = write_compiled_profile_manifest(paths, &manifest)?;
    render_profile_compile_message(
        "Complete",
        format!(
            "Compiled profile '{}' for dataset '{}' into {} edge metrics",
            manifest.profile_id,
            manifest.dataset_id.0,
            compiled_bundle.edge_metrics.len()
        ),
        true,
        &mut progress_line_len,
    );
    println!(
        "compiled profile '{}' for dataset '{}' into {} edge metrics",
        manifest.profile_id,
        manifest.dataset_id.0,
        compiled_bundle.edge_metrics.len()
    );
    println!("compiled profile manifest written to {}", path.display());
    Ok(())
}

fn render_profile_compile_progress(event: &ProfileCompileProgress, last_line_len: &mut usize) {
    render_profile_compile_message(
        event.stage.label(),
        event.message.clone(),
        matches!(event.stage, ProfileCompileStage::Complete),
        last_line_len,
    );
}

fn render_profile_compile_message(
    stage_label: &str,
    message: String,
    done: bool,
    last_line_len: &mut usize,
) {
    let mut line = format!("[{stage_label}] {message}");
    if done {
        line.push_str(" [done]");
    }
    let padding = last_line_len.saturating_sub(line.len());
    eprint!("\r{line}{:padding$}", "");
    if done {
        eprintln!();
        *last_line_len = 0;
    } else {
        *last_line_len = line.len();
    }
}
