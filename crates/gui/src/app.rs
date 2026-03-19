use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use eframe::egui::{self, Color32, FontFamily, FontId, RichText, Vec2};
use netan_core::{CacheBundleId, CompiledProfileBundle, TopologyBundle};
use netan_ingest::{DatasetImportOptions, import_dataset};
use netan_persist::{
    WorkspacePaths, list_json_files, read_compiled_profile_bundle, read_compiled_profile_manifests,
    read_dataset_manifest, read_dataset_manifests, read_run_manifest, read_topology_bundle,
    write_compiled_profile_bundle, write_compiled_profile_manifest, write_run_manifest,
};
use netan_profile::{ProfileDocument, ReturnGeometry, compile_profile_bundle, load_profile};
use netan_query::{
    LabeledPoint, MatrixResult, OdResult, PointSetDocument, RouteRequest, RouteResult, SnapOptions,
    execute_matrix, execute_od, execute_route, load_od_pairs, load_point_set, load_route_request,
};
use netan_report::{
    BundleRef, CompiledProfileManifest, DatasetManifest, RunKind, RunManifest, RunResultSummary,
    RunStatus, SoftwareInfo, load_run_result_summary, new_run_manifest, write_matrix_result,
    write_od_result, write_route_result,
};

pub fn launch(paths: WorkspacePaths) -> Result<()> {
    let native_options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_inner_size(Vec2::new(1320.0, 860.0)),
        ..Default::default()
    };
    eframe::run_native(
        "netan",
        native_options,
        Box::new(move |cc| {
            configure_theme(&cc.egui_ctx);
            Ok(Box::new(NetanApp::new(paths.clone())))
        }),
    )
    .map_err(|err| anyhow::anyhow!(err.to_string()))
}

