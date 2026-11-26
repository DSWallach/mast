use gpui::{AppContext, Context, Render, Window, div, prelude::*, px, size};
use image::{RgbaImage, imageops};
use mast_ui::{
    theme::{Appearance, Theme},
    ui_style::{UiLayout, UiStyleId},
};
use std::{
    collections::BTreeMap,
    env,
    path::{Path, PathBuf},
    process::Command,
};

pub const FOOTER_HEIGHT: u32 = 30;

pub struct Provenance {
    pub revision: String,
    pub branch: String,
    pub dirty: bool,
    pub host: String,
    pub command: String,
    pub env: BTreeMap<String, String>,
}

fn checkout_output(root: &Path, program: &str, args: &[&str]) -> Option<String> {
    let output = Command::new(program)
        .current_dir(root)
        .args(args)
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

fn parse_revision(stamp: &str) -> anyhow::Result<(&str, bool)> {
    let revision = stamp.strip_suffix("-dirty").unwrap_or(stamp);
    anyhow::ensure!(
        revision.len() == 40 && revision.bytes().all(|byte| byte.is_ascii_hexdigit()),
        "preview revision is not a Git SHA",
    );
    Ok((revision, revision != stamp))
}

#[test]
fn preview_provenance_ignores_stale_env() -> anyhow::Result<()> {
    let repo = tempfile::tempdir()?;
    let root = repo.path();
    let git = |args: &[&str]| -> anyhow::Result<String> {
        let output = Command::new("git")
            .current_dir(root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .args([
                "-c",
                "user.name=Preview Fixture",
                "-c",
                "user.email=preview@example.invalid",
            ])
            .args(args)
            .output()?;
        anyhow::ensure!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr),
        );
        Ok(String::from_utf8(output.stdout)?.trim().into())
    };
    git(&["init", "-q", "--template=", "--initial-branch=preview-fixture"])?;
    let scripts = root.join("scripts");
    std::fs::create_dir(&scripts)?;
    std::fs::write(
        scripts.join("build-revision.sh"),
        include_str!("../../../../scripts/build-revision.sh"),
    )?;
    std::fs::write(root.join("tracked.txt"), "fixture")?;
    git(&["add", "scripts/build-revision.sh", "tracked.txt"])?;
    git(&["commit", "-qm", "Preview fixture"])?;
    let expected = git(&["log", "-1", "--format=%H"])?;
    let output = Command::new(env::current_exe()?)
        .args([
            "--ignored",
            "--exact",
            "provenance::preview_provenance_fixture_child",
            "--nocapture",
        ])
        .env("MAST_TEST_PREVIEW_FIXTURE", root)
        .env("MAST_TEST_PREVIEW_EXPECTED_SHA", expected)
        .env("MAST_UI_REVISION", "0000000000000000000000000000000000000000")
        .env("MAST_UI_BRANCH", "stale-branch")
        .output()?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    anyhow::ensure!(
        output.status.success() && stdout.contains("1 passed"),
        "provenance child failed: {stdout}\n{}",
        String::from_utf8_lossy(&output.stderr),
    );
    Ok(())
}

#[test]
#[ignore = "started by preview_provenance_ignores_stale_env with a temporary Git fixture"]
fn preview_provenance_fixture_child() -> anyhow::Result<()> {
    let root = PathBuf::from(env::var_os("MAST_TEST_PREVIEW_FIXTURE").unwrap());
    let expected = env::var("MAST_TEST_PREVIEW_EXPECTED_SHA")?;
    let clean = Provenance::read_from(&root)?;
    assert_eq!(clean.revision, expected);
    assert_eq!(clean.branch, "preview-fixture");
    assert!(!clean.dirty);
    std::fs::write(root.join("tracked.txt"), "changed")?;
    let dirty = Provenance::read_from(&root)?;
    assert_eq!(dirty.revision, expected);
    assert_eq!(dirty.branch, "preview-fixture");
    assert!(dirty.dirty);
    assert!(parse_revision("unknown").is_err());
    Ok(())
}

