use std::process::Command;

fn main() {
    // HEAD is usually symbolic and does not change when its branch advances.
    // Resolve Git paths so regular clones and linked worktrees both rebuild.
    for path in ["HEAD", "index", "packed-refs"] {
        watch_git_path(path);
    }
    if let Ok(output) = Command::new("git")
        .args(["symbolic-ref", "-q", "HEAD"])
        .output()
        && output.status.success()
    {
        watch_git_path(String::from_utf8_lossy(&output.stdout).trim());
    }

    if let Ok(output) = Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        && output.status.success()
    {
        let commit = String::from_utf8_lossy(&output.stdout).trim().to_string();
        println!("cargo:rustc-env=NETWEEVIL_GIT_COMMIT={commit}");
    }
}

fn watch_git_path(name: &str) {
    if let Ok(output) = Command::new("git")
        .args(["rev-parse", "--git-path", name])
        .output()
        && output.status.success()
    {
        println!(
            "cargo:rerun-if-changed={}",
            String::from_utf8_lossy(&output.stdout).trim()
        );
    }
}