fn configure_theme(ctx: &egui::Context) {
    let mut style = (*ctx.style()).clone();
    style.spacing.item_spacing = Vec2::new(10.0, 10.0);
    style.spacing.button_padding = Vec2::new(12.0, 8.0);
    style.spacing.window_margin = egui::Margin::same(14);
    style.visuals = egui::Visuals::dark();
    style.visuals.override_text_color = Some(Color32::from_rgb(232, 236, 239));
    style.visuals.panel_fill = Color32::from_rgb(16, 24, 32);
    style.visuals.window_fill = Color32::from_rgb(16, 24, 32);
    style.visuals.extreme_bg_color = Color32::from_rgb(10, 16, 22);
    style.visuals.faint_bg_color = Color32::from_rgb(27, 38, 49);
    style.visuals.code_bg_color = Color32::from_rgb(12, 19, 25);
    style.visuals.selection.bg_fill = Color32::from_rgb(215, 123, 55);
    style.visuals.selection.stroke.color = Color32::from_rgb(250, 240, 229);
    style.text_styles.insert(
        egui::TextStyle::Heading,
        FontId::new(28.0, FontFamily::Proportional),
    );
    style.text_styles.insert(
        egui::TextStyle::Body,
        FontId::new(16.0, FontFamily::Proportional),
    );
    style.text_styles.insert(
        egui::TextStyle::Monospace,
        FontId::new(14.0, FontFamily::Monospace),
    );
    ctx.set_style(style);
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum NavTab {
    Overview,
    Datasets,
    Profiles,
    Analyses,
    Runs,
    Help,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum AnalysisMode {
    Route,
    Od,
    Matrix,
}

impl AnalysisMode {
    fn label(self) -> &'static str {
        match self {
            Self::Route => "Route",
            Self::Od => "OD",
            Self::Matrix => "Matrix",
        }
    }
}

#[derive(Default)]
struct DatasetForm {
    source_path: String,
    dataset_id: String,
}

#[derive(Default)]
struct ProfileForm {
    profile_path: String,
}

struct RouteBuilder {
    request_path: String,
    route_id: String,
    origin_id: String,
    origin_lon: String,
    origin_lat: String,
    destination_id: String,
    destination_lon: String,
    destination_lat: String,
    snap_distance_m: String,
}

impl Default for RouteBuilder {
    fn default() -> Self {
        Self {
            request_path: "examples/requests/route_gui.json".to_string(),
            route_id: "gui_route_001".to_string(),
            origin_id: "origin_a".to_string(),
            origin_lon: "6.5665".to_string(),
            origin_lat: "53.2194".to_string(),
            destination_id: "destination_b".to_string(),
            destination_lon: "6.5716".to_string(),
            destination_lat: "53.2148".to_string(),
            snap_distance_m: "500".to_string(),
        }
    }
}

struct AnalysisForm {
    mode: AnalysisMode,
    profile_path: String,
    route_request_path: String,
    od_pairs_path: String,
    matrix_origins_path: String,
    matrix_destinations_path: String,
    output_path: String,
    route_builder: RouteBuilder,
}

impl Default for AnalysisForm {
    fn default() -> Self {
        Self {
            mode: AnalysisMode::Route,
            profile_path: "examples/profiles/car_research_v1.yml".to_string(),
            route_request_path: "examples/requests/route.json".to_string(),
            od_pairs_path: "examples/requests/od_pairs.csv".to_string(),
            matrix_origins_path: "examples/requests/matrix_origins.csv".to_string(),
            matrix_destinations_path: "examples/requests/matrix_destinations.csv".to_string(),
            output_path: String::new(),
            route_builder: RouteBuilder::default(),
        }
    }
}

#[derive(Clone)]
struct ProfilePreview {
    path: PathBuf,
    profile: ProfileDocument,
    fingerprint: String,
}

#[derive(Clone)]
struct RunRecord {
    manifest_path: PathBuf,
    manifest: RunManifest,
    summary: Option<RunResultSummary>,
}

#[derive(Default)]
struct WorkspaceSnapshot {
    datasets: Vec<DatasetManifest>,
    compiled_profiles: Vec<CompiledProfileManifest>,
    runs: Vec<RunRecord>,
}

#[derive(Clone, Copy)]
enum StatusKind {
    Info,
    Error,
}

struct StatusMessage {
    kind: StatusKind,
    text: String,
}

pub struct NetanApp {
    paths: WorkspacePaths,
    nav: NavTab,
    snapshot: WorkspaceSnapshot,
    selected_dataset_id: String,
    selected_run: Option<String>,
    dataset_form: DatasetForm,
    profile_form: ProfileForm,
    analysis_form: AnalysisForm,
    profile_preview: Option<ProfilePreview>,
    status: StatusMessage,
}

impl NetanApp {
    fn new(paths: WorkspacePaths) -> Self {
        let mut app = Self {
            paths,
            nav: NavTab::Overview,
            snapshot: WorkspaceSnapshot::default(),
            selected_dataset_id: String::new(),
            selected_run: None,
            dataset_form: DatasetForm::default(),
            profile_form: ProfileForm {
                profile_path: "examples/profiles/car_research_v1.yml".to_string(),
            },
            analysis_form: AnalysisForm::default(),
            profile_preview: None,
            status: StatusMessage {
                kind: StatusKind::Info,
                text: "Workspace ready. Use example-relative paths or absolute paths.".to_string(),
            },
        };
        if let Err(error) = app.refresh_snapshot() {
            app.set_error(error);
        }
        app
    }

    fn refresh_snapshot(&mut self) -> Result<()> {
        let mut runs = Vec::new();
        for path in list_json_files(&self.paths.runs_dir)? {
            let manifest = read_run_manifest(&path)?;
            let summary = load_run_result_summary(&manifest)?;
            runs.push(RunRecord {
                manifest_path: path,
                manifest,
                summary,
            });
        }
        runs.sort_by(|left, right| right.manifest.created_at.cmp(&left.manifest.created_at));

        self.snapshot = WorkspaceSnapshot {
            datasets: read_dataset_manifests(&self.paths)?,
            compiled_profiles: read_compiled_profile_manifests(&self.paths)?,
            runs,
        };

        if self.selected_dataset_id.is_empty() {
            if let Some(dataset) = self.snapshot.datasets.first() {
                self.selected_dataset_id = dataset.dataset_id.0.clone();
            }
        } else if !self
            .snapshot
            .datasets
            .iter()
            .any(|dataset| dataset.dataset_id.0 == self.selected_dataset_id)
        {
            self.selected_dataset_id = self
                .snapshot
                .datasets
                .first()
                .map(|dataset| dataset.dataset_id.0.clone())
                .unwrap_or_default();
        }

        if self.selected_run.is_none() {
            self.selected_run = self
                .snapshot
                .runs
                .first()
                .map(|run| run.manifest.run_id.clone());
        }
        Ok(())
    }

    fn resolve_path(&self, raw: &str) -> PathBuf {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return self.paths.root.clone();
        }
        let candidate = Path::new(trimmed);
        if candidate.is_absolute() {
            candidate.to_path_buf()
        } else {
            self.paths.root.join(candidate)
        }
    }

    fn optional_output_path(&self, raw: &str) -> Option<PathBuf> {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(self.resolve_path(trimmed))
        }
    }

    fn set_info(&mut self, text: impl Into<String>) {
        self.status = StatusMessage {
            kind: StatusKind::Info,
            text: text.into(),
        };
    }

    fn set_error(&mut self, error: anyhow::Error) {
        self.status = StatusMessage {
            kind: StatusKind::Error,
            text: error.to_string(),
        };
    }

    fn active_run(&self) -> Option<&RunRecord> {
        self.selected_run.as_ref().and_then(|run_id| {
            self.snapshot
                .runs
                .iter()
                .find(|record| &record.manifest.run_id == run_id)
        })
    }

    fn import_dataset_action(&mut self) -> Result<()> {
        let source_path = self.resolve_path(&self.dataset_form.source_path);
        let dataset_id = self.dataset_form.dataset_id.trim();
        if dataset_id.is_empty() {
            bail!("dataset id is required");
        }
        let manifest = import_dataset(
            &self.paths,
            &source_path,
            DatasetImportOptions {
                name: dataset_id.to_string(),
                source: source_path.display().to_string(),
            },
        )?;
        self.selected_dataset_id = manifest.dataset_id.0.clone();
        self.refresh_snapshot()?;
        self.set_info(format!(
            "Imported dataset '{}' with {} nodes and {} directed edges.",
            manifest.dataset_id.0,
            manifest
                .topology_meta
                .as_ref()
                .map(|meta| meta.node_count)
                .unwrap_or_default(),
            manifest
                .topology_meta
                .as_ref()
                .map(|meta| meta.edge_count)
                .unwrap_or_default()
        ));
        Ok(())
    }

    fn validate_profile_action(&mut self) -> Result<()> {
        let path = self.resolve_path(&self.profile_form.profile_path);
        let profile = load_profile(&path)?;
        profile.validate()?;
        let fingerprint = profile.fingerprint()?;
        self.analysis_form.profile_path = self.profile_form.profile_path.clone();
        self.profile_preview = Some(ProfilePreview {
            path: path.clone(),
            profile,
            fingerprint: fingerprint.clone(),
        });
        self.set_info(format!(
            "Validated profile '{}' ({fingerprint}).",
            self.profile_preview
                .as_ref()
                .map(|preview| preview.profile.profile.id.as_str())
                .unwrap_or("unknown")
        ));
        Ok(())
    }

    fn load_validated_profile(&mut self) -> Result<ProfilePreview> {
        let path = self.resolve_path(&self.profile_form.profile_path);
        let profile = load_profile(&path)?;
        profile.validate()?;
        let preview = ProfilePreview {
            path,
            fingerprint: profile.fingerprint()?,
            profile,
        };
        self.profile_preview = Some(preview.clone());
        Ok(preview)
    }

    fn compile_profile_action(&mut self) -> Result<()> {
        let preview = self.load_validated_profile()?;
        let manifest =
            compile_profile_for_dataset(&self.paths, &self.selected_dataset_id, &preview.profile)?;
        self.refresh_snapshot()?;
        self.set_info(format!(
            "Compiled profile '{}' for dataset '{}'.",
            manifest.profile_id, manifest.dataset_id.0
        ));
        Ok(())
    }

    fn write_route_request_action(&mut self) -> Result<()> {
        let builder = &self.analysis_form.route_builder;
        let request_path = self.resolve_path(&builder.request_path);
        let request = RouteRequest {
            route_id: builder.route_id.trim().to_string(),
            origin: LabeledPoint {
                id: builder.origin_id.trim().to_string(),
                lon: parse_f64(&builder.origin_lon, "origin lon")?,
                lat: parse_f64(&builder.origin_lat, "origin lat")?,
            },
            destination: LabeledPoint {
                id: builder.destination_id.trim().to_string(),
                lon: parse_f64(&builder.destination_lon, "destination lon")?,
                lat: parse_f64(&builder.destination_lat, "destination lat")?,
            },
            snap: SnapOptions {
                max_distance_m: parse_f64(&builder.snap_distance_m, "snap distance")?,
            },
            returns: netan_profile::ReturnConfig {
                geometry: ReturnGeometry::Full,
                segment_rows: true,
                road_type_breakdown: vec![
                    netan_profile::BreakdownMetric::TimeS,
                    netan_profile::BreakdownMetric::DistanceM,
                ],
                surface_breakdown: vec![
                    netan_profile::BreakdownMetric::TimeS,
                    netan_profile::BreakdownMetric::DistanceM,
                ],
                penalty_breakdown: true,
                explain_cost_derivation: true,
            },
        };
        if request.route_id.trim().is_empty() {
            bail!("route id is required");
        }
        if let Some(parent) = request_path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("creating directory {}", parent.display()))?;
        }
        fs::write(&request_path, serde_json::to_string_pretty(&request)?)
            .with_context(|| format!("writing {}", request_path.display()))?;
        self.analysis_form.route_request_path =
            self.analysis_form.route_builder.request_path.clone();
        self.set_info(format!(
            "Wrote route request to {}.",
            request_path.display()
        ));
        Ok(())
    }

    fn run_analysis_action(&mut self) -> Result<()> {
        let dataset_id = self.selected_dataset_id.trim().to_string();
        if dataset_id.is_empty() {
            bail!("import a dataset first");
        }
        let profile_path_raw = self.analysis_form.profile_path.clone();
        let profile_path = self.resolve_path(&profile_path_raw);
        let profile = load_profile(&profile_path)?;
        profile.validate()?;
        let (topology, compiled_manifest, compiled_bundle) =
            load_or_compile_execution_inputs(&self.paths, &dataset_id, &profile)?;
        let engine = engine_description(&topology);
        match self.analysis_form.mode {
            AnalysisMode::Route => {
                let request_path = self.resolve_path(&self.analysis_form.route_request_path);
                let mut request = load_route_request(&request_path)?;
                let out = self.optional_output_path(&self.analysis_form.output_path);
                if out.as_deref().is_some_and(output_needs_geometry)
                    && matches!(request.returns.geometry, ReturnGeometry::None)
                {
                    request.returns.geometry = ReturnGeometry::Full;
                }
                let result = execute_route(&topology, &compiled_bundle, &request)?;
                let manifest = store_route_run(
                    &self.paths,
                    &dataset_id,
                    &profile,
                    &request_path,
                    &request,
                    &result,
                    &compiled_manifest,
                    engine,
                    out,
                )?;
                self.refresh_snapshot()?;
                self.selected_run = Some(manifest.run_id.clone());
                self.set_info(format!(
                    "Route '{}' solved. Result: {}",
                    result.route_id,
                    manifest.result_path.as_deref().unwrap_or("not written")
                ));
            }
            AnalysisMode::Od => {
                let request_path = self.resolve_path(&self.analysis_form.od_pairs_path);
                let mut request = load_od_pairs(&request_path)?;
                let out = self.optional_output_path(&self.analysis_form.output_path);
                if out.as_deref().is_some_and(output_needs_geometry)
                    && matches!(request.returns.geometry, ReturnGeometry::None)
                {
                    request.returns.geometry = ReturnGeometry::Full;
                }
                let result = execute_od(&topology, &compiled_bundle, &request)?;
                let manifest = store_od_run(
                    &self.paths,
                    &dataset_id,
                    &profile,
                    &request_path,
                    &request,
                    &result,
                    &compiled_manifest,
                    engine,
                    out,
                )?;
                self.refresh_snapshot()?;
                self.selected_run = Some(manifest.run_id.clone());
                self.set_info(format!(
                    "OD batch finished. Result: {}",
                    manifest.result_path.as_deref().unwrap_or("not written")
                ));
            }
            AnalysisMode::Matrix => {
                let origins_path = self.resolve_path(&self.analysis_form.matrix_origins_path);
                let destinations_path =
                    self.resolve_path(&self.analysis_form.matrix_destinations_path);
                let mut origins = load_point_set(&origins_path)?;
                let mut destinations = load_point_set(&destinations_path)?;
                let out = self.optional_output_path(&self.analysis_form.output_path);
                if out.as_deref().is_some_and(output_needs_geometry) {
                    if matches!(origins.returns.geometry, ReturnGeometry::None) {
                        origins.returns.geometry = ReturnGeometry::Full;
                    }
                    if matches!(destinations.returns.geometry, ReturnGeometry::None) {
                        destinations.returns.geometry = ReturnGeometry::Full;
                    }
                }
                let result = execute_matrix(&topology, &compiled_bundle, &origins, &destinations)?;
                let manifest = store_matrix_run(
                    &self.paths,
                    &dataset_id,
                    &profile,
                    &origins_path,
                    &destinations_path,
                    &origins,
                    &destinations,
                    &result,
                    &compiled_manifest,
                    engine,
                    out,
                )?;
                self.refresh_snapshot()?;
                self.selected_run = Some(manifest.run_id.clone());
                self.set_info(format!(
                    "Matrix batch finished. Result: {}",
                    manifest.result_path.as_deref().unwrap_or("not written")
                ));
            }
        }
        Ok(())
    }

    fn draw_header(&self, ctx: &egui::Context) {
        egui::TopBottomPanel::top("header")
            .exact_height(74.0)
            .show(ctx, |ui| {
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    ui.vertical(|ui| {
                        ui.label(
                            RichText::new("netan")
                                .size(30.0)
                                .strong()
                                .color(Color32::from_rgb(248, 183, 96)),
                        );
                        ui.label(
                            RichText::new(
                                "Research-first local routing with one cache, one schema, and spatial exports that QGIS can ingest directly.",
                            )
                            .size(14.0)
                            .color(Color32::from_rgb(176, 190, 198)),
                        );
                    });
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let badge = match self.status.kind {
                            StatusKind::Info => RichText::new("workspace ready")
                                .color(Color32::from_rgb(168, 222, 134)),
                            StatusKind::Error => {
                                RichText::new("attention required").color(Color32::from_rgb(255, 128, 128))
                            }
                        };
                        ui.label(badge);
                    });
                });
            });
    }

    fn draw_sidebar(&mut self, ctx: &egui::Context) {
        egui::SidePanel::left("nav")
            .resizable(false)
            .default_width(210.0)
            .show(ctx, |ui| {
                ui.add_space(4.0);
                ui.label(
                    RichText::new("Workspace")
                        .size(15.0)
                        .color(Color32::from_rgb(141, 161, 176)),
                );
                ui.monospace(self.paths.root.display().to_string());
                ui.separator();

                for (tab, label, hint) in [
                    (NavTab::Overview, "Overview", "state, examples, quick stats"),
                    (
                        NavTab::Datasets,
                        "Datasets",
                        "import and inspect topology bundles",
                    ),
                    (
                        NavTab::Profiles,
                        "Profiles",
                        "validate and compile profile bundles",
                    ),
                    (
                        NavTab::Analyses,
                        "Analyses",
                        "run route, OD, or matrix jobs",
                    ),
                    (
                        NavTab::Runs,
                        "Runs",
                        "inspect recent manifests and summaries",
                    ),
                    (NavTab::Help, "Help", "commands, examples, and plugin flow"),
                ] {
                    let selected = self.nav == tab;
                    let button = egui::Button::new(
                        RichText::new(format!("{label}\n{hint}")).size(if selected {
                            16.0
                        } else {
                            15.0
                        }),
                    )
                    .fill(if selected {
                        Color32::from_rgb(46, 61, 76)
                    } else {
                        Color32::from_rgb(22, 31, 40)
                    })
                    .min_size(Vec2::new(180.0, 52.0));
                    if ui.add(button).clicked() {
                        self.nav = tab;
                    }
                }

                ui.add_space(14.0);
                if ui.button("Refresh Workspace").clicked() {
                    if let Err(error) = self.refresh_snapshot() {
                        self.set_error(error);
                    } else {
                        self.set_info("Workspace snapshot refreshed.");
                    }
                }
                ui.separator();
                ui.small(format!(
                    "{} dataset(s)  {} compiled profile(s)  {} run(s)",
                    self.snapshot.datasets.len(),
                    self.snapshot.compiled_profiles.len(),
                    self.snapshot.runs.len()
                ));
            });
    }

    fn draw_status_bar(&self, ctx: &egui::Context) {
        egui::TopBottomPanel::bottom("status")
            .exact_height(34.0)
            .show(ctx, |ui| {
                let color = match self.status.kind {
                    StatusKind::Info => Color32::from_rgb(151, 214, 116),
                    StatusKind::Error => Color32::from_rgb(255, 120, 120),
                };
                ui.horizontal_wrapped(|ui| {
                    ui.label(RichText::new("Status").strong().color(color));
                    ui.label(&self.status.text);
                });
            });
    }

    fn draw_overview(&mut self, ui: &mut egui::Ui) {
        ui.heading("Overview");
        ui.label("The desktop shell is wired into the same manifests, bundles, and result writers as the CLI.");
        ui.add_space(10.0);

        metric_cards(
            ui,
            &[
                ("Datasets", self.snapshot.datasets.len().to_string()),
                (
                    "Compiled Profiles",
                    self.snapshot.compiled_profiles.len().to_string(),
                ),
                ("Runs", self.snapshot.runs.len().to_string()),
                ("State Dir", self.paths.state_dir.display().to_string()),
            ],
        );

        ui.add_space(10.0);
        egui::Grid::new("overview_paths")
            .num_columns(2)
            .spacing(Vec2::new(16.0, 8.0))
            .show(ui, |ui| {
                ui.label(RichText::new("Example profile").strong());
                ui.monospace("examples/profiles/car_research_v1.yml");
                ui.end_row();
                ui.label(RichText::new("Example route request").strong());
                ui.monospace("examples/requests/route.json");
                ui.end_row();
                ui.label(RichText::new("Example OD CSV").strong());
                ui.monospace("examples/requests/od_pairs.csv");
                ui.end_row();
                ui.label(RichText::new("Example matrix CSVs").strong());
                ui.monospace("examples/requests/matrix_origins.csv + matrix_destinations.csv");
                ui.end_row();
            });

        ui.add_space(10.0);
        ui.horizontal(|ui| {
            if ui.button("Load Example Dataset Path").clicked() {
                self.dataset_form.source_path = "datasets/groningen-260317.osm.pbf".to_string();
                self.dataset_form.dataset_id = "groningen_2026_03".to_string();
                self.nav = NavTab::Datasets;
            }
            if ui.button("Load Example Profile").clicked() {
                self.profile_form.profile_path =
                    "examples/profiles/car_research_v1.yml".to_string();
                self.analysis_form.profile_path = self.profile_form.profile_path.clone();
                self.nav = NavTab::Profiles;
            }
            if ui.button("Open Analysis Workspace").clicked() {
                self.nav = NavTab::Analyses;
            }
        });

        ui.add_space(14.0);
        ui.label(RichText::new("Recent runs").strong());
        if self.snapshot.runs.is_empty() {
            ui.label("No runs yet. Import a dataset, compile a profile, then execute a route or batch job.");
        } else {
            for run in self.snapshot.runs.iter().take(4) {
                draw_run_summary_card(ui, run, self.paths.root.as_path());
            }
        }
    }

    fn draw_datasets(&mut self, ui: &mut egui::Ui) {
        ui.heading("Datasets");
        ui.label("Import a local `.osm.pbf` into the persisted topology cache under `.netan/`.");
        ui.add_space(10.0);

        egui::Frame::group(ui.style())
            .fill(Color32::from_rgb(22, 31, 40))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label("Source");
                    ui.add(
                        egui::TextEdit::singleline(&mut self.dataset_form.source_path)
                            .desired_width(520.0),
                    );
                    if ui.button("Use Groningen Example").clicked() {
                        self.dataset_form.source_path = "datasets/groningen-260317.osm.pbf".to_string();
                        self.dataset_form.dataset_id = "groningen_2026_03".to_string();
                    }
                });
                ui.horizontal(|ui| {
                    ui.label("Dataset ID");
                    ui.add(
                        egui::TextEdit::singleline(&mut self.dataset_form.dataset_id)
                            .desired_width(240.0),
                    );
                    if ui.button("Import Dataset").clicked() {
                        if let Err(error) = self.import_dataset_action() {
                            self.set_error(error);
                        }
                    }
                });
                ui.small("Paths can be repository-relative or absolute. Import writes the dataset manifest and topology bundle into `.netan/`.");
            });

        ui.add_space(12.0);
        ui.label(RichText::new("Imported datasets").strong());
        if self.snapshot.datasets.is_empty() {
            ui.label("No datasets imported yet.");
            return;
        }

        egui::ScrollArea::vertical().show(ui, |ui| {
            for dataset in &self.snapshot.datasets {
                let selected = self.selected_dataset_id == dataset.dataset_id.0;
                egui::Frame::group(ui.style())
                    .fill(if selected {
                        Color32::from_rgb(39, 51, 64)
                    } else {
                        Color32::from_rgb(20, 29, 37)
                    })
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            if ui
                                .selectable_label(selected, &dataset.dataset_id.0)
                                .clicked()
                            {
                                self.selected_dataset_id = dataset.dataset_id.0.clone();
                            }
                            ui.label(
                                RichText::new(dataset.imported_at.as_str())
                                    .color(Color32::from_rgb(138, 154, 166)),
                            );
                        });
                        ui.monospace(relative_or_full(
                            Path::new(&dataset.source_path),
                            self.paths.root.as_path(),
                        ));
                        if let Some(meta) = dataset.topology_meta.as_ref() {
                            ui.small(format!(
                                "{} nodes, {} directed edges, {} turn restrictions",
                                meta.node_count, meta.edge_count, meta.turn_count
                            ));
                        }
                        if let Some(bundle) = dataset.topology_bundle.as_ref() {
                            ui.small(format!("bundle: {}", bundle.path));
                        }
                    });
                ui.add_space(8.0);
            }
        });
    }

    fn draw_profiles(&mut self, ui: &mut egui::Ui) {
        ui.heading("Profiles");
        ui.label("Validate the shared YAML/TOML schema and compile a metric bundle for the selected dataset.");
        ui.add_space(10.0);

        egui::Frame::group(ui.style())
            .fill(Color32::from_rgb(22, 31, 40))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label("Profile");
                    ui.add(
                        egui::TextEdit::singleline(&mut self.profile_form.profile_path)
                            .desired_width(520.0),
                    );
                    if ui.button("Use Example").clicked() {
                        self.profile_form.profile_path =
                            "examples/profiles/car_research_v1.yml".to_string();
                    }
                });
                dataset_selector(ui, &self.snapshot.datasets, &mut self.selected_dataset_id);
                ui.horizontal(|ui| {
                    if ui.button("Validate Profile").clicked() {
                        if let Err(error) = self.validate_profile_action() {
                            self.set_error(error);
                        }
                    }
                    if ui.button("Compile For Dataset").clicked() {
                        if let Err(error) = self.compile_profile_action() {
                            self.set_error(error);
                        }
                    }
                });
            });

        ui.add_space(12.0);
        if let Some(preview) = self.profile_preview.as_ref() {
            egui::Frame::group(ui.style())
                .fill(Color32::from_rgb(20, 29, 37))
                .show(ui, |ui| {
                    ui.label(RichText::new("Validated profile").strong());
                    ui.label(format!(
                        "{} ({:?})",
                        preview.profile.profile.id, preview.profile.profile.mode
                    ));
                    ui.monospace(preview.path.display().to_string());
                    ui.small(format!(
                        "defaults pack: {}",
                        preview.profile.profile.defaults_pack
                    ));
                    ui.small(format!("fingerprint: {}", preview.fingerprint));
                    ui.small(format!(
                        "{} speed rules, {} factors",
                        preview.profile.speed_rules.len(),
                        preview.profile.factors.len()
                    ));
                });
        }

        ui.add_space(12.0);
        ui.label(RichText::new("Compiled profile bundles").strong());
        if self.snapshot.compiled_profiles.is_empty() {
            ui.label("No compiled profiles yet.");
            return;
        }

        egui::ScrollArea::vertical().show(ui, |ui| {
            for profile in &self.snapshot.compiled_profiles {
                egui::Frame::group(ui.style())
                    .fill(Color32::from_rgb(20, 29, 37))
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.label(RichText::new(profile.profile_id.as_str()).strong());
                            ui.label(format!("dataset {}", profile.dataset_id.0));
                        });
                        ui.small(format!("compile id: {}", profile.compile_id));
                        ui.small(format!("profile hash: {}", profile.profile_hash));
                        ui.small(format!(
                            "mode {:?}, defaults {}, edge count {}",
                            profile.mode,
                            profile.defaults_pack,
                            profile.edge_count.unwrap_or_default()
                        ));
                        ui.monospace(profile.bundle.path.as_str());
                    });
                ui.add_space(8.0);
            }
        });
    }

    fn draw_analyses(&mut self, ui: &mut egui::Ui) {
        ui.heading("Analyses");
        ui.label("Use the same persisted bundles and result writers as the CLI. Geometry is auto-enabled when the selected output format needs it.");
        ui.add_space(10.0);

        egui::Frame::group(ui.style())
            .fill(Color32::from_rgb(22, 31, 40))
            .show(ui, |ui| {
                dataset_selector(ui, &self.snapshot.datasets, &mut self.selected_dataset_id);
                ui.horizontal(|ui| {
                    ui.label("Profile");
                    ui.add(
                        egui::TextEdit::singleline(&mut self.analysis_form.profile_path)
                            .desired_width(520.0),
                    );
                    if ui.button("Use Example").clicked() {
                        self.analysis_form.profile_path =
                            "examples/profiles/car_research_v1.yml".to_string();
                    }
                });
                ui.horizontal(|ui| {
                    ui.label("Mode");
                    for mode in [AnalysisMode::Route, AnalysisMode::Od, AnalysisMode::Matrix] {
                        ui.selectable_value(&mut self.analysis_form.mode, mode, mode.label());
                    }
                });

                match self.analysis_form.mode {
                    AnalysisMode::Route => {
                        ui.horizontal(|ui| {
                            ui.label("Request");
                            ui.add(
                                egui::TextEdit::singleline(
                                    &mut self.analysis_form.route_request_path,
                                )
                                .desired_width(520.0),
                            );
                            if ui.button("Use Example").clicked() {
                                self.analysis_form.route_request_path =
                                    "examples/requests/route.json".to_string();
                            }
                        });
                    }
                    AnalysisMode::Od => {
                        ui.horizontal(|ui| {
                            ui.label("Pairs CSV/JSON");
                            ui.add(
                                egui::TextEdit::singleline(&mut self.analysis_form.od_pairs_path)
                                    .desired_width(520.0),
                            );
                            if ui.button("Use Example").clicked() {
                                self.analysis_form.od_pairs_path =
                                    "examples/requests/od_pairs.csv".to_string();
                            }
                        });
                    }
                    AnalysisMode::Matrix => {
                        ui.horizontal(|ui| {
                            ui.label("Origins");
                            ui.add(
                                egui::TextEdit::singleline(
                                    &mut self.analysis_form.matrix_origins_path,
                                )
                                .desired_width(520.0),
                            );
                        });
                        ui.horizontal(|ui| {
                            ui.label("Destinations");
                            ui.add(
                                egui::TextEdit::singleline(
                                    &mut self.analysis_form.matrix_destinations_path,
                                )
                                .desired_width(520.0),
                            );
                            if ui.button("Use Example").clicked() {
                                self.analysis_form.matrix_origins_path =
                                    "examples/requests/matrix_origins.csv".to_string();
                                self.analysis_form.matrix_destinations_path =
                                    "examples/requests/matrix_destinations.csv".to_string();
                            }
                        });
                    }
                }

                ui.horizontal(|ui| {
                    ui.label("Output");
                    ui.add(
                        egui::TextEdit::singleline(&mut self.analysis_form.output_path)
                            .desired_width(520.0),
                    );
                    ui.small("Leave blank for `.netan/runs/<run-id>-result.json`.");
                });

                if ui.button("Execute Analysis").clicked() {
                    if let Err(error) = self.run_analysis_action() {
                        self.set_error(error);
                    }
                }
            });

        ui.add_space(12.0);
        egui::CollapsingHeader::new("Route Request Builder")
            .default_open(true)
            .show(ui, |ui| {
                ui.label("Write a route request JSON without leaving the GUI. QGIS can then reuse the same file.");
                let builder = &mut self.analysis_form.route_builder;
                ui.horizontal(|ui| {
                    ui.label("Request path");
                    ui.add(
                        egui::TextEdit::singleline(&mut builder.request_path)
                            .desired_width(420.0),
                    );
                    if ui.button("Use Example Path").clicked() {
                        builder.request_path = "examples/requests/route_gui.json".to_string();
                    }
                });
                ui.horizontal(|ui| {
                    ui.label("Route id");
                    ui.add(egui::TextEdit::singleline(&mut builder.route_id).desired_width(180.0));
                    ui.label("Snap m");
                    ui.add(
                        egui::TextEdit::singleline(&mut builder.snap_distance_m)
                            .desired_width(90.0),
                    );
                });
                ui.columns(2, |columns| {
                    columns[0].label(RichText::new("Origin").strong());
                    columns[0].horizontal(|ui| {
                        ui.label("ID");
                        ui.add(
                            egui::TextEdit::singleline(&mut builder.origin_id)
                                .desired_width(140.0),
                        );
                    });
                    columns[0].horizontal(|ui| {
                        ui.label("Lon");
                        ui.add(
                            egui::TextEdit::singleline(&mut builder.origin_lon)
                                .desired_width(120.0),
                        );
                        ui.label("Lat");
                        ui.add(
                            egui::TextEdit::singleline(&mut builder.origin_lat)
                                .desired_width(120.0),
                        );
                    });
                    columns[1].label(RichText::new("Destination").strong());
                    columns[1].horizontal(|ui| {
                        ui.label("ID");
                        ui.add(
                            egui::TextEdit::singleline(&mut builder.destination_id)
                                .desired_width(140.0),
                        );
                    });
                    columns[1].horizontal(|ui| {
                        ui.label("Lon");
                        ui.add(
                            egui::TextEdit::singleline(&mut builder.destination_lon)
                                .desired_width(120.0),
                        );
                        ui.label("Lat");
                        ui.add(
                            egui::TextEdit::singleline(&mut builder.destination_lat)
                                .desired_width(120.0),
                        );
                    });
                });
                if ui.button("Write Route Request").clicked() {
                    if let Err(error) = self.write_route_request_action() {
                        self.set_error(error);
                    }
                }
            });
    }

    fn draw_runs(&mut self, ui: &mut egui::Ui) {
        ui.heading("Runs");
        ui.label("Inspect the persisted run manifests and JSON summaries that also back report rendering.");
        ui.add_space(10.0);

        ui.columns(2, |columns| {
            columns[0].label(RichText::new("Recent run manifests").strong());
            egui::ScrollArea::vertical().show(&mut columns[0], |ui| {
                if self.snapshot.runs.is_empty() {
                    ui.label("No runs written yet.");
                }
                for run in &self.snapshot.runs {
                    let selected =
                        self.selected_run.as_deref() == Some(run.manifest.run_id.as_str());
                    let label = format!(
                        "{}  {}  {}",
                        run.manifest.created_at,
                        run_kind_label(run.manifest.run_kind),
                        run.manifest.dataset_id
                    );
                    if ui.selectable_label(selected, label).clicked() {
                        self.selected_run = Some(run.manifest.run_id.clone());
                    }
                }
            });

            columns[1].label(RichText::new("Selected run").strong());
            egui::ScrollArea::vertical().show(&mut columns[1], |ui| {
                if let Some(run) = self.active_run() {
                    draw_run_detail(ui, run);
                } else {
                    ui.label("Select a run to inspect.");
                }
            });
        });
    }

    fn draw_help(&mut self, ui: &mut egui::Ui) {
        ui.heading("Help");
        ui.label("CLI, GUI, and QGIS all target the same local workspace under `.netan/`.");
        ui.add_space(12.0);
        ui.label(RichText::new("Typical flow").strong());
        ui.label("1. Import a dataset from `datasets/*.osm.pbf` or another local extract.");
        ui.label("2. Validate and compile a profile against that dataset.");
        ui.label("3. Run a route, OD, or matrix analysis and choose a spatial output such as `.geojson` or `.gpkg` when needed.");
        ui.label("4. Open the result directly in QGIS or use the included plugin to execute the CLI from QGIS.");

        ui.add_space(12.0);
        ui.label(RichText::new("Example commands").strong());
        code_block(
            ui,
            "cargo run -p netan-cli -- dataset import datasets/groningen-260317.osm.pbf --name groningen_2026_03\ncargo run -p netan-cli -- profile compile --dataset groningen_2026_03 --profile examples/profiles/car_research_v1.yml\ncargo run -p netan-cli -- analyze route --dataset groningen_2026_03 --profile examples/profiles/car_research_v1.yml --request examples/requests/route.json --out .netan/runs/example-route.geojson\ncargo run -p netan-cli -- gui",
        );

        ui.add_space(12.0);
        ui.label(RichText::new("QGIS export targets").strong());
        ui.label("Use `.geojson`, `.gpkg`, or `.geoparquet` outputs for easy loading in QGIS.");
        ui.label("The plugin included under `qgis_plugin/netan_qgis/` can build route request JSON, call `netan analyze ...`, and load the written layer.");
    }
}

