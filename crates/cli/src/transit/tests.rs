use clap::Parser;

use super::*;

fn parse_transit(args: &[&str]) -> TransitCommand {
    let cli = crate::Cli::try_parse_from(["netweevil", "transit"].iter().chain(args))
        .unwrap_or_else(|error| panic!("{args:?} should parse: {error}"));
    let crate::Command::Transit { command } = cli.command else {
        panic!("expected transit command");
    };
    command
}

#[test]
fn parses_transfer_build_command() {
    let TransitCommand::Transfers {
        command: TransitTransfersCommand::Build(args),
    } = parse_transit(&[
        "transfers",
        "build",
        "--feed",
        "hk-mtr",
        "--dataset",
        "hk-pedestrian",
        "--profile",
        "step-free.yml",
        "--max-transfer-distance-m",
        "750",
    ])
    else {
        panic!("expected transit transfers build command");
    };
    assert_eq!(args.feed, "hk-mtr");
    assert_eq!(args.dataset, "hk-pedestrian");
    assert_eq!(args.max_transfer_distance_m, 750.0);
    assert_eq!(args.max_candidates_per_stop, 32);
}

#[test]
fn parses_binding_apply_command() {
    let TransitCommand::Bindings {
        command: TransitBindingsCommand::Apply(args),
    } = parse_transit(&["bindings", "apply", "--feed", "hk-mtr", "bindings.json"])
    else {
        panic!("expected transit bindings apply command");
    };
    assert_eq!(args.feed, "hk-mtr");
    assert_eq!(args.bindings, PathBuf::from("bindings.json"));
}