impl Provenance {
    pub fn read() -> anyhow::Result<Self> {
        let root = Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../.."));
        Self::read_from(root)
    }

    fn read_from(root: &Path) -> anyhow::Result<Self> {
        let stamped_revision = checkout_output(root, "sh", &["scripts/build-revision.sh"])
            .ok_or_else(|| anyhow::anyhow!("could not read preview revision"))?;
        let (revision, dirty) = parse_revision(&stamped_revision)?;
        let branch = checkout_output(root, "git", &["symbolic-ref", "--quiet", "--short", "HEAD"])
            .unwrap_or_else(|| "detached".into());
        let vars = env::vars_os()
            .filter_map(|(key, value)| {
                let key = key.into_string().ok()?;
                if !key.starts_with("MAST_") {
                    return None;
                }
                // Other Mast settings may contain live credentials or private state.
                let value = if key.starts_with("MAST_PREVIEW_") {
                    value.to_string_lossy().into_owned()
                } else {
                    "[redacted]".into()
                };
                Some((key, value))
            })
            .collect();
        Ok(Self {
            revision: revision.into(),
            branch,
            dirty,
            host: checkout_output(root, "hostname", &[]).unwrap_or_else(|| "unknown".into()),
            command: env::var("MAST_UI_COMMAND")
                .unwrap_or_else(|_| format!("{:?}", env::args_os().collect::<Vec<_>>())),
            env: vars,
        })
    }

    pub fn output_dir(&self, target: &Path) -> anyhow::Result<PathBuf> {
        let branch: String = self
            .branch
            .chars()
            .take(80)
            .map(|c| {
                if c.is_ascii_alphanumeric() || matches!(c, '-' | '_') {
                    c
                } else {
                    '-'
                }
            })
            .collect();
        let parent = target.join("ui-preview");
        std::fs::create_dir_all(&parent)?;
        let name = format!(
            "{branch}-{}-{}-",
            self.short_revision(),
            chrono::Local::now().format("%Y%m%d-%H%M%S-%3f")
        );
        Ok(tempfile::Builder::new()
            .prefix(&name)
            .tempdir_in(parent)?
            .keep())
    }

    fn short_revision(&self) -> &str {
        &self.revision[..7]
    }

    pub fn stamp(&self, sheet: RgbaImage) -> anyhow::Result<RgbaImage> {
        let (width, height) = sheet.dimensions();
        let branch = self.branch.clone();
        let revision = format!(
            "@ {}{}",
            self.short_revision(),
            if self.dirty { "-dirty" } else { "" }
        );
        let path = super::snapshot(
            &format!("preview-footer-{}", std::process::id()),
            size(px(width as f32), px(FOOTER_HEIGHT as f32)),
            UiStyleId::Default,
            UiLayout::Minimal,
            Appearance::Light,
            |_, cx| cx.new(|_| Footer { branch, revision }),
        )?;
        let footer = image::open(&path);
        let _ = std::fs::remove_file(&path);
        let footer = footer?.into_rgba8();
        let footer = imageops::resize(
            &footer,
            width,
            FOOTER_HEIGHT,
            imageops::FilterType::Lanczos3,
        );
        let mut stamped = RgbaImage::new(width, height + FOOTER_HEIGHT);
        imageops::replace(&mut stamped, &sheet, 0, 0);
        imageops::replace(&mut stamped, &footer, 0, height.into());
        Ok(stamped)
    }
}

struct Footer {
    branch: String,
    revision: String,
}

impl Render for Footer {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::of(cx);
        div()
            .size_full()
            .flex()
            .items_center()
            .gap(px(6.))
            .px(px(10.))
            .bg(theme.surface_raised)
            .text_color(theme.text)
            .text_size(px(13.))
            .child(div().min_w_0().text_ellipsis().child(self.branch.clone()))
            .child(div().flex_none().child(self.revision.clone()))
    }
}