impl eframe::App for NetanApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.draw_header(ctx);
        self.draw_sidebar(ctx);
        self.draw_status_bar(ctx);

        egui::CentralPanel::default().show(ctx, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| match self.nav {
                NavTab::Overview => self.draw_overview(ui),
                NavTab::Datasets => self.draw_datasets(ui),
                NavTab::Profiles => self.draw_profiles(ui),
                NavTab::Analyses => self.draw_analyses(ui),
                NavTab::Runs => self.draw_runs(ui),
                NavTab::Help => self.draw_help(ui),
            });
        });
    }
}

fn dataset_selector(
    ui: &mut egui::Ui,
    datasets: &[DatasetManifest],
    selected_dataset_id: &mut String,
) {
    ui.horizontal(|ui| {
        ui.label("Dataset");
        if datasets.is_empty() {
            ui.label("No datasets imported yet");
            return;
        }
        egui::ComboBox::from_id_salt(ui.id().with("dataset_selector"))
            .selected_text(selected_dataset_id.as_str())
            .show_ui(ui, |ui| {
                for dataset in datasets {
                    ui.selectable_value(
                        selected_dataset_id,
                        dataset.dataset_id.0.clone(),
                        dataset.dataset_id.0.as_str(),
                    );
                }
            });
    });
}

