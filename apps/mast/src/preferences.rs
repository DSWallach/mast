//! Copy the previous bundle's preferences once, when the destination is empty.

#[cfg(any(target_os = "macos", test))]
fn legacy_domain(bundle_id: &str) -> Option<&'static str> {
    match bundle_id {
        "computer.mikey.mast" => Some("sh.zeron.app"),
        "computer.mikey.mast.dev" => Some("sh.zeron.app.dev"),
        _ => None,
    }
}

#[cfg(any(target_os = "macos", test))]
fn preferences_empty(read_succeeded: bool, output: &str, error: &str) -> bool {
    if read_succeeded {
        output
            .chars()
            .filter(|c| !c.is_whitespace())
            .collect::<String>()
            == "{}"
    } else {
        error.contains("does not exist")
    }
}

#[cfg(all(target_os = "macos", not(test)))]
pub fn migrate() {
    use std::io::Write;
    use std::process::{Command, Stdio};
    let Some(plist) = std::env::current_exe().ok().and_then(|exe| {
        exe.parent()?
            .parent()
            .map(|contents| contents.join("Info.plist"))
    }) else {
        return;
    };
    let Ok(bundle) = Command::new("/usr/libexec/PlistBuddy")
        .args(["-c", "Print :CFBundleIdentifier"])
        .arg(plist)
        .output()
    else {
        return;
    };
    if !bundle.status.success() {
        return;
    }
    let bundle = String::from_utf8_lossy(&bundle.stdout);
    let bundle = bundle.trim();
    let Some(legacy) = legacy_domain(bundle) else {
        return;
    };
    let Ok(current) = Command::new("/usr/bin/defaults")
        .args(["read", bundle])
        .output()
    else {
        return;
    };
    if !preferences_empty(
        current.status.success(),
        &String::from_utf8_lossy(&current.stdout),
        &String::from_utf8_lossy(&current.stderr),
    ) {
        return;
    }
    let Ok(export) = Command::new("/usr/bin/defaults")
        .args(["export", legacy, "-"])
        .output()
    else {
        return;
    };
    if !export.status.success() {
        return;
    }
    let Ok(mut import) = Command::new("/usr/bin/defaults")
        .args(["import", bundle, "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    else {
        return;
    };
    if let Some(mut stdin) = import.stdin.take() {
        let _ = stdin.write_all(&export.stdout);
    }
    let _ = import.wait();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_only_distribution_bundle_domains() {
        assert_eq!(legacy_domain("computer.mikey.mast"), Some("sh.zeron.app"));
        assert_eq!(
            legacy_domain("computer.mikey.mast.dev"),
            Some("sh.zeron.app.dev")
        );
        assert_eq!(legacy_domain("other"), None);
    }

    #[test]
    fn migrates_only_into_an_empty_domain() {
        assert!(preferences_empty(false, "", "Domain does not exist"));
        assert!(!preferences_empty(false, "", "permission denied"));
        assert!(preferences_empty(true, "{\n}\n", ""));
        assert!(!preferences_empty(true, "{ key = value; }", ""));
    }
}
