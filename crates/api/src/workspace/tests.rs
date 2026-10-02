use super::*;

fn temp_workspace() -> WorkspacePaths {
    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    WorkspacePaths::discover(std::env::temp_dir().join(format!("netweevil-ws-test-{unique}")))
        .unwrap()
}

/// Request fields that name files resolve only inside the state directory
/// (uploads, saved profiles) or the example profiles; server paths, `..`
/// traversal and path-shaped ids are rejected before any file is opened.
#[test]
fn workspace_requests_cannot_reach_files_outside_the_state_directory() {
    let paths = temp_workspace();
    let outside = paths.root.join("outside.yml");
    fs::write(&outside, "profile: {}\n").unwrap();
    fs::write(paths.uploads_dir.join("city.osm.pbf"), b"pbf").unwrap();
    fs::write(paths.profiles_dir.join("car.yml"), "profile: {}\n").unwrap();

    assert_eq!(
        resolve_upload(&paths, "city.osm.pbf").unwrap(),
        paths.uploads_dir.join("city.osm.pbf")
    );
    let outside_text = outside.display().to_string();
    for source in [
        outside_text.as_str(),
        "../outside.yml",
        "uploads/../../outside.yml",
        "/etc/hosts",
    ] {
        assert!(resolve_upload(&paths, source).is_err(), "{source}");
    }

    let saved = paths.profiles_dir.join("car.yml").canonicalize().unwrap();
    assert_eq!(resolve_profile_source(&paths, "car.yml").unwrap(), saved);
    assert_eq!(
        resolve_profile_source(&paths, &saved.display().to_string()).unwrap(),
        saved
    );
    let traversal = paths.profiles_dir.join("../../outside.yml");
    for profile in [
        outside_text.as_str(),
        traversal.to_str().unwrap(),
        "../outside.yml",
        "/etc/hosts",
    ] {
        assert!(
            resolve_profile_source(&paths, profile).is_err(),
            "{profile}"
        );
    }

    for id in ["../uploads/evil", "/etc/passwd", "a/b", ""] {
        assert!(!is_identifier(id), "{id}");
    }
    assert!(remove_dataset(&paths, "../../outside").is_err());
    remove_workspace_file(&paths, &paths.state_dir.join("../outside.yml"));
    assert!(
        outside.exists(),
        "files outside .netweevil are never deleted"
    );
    let _ = fs::remove_dir_all(&paths.root);
}