fn metric_cards(ui: &mut egui::Ui, items: &[(&str, String)]) {
    ui.columns(items.len(), |columns| {
        for (column, (label, value)) in columns.iter_mut().zip(items.iter()) {
            egui::Frame::group(column.style())
                .fill(Color32::from_rgb(23, 33, 42))
                .show(column, |ui| {
                    ui.set_min_height(86.0);
                    ui.label(
                        RichText::new(*label)
                            .size(14.0)
                            .color(Color32::from_rgb(151, 168, 180)),
                    );
                    ui.add_space(4.0);
                    ui.label(
                        RichText::new(value.as_str())
                            .size(20.0)
                            .strong()
                            .color(Color32::from_rgb(245, 241, 230)),
                    );
                });
        }
    });
}

fn draw_run_summary_card(ui: &mut egui::Ui, run: &RunRecord, root: &Path) {
    egui::Frame::group(ui.style())
        .fill(Color32::from_rgb(20, 29, 37))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new(run_kind_label(run.manifest.run_kind)).strong());
                ui.label(format!(
                    "{}  dataset {}",
                    run.manifest.created_at, run.manifest.dataset_id
                ));
            });
            ui.small(relative_or_full(&run.manifest_path, root));
            if let Some(summary) = run.summary.as_ref() {
                ui.small(format_run_summary(summary));
            }
            ui.small(run.manifest.message.as_str());
        });
}

