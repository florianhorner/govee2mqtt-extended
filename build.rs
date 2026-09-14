fn derive_git_ci_tag() -> Option<String> {
    let output = std::process::Command::new("git")
        .args(["show", "-s", "--format=%cd%n%H", "--date=format:%Y.%m.%d"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }

    let info = String::from_utf8(output.stdout).ok()?;
    let mut lines = info.lines();
    let release_date = lines.next()?;
    let commit = lines.next()?;
    if commit.len() != 40
        || lines.next().is_some()
        || !commit.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return None;
    }

    let candidate_prefix = commit.get(..8)?;
    Some(format!("{release_date}-{candidate_prefix}"))
}

fn main() {
    let mut ci_tag = String::new();

    if let Ok(env) = std::env::var("GOVEE_CI_TAG") {
        ci_tag = env.trim().to_string();
    } else if let Ok(tag) = std::fs::read(".tag") {
        if let Ok(s) = String::from_utf8(tag) {
            ci_tag = s.trim().to_string();
        }
    } else if let Some(tag) = derive_git_ci_tag() {
        ci_tag = tag;
    }

    println!("cargo:rerun-if-changed=.tag");
    println!("cargo:rustc-env=GOVEE_CI_TAG={ci_tag}");
}
