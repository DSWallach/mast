#[path = "ui_preview/matrix.rs"]
mod matrix;
#[cfg(all(target_os = "macos", feature = "snapshot"))]
#[path = "ui_preview/render.rs"]
mod render;

#[cfg(all(target_os = "macos", feature = "snapshot"))]
#[test]
#[ignore = "requires macOS Metal and access to macOS services"]
fn render_ui_preview() -> anyhow::Result<()> {
    use image::{RgbaImage, imageops};
    use mast_ui::{theme::Appearance, ui_style::UiStyleId};
    use std::path::PathBuf;

    fn selection(variable: &str, defaults: &[&str]) -> Vec<String> {
        std::env::var(variable)
            .ok()
            .map(|value| {
                value
                    .split(',')
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_else(|| defaults.iter().map(|value| (*value).to_owned()).collect())
    }
    let screens = selection(
        "MAST_PREVIEW_SCREENS",
        &["chat", "details", "walkthrough", "appearance", "controls"],
    );
    let all_screens = ["chat", "details", "walkthrough", "appearance", "controls"];
    anyhow::ensure!(
        !screens.is_empty() && screens.iter().all(|s| all_screens.contains(&s.as_str())),
        "invalid or empty screen selection"
    );
    let style_names: Vec<String> = UiStyleId::ALL
        .iter()
        .map(|s| s.label().to_ascii_lowercase())
        .collect();
    let defaults: Vec<&str> = style_names.iter().map(String::as_str).collect();
    let styles = selection("MAST_PREVIEW_STYLES", &defaults);
    anyhow::ensure!(
        !styles.is_empty() && styles.iter().all(|s| style_names.contains(s)),
        "invalid or empty style selection"
    );
    let mut themes = vec!["mast-light".to_string(), "mast-dark".to_string()];
    for theme in selection("MAST_PREVIEW_THEMES", &[]) {
        if !themes.contains(&theme) {
            themes.push(theme);
        }
    }
    let registry = mast_theme::ThemeRegistry::builtin();
    for theme in &themes {
        anyhow::ensure!(registry.variant(theme).is_some(), "unknown theme: {theme}");
    }
    let target = std::env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target"));
    let directory = target
        .join("ui-preview")
        .join(chrono::Local::now().format("%Y%m%d-%H%M%S-%3f").to_string());
    std::fs::create_dir_all(&directory)?;
    let data_dir = tempfile::tempdir()?;
    let runtime = tokio::runtime::Runtime::new()?;
    let _guard = runtime.enter();
    let screen_refs: Vec<&str> = screens.iter().map(String::as_str).collect();
    let style_refs: Vec<&str> = styles.iter().map(String::as_str).collect();
    let theme_refs: Vec<&str> = themes.iter().map(String::as_str).collect();
    let cells = matrix::matrix(&screen_refs, &style_refs, &theme_refs);
    let (cell_width, cell_height) =
        matrix::cell_size(1440, 900, themes.len() as u32, styles.len() as u32, 4096)
            .ok_or_else(|| anyhow::anyhow!("too many styles or themes for a contact sheet"))?;
    let mut sheets = Vec::new();
    for (screen_index, screen) in screens.iter().enumerate() {
        let mut sheet = RgbaImage::new(
            cell_width * themes.len() as u32,
            cell_height * styles.len() as u32,
        );
        for (index, cell) in cells
            .iter()
            .filter(|cell| cell.screen == screen)
            .enumerate()
        {
            let style = UiStyleId::ALL[style_names.iter().position(|s| s == cell.style).unwrap()];
            let variant = registry.variant(cell.theme).unwrap();
            let appearance = match variant.appearance {
                mast_theme::Appearance::Light => Appearance::Light,
                mast_theme::Appearance::Dark => Appearance::Dark,
            };
            let path = render::render(
                cell.screen,
                style,
                cell.theme,
                appearance,
                cell.label.clone(),
                data_dir.path().to_owned(),
                &format!("preview-{screen_index}-{index}"),
            )?;
            let image = image::open(path)?.into_rgba8();
            let image = imageops::resize(
                &image,
                cell_width,
                cell_height,
                imageops::FilterType::Lanczos3,
            );
            imageops::replace(
                &mut sheet,
                &image,
                (index % themes.len()) as i64 * cell_width as i64,
                (index / themes.len()) as i64 * cell_height as i64,
            );
        }
        let path = directory.join(format!("{screen}.png"));
        sheet.save(&path)?;
        println!("{}", path.display());
        sheets.push(sheet);
    }
    let columns = 2_u32.min(sheets.len() as u32);
    let rows = (sheets.len() as u32).div_ceil(columns);
    let max_width = sheets.iter().map(|s| s.width()).max().unwrap();
    let max_height = sheets.iter().map(|s| s.height()).max().unwrap();
    let (width, height) = matrix::cell_size(max_width, max_height, columns, rows, 4096)
        .ok_or_else(|| anyhow::anyhow!("index too large"))?;
    let mut index = RgbaImage::new(width * columns, height * rows);
    for (position, sheet) in sheets.iter().enumerate() {
        let thumbnail = imageops::resize(sheet, width, height, imageops::FilterType::Lanczos3);
        imageops::replace(
            &mut index,
            &thumbnail,
            (position as u32 % columns * width) as i64,
            (position as u32 / columns * height) as i64,
        );
    }
    let path = directory.join("index.png");
    index.save(&path)?;
    println!("{}", path.display());
    Ok(())
}