fn draw_run_detail(ui: &mut egui::Ui, run: &RunRecord) {
    ui.label(RichText::new(run.manifest.run_id.as_str()).strong());
    ui.small(format!(
        "{} {} {}",
        run.manifest.created_at,
        run_kind_label(run.manifest.run_kind),
        status_label(run.manifest.status)
    ));
    ui.separator();
    ui.label(format!("Dataset: {}", run.manifest.dataset_id));
    ui.label(format!("Profile: {}", run.manifest.profile_id));
    ui.label(format!("Request: {}", run.manifest.request_source));
    ui.label(format!("Algorithm: {}", run.manifest.algorithm.engine));
    if let Some(result_path) = run.manifest.result_path.as_deref() {
        ui.monospace(result_path);
    }
    ui.monospace(run.manifest_path.display().to_string());
    ui.add_space(8.0);
    ui.label(run.manifest.message.as_str());
    ui.add_space(8.0);
    if let Some(summary) = run.summary.as_ref() {
        ui.label(RichText::new("Result summary").strong());
        code_block(ui, &format_run_summary(summary));
    }
}

fn code_block(ui: &mut egui::Ui, code: &str) {
    egui::Frame::group(ui.style())
        .fill(Color32::from_rgb(12, 19, 25))
        .show(ui, |ui| {
            ui.monospace(code);
        });
}

fn parse_f64(raw: &str, label: &str) -> Result<f64> {
    raw.trim()
        .parse::<f64>()
        .with_context(|| format!("invalid {label}"))
}

fn relative_or_full(path: &Path, root: &Path) -> String {
    path.strip_prefix(root)
        .map(|relative| relative.display().to_string())
        .unwrap_or_else(|_| path.display().to_string())
}

fn compile_profile_for_dataset(
    paths: &WorkspacePaths,
    dataset_id: &str,
    profile: &ProfileDocument,
) -> Result<CompiledProfileManifest> {
    let dataset_manifest = read_dataset_manifest(paths, dataset_id)
        .with_context(|| format!("reading dataset manifest for '{dataset_id}'"))?;
    let topology_ref = dataset_manifest
        .topology_bundle
        .clone()
        .context("dataset is missing a topology bundle; run dataset import first")?;
    let topology: TopologyBundle = read_topology_bundle(&topology_ref.path)
        .with_context(|| format!("reading topology bundle {}", topology_ref.path))?;
    let compiled_bundle =
        compile_profile_bundle(profile, &topology, topology_ref.bundle_id.clone()).with_context(
            || {
                format!(
                    "compiling profile '{}' for dataset '{dataset_id}'",
                    profile.profile.id
                )
            },
        )?;
    let profile_hash = profile.fingerprint()?;
    let compile_id = format!("{dataset_id}-{}", &profile_hash[..12]);
    let bundle_path = paths
        .metric_bundles_dir
        .join(format!("metric-{compile_id}.bin"));
    write_compiled_profile_bundle(&bundle_path, &compiled_bundle)?;
    let manifest = CompiledProfileManifest {
        compile_id: compile_id.clone(),
        dataset_id: netan_core::DatasetId::new(dataset_id.to_string()),
        profile_id: profile.profile.id.clone(),
        profile_hash,
        defaults_pack: profile.profile.defaults_pack.clone(),
        mode: profile.profile.mode,
        created_at: netan_report::now_rfc3339()?,
        topology_bundle_id: Some(topology_ref.bundle_id),
        edge_count: Some(compiled_bundle.edge_metrics.len() as u64),
        bundle: BundleRef {
            bundle_id: CacheBundleId::new(format!("metric-{compile_id}")),
            path: bundle_path.display().to_string(),
        },
    };
    write_compiled_profile_manifest(paths, &manifest)?;
    Ok(manifest)
}

fn load_or_compile_execution_inputs(
    paths: &WorkspacePaths,
    dataset_id: &str,
    profile: &ProfileDocument,
) -> Result<(
    TopologyBundle,
    CompiledProfileManifest,
    CompiledProfileBundle,
)> {
    let dataset_manifest = read_dataset_manifest(paths, dataset_id)
        .with_context(|| format!("reading dataset manifest for '{dataset_id}'"))?;
    let topology_ref = dataset_manifest
        .topology_bundle
        .clone()
        .context("dataset is missing a topology bundle; run dataset import first")?;
    let topology: TopologyBundle = read_topology_bundle(&topology_ref.path)
        .with_context(|| format!("reading topology bundle {}", topology_ref.path))?;
    let wanted_hash = profile.fingerprint()?;
    let compiled_manifest = read_compiled_profile_manifests(paths)?
        .into_iter()
        .find(|manifest| {
            manifest.dataset_id.0 == dataset_id && manifest.profile_hash == wanted_hash
        })
        .unwrap_or(compile_profile_for_dataset(paths, dataset_id, profile)?);
    let compiled_bundle: CompiledProfileBundle =
        read_compiled_profile_bundle(&compiled_manifest.bundle.path).with_context(|| {
            format!(
                "reading compiled profile bundle {}",
                compiled_manifest.bundle.path
            )
        })?;
    Ok((topology, compiled_manifest, compiled_bundle))
}

#[derive(Debug, Clone, Copy)]
struct EngineDescription {
    route_engine: &'static str,
    route_summary: &'static str,
    batch_engine: &'static str,
    batch_summary: &'static str,
}

fn engine_description(topology: &TopologyBundle) -> EngineDescription {
    let has_multi_edge_restrictions = topology
        .turn_restrictions
        .iter()
        .any(|restriction| restriction.edge_path.len() > 2);
    if has_multi_edge_restrictions {
        EngineDescription {
            route_engine: "astar_exact_multi_edge_turns",
            route_summary: "Exact forward A* shortest-path search over the compiled directed edge graph with nearest-node snapping and persisted multi-edge turn-restriction sequences. Turn penalties are not modeled yet.",
            batch_engine: "astar_exact_multi_edge_turns_repeated",
            batch_summary: "Repeated exact forward A* shortest-path searches over the compiled directed edge graph, one OD pair or matrix cell at a time, with nearest-node snapping and persisted multi-edge turn-restriction sequences. Turn penalties are not modeled yet.",
        }
    } else {
        EngineDescription {
            route_engine: "bidirectional_dijkstra_exact",
            route_summary: "Exact bidirectional Dijkstra shortest-path search over the compiled directed edge graph with nearest-node snapping and persisted pairwise turn prohibitions. Turn penalties are not modeled yet.",
            batch_engine: "bidirectional_dijkstra_exact_repeated",
            batch_summary: "Repeated exact bidirectional Dijkstra shortest-path searches over the compiled directed edge graph, one OD pair or matrix cell at a time, with nearest-node snapping and persisted pairwise turn prohibitions. Turn penalties are not modeled yet.",
        }
    }
}

fn software_info() -> SoftwareInfo {
    SoftwareInfo {
        executable: "netan".to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        git_commit: option_env!("NETAN_GIT_COMMIT").map(ToString::to_string),
    }
}

fn output_needs_geometry(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| {
            ext.eq_ignore_ascii_case("csv")
                || ext.eq_ignore_ascii_case("geojson")
                || ext.eq_ignore_ascii_case("gpkg")
                || ext.eq_ignore_ascii_case("geopackage")
                || ext.eq_ignore_ascii_case("geoparquet")
                || ext.eq_ignore_ascii_case("gpq")
        })
        .unwrap_or(false)
}

fn store_route_run(
    paths: &WorkspacePaths,
    dataset_id: &str,
    profile: &ProfileDocument,
    request_path: &Path,
    request: &RouteRequest,
    result: &RouteResult,
    compiled_manifest: &CompiledProfileManifest,
    engine: EngineDescription,
    out: Option<PathBuf>,
) -> Result<RunManifest> {
    let mut manifest = new_run_manifest(
        RunKind::Route,
        dataset_id.to_string(),
        profile,
        request_path.display().to_string(),
        RunStatus::Succeeded,
        format!(
            "Route '{}' solved from node {} to node {} across {} edges.",
            result.route_id,
            result.origin.snapped_node_id,
            result.destination.snapped_node_id,
            result.summary.segment_count
        ),
        software_info(),
        Some(compiled_manifest.bundle.bundle_id.0.clone()),
    )?;
    manifest.algorithm.engine = engine.route_engine.to_string();
    manifest.methods_summary.plain_language = engine.route_summary.to_string();
    let result_path = out.unwrap_or_else(|| {
        paths
            .runs_dir
            .join(format!("{}-result.json", manifest.run_id))
    });
    write_route_result(&result_path, request, result)?;
    manifest.result_path = Some(result_path.display().to_string());
    write_run_manifest(paths, &manifest)?;
    Ok(manifest)
}

fn store_od_run(
    paths: &WorkspacePaths,
    dataset_id: &str,
    profile: &ProfileDocument,
    pairs_path: &Path,
    request: &netan_query::OdPairsDocument,
    result: &OdResult,
    compiled_manifest: &CompiledProfileManifest,
    engine: EngineDescription,
    out: Option<PathBuf>,
) -> Result<RunManifest> {
    let mut manifest = new_run_manifest(
        RunKind::Od,
        dataset_id.to_string(),
        profile,
        pairs_path.display().to_string(),
        RunStatus::Succeeded,
        format!(
            "OD batch completed with {} succeeded pairs and {} failed pairs.",
            result.succeeded_count, result.failed_count
        ),
        software_info(),
        Some(compiled_manifest.bundle.bundle_id.0.clone()),
    )?;
    manifest.algorithm.engine = engine.batch_engine.to_string();
    manifest.methods_summary.plain_language = engine.batch_summary.to_string();
    let result_path = out.unwrap_or_else(|| {
        paths
            .runs_dir
            .join(format!("{}-result.json", manifest.run_id))
    });
    write_od_result(&result_path, request, result)?;
    manifest.result_path = Some(result_path.display().to_string());
    write_run_manifest(paths, &manifest)?;
    Ok(manifest)
}

fn store_matrix_run(
    paths: &WorkspacePaths,
    dataset_id: &str,
    profile: &ProfileDocument,
    origins_path: &Path,
    destinations_path: &Path,
    origins: &PointSetDocument,
    destinations: &PointSetDocument,
    result: &MatrixResult,
    compiled_manifest: &CompiledProfileManifest,
    engine: EngineDescription,
    out: Option<PathBuf>,
) -> Result<RunManifest> {
    let mut manifest = new_run_manifest(
        RunKind::Matrix,
        dataset_id.to_string(),
        profile,
        format!(
            "{} | {}",
            origins_path.display(),
            destinations_path.display()
        ),
        RunStatus::Succeeded,
        format!(
            "Matrix batch completed with {} succeeded cells and {} failed cells.",
            result.succeeded_count, result.failed_count
        ),
        software_info(),
        Some(compiled_manifest.bundle.bundle_id.0.clone()),
    )?;
    manifest.algorithm.engine = engine.batch_engine.to_string();
    manifest.methods_summary.plain_language = engine.batch_summary.to_string();
    let result_path = out.unwrap_or_else(|| {
        paths
            .runs_dir
            .join(format!("{}-result.json", manifest.run_id))
    });
    write_matrix_result(&result_path, origins, destinations, result)?;
    manifest.result_path = Some(result_path.display().to_string());
    write_run_manifest(paths, &manifest)?;
    Ok(manifest)
}

fn run_kind_label(kind: RunKind) -> &'static str {
    match kind {
        RunKind::Route => "Route",
        RunKind::Od => "OD",
        RunKind::Matrix => "Matrix",
        RunKind::Experiment => "Experiment",
    }
}

fn status_label(status: RunStatus) -> &'static str {
    match status {
        RunStatus::Planned => "planned",
        RunStatus::Succeeded => "succeeded",
        RunStatus::Failed => "failed",
        RunStatus::NotImplemented => "not_implemented",
    }
}

fn format_run_summary(summary: &RunResultSummary) -> String {
    match summary {
        RunResultSummary::Route(route) => format!(
            "route_id: {}\ndistance_m: {}\ntravel_time_s: {:.3}\ngeneralized_cost: {:.3}\nsegments: {}\nwarnings: {}",
            route.route_id,
            route.total_distance_m,
            route.total_travel_time_s,
            route.total_generalized_cost,
            route.segment_count,
            route.warnings.join(" | ")
        ),
        RunResultSummary::Batch(batch) => format!(
            "label: {}\nitems: {}\nsucceeded: {}\nfailed: {}\nwarnings: {}",
            batch.label,
            batch.item_count,
            batch.succeeded_count,
            batch.failed_count,
            batch.warnings.join(" | ")
        ),
    }
}
