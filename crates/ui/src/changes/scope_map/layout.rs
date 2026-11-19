//! Deterministic squarified rectangle layout, independent of the UI.

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(super) struct Rect {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

#[derive(Clone, Debug)]
pub(super) struct Entry {
    pub id: usize,
    pub weight: u64,
}

#[derive(Clone, Debug, Default)]
pub(super) struct Folder {
    pub path: String,
    pub files: Vec<Entry>,
    pub folders: Vec<Folder>,
}

#[derive(Clone, Debug, PartialEq)]
pub(super) enum TileKind {
    File(usize),
    More(Vec<usize>),
    Folder(String),
}

#[derive(Clone, Debug, PartialEq)]
pub(super) struct Tile {
    pub kind: TileKind,
    pub bounds: Rect,
}

const MIN_TILE: f32 = 16.0;

impl Folder {
    fn weight(&self) -> u64 {
        self.files.iter().map(|f| f.weight.max(1)).sum::<u64>()
            + self.folders.iter().map(Folder::weight).sum::<u64>()
    }
    fn count(&self) -> usize {
        self.files.len() + self.folders.iter().map(Folder::count).sum::<usize>()
    }
}

/// Group the smallest files in each folder once the diff exceeds the limit.
/// Folders remain separate even when there are more folders than the limit.
pub(super) fn layout(root: &Folder, bounds: Rect) -> Vec<Tile> {
    let count = root.count();
    let mut result = Vec::new();
    if count <= 3 {
        fn collect(folder: &Folder, out: &mut Vec<Entry>) {
            out.extend(folder.files.iter().cloned());
            for child in &folder.folders {
                collect(child, out);
            }
        }
        let mut files = Vec::new();
        collect(root, &mut files);
        if !files.is_empty() {
            let minimum = MIN_TILE.min(bounds.width / files.len() as f32);
            let mut widths = vec![0.0; files.len()];
            let mut remaining = bounds.width;
            let mut pending: Vec<usize> = (0..files.len()).collect();
            loop {
                let total: u64 = pending
                    .iter()
                    .map(|&index| files[index].weight.max(1))
                    .sum();
                if pending.is_empty() {
                    break;
                }
                let small: Vec<_> = pending
                    .iter()
                    .copied()
                    .filter(|&index| {
                        remaining * files[index].weight.max(1) as f32 / (total as f32) < minimum
                    })
                    .collect();
                if small.is_empty() {
                    for index in pending {
                        widths[index] =
                            remaining * files[index].weight.max(1) as f32 / total as f32;
                    }
                    break;
                }
                for index in small {
                    widths[index] = minimum;
                    remaining -= minimum;
                    pending.retain(|item| *item != index);
                }
            }
            let mut x = bounds.x;
            for (file, width) in files.iter().zip(widths) {
                result.push(Tile {
                    kind: TileKind::File(file.id),
                    bounds: Rect { x, width, ..bounds },
                });
                x += width;
            }
        }
        return result;
    }
    fn visit(folder: &Folder, bounds: Rect, grouped: bool, total: usize, result: &mut Vec<Tile>) {
        enum Item<'a> {
            File(&'a Entry),
            More(Vec<&'a Entry>),
            Folder(&'a Folder),
        }
        let mut files: Vec<_> = folder.files.iter().collect();
        files.sort_by(|a, b| b.weight.cmp(&a.weight).then(a.id.cmp(&b.id)));
        let keep = if grouped {
            (299 * folder.files.len() / total).max(1).min(files.len())
        } else {
            files.len()
        };
        let mut visible = files[..keep].to_vec();
        let mut more = files[keep..].to_vec();
        let (items, rects) = loop {
            let mut items: Vec<_> = visible.iter().copied().map(Item::File).collect();
            if !more.is_empty() {
                items.push(Item::More(more.clone()));
            }
            items.extend(folder.folders.iter().map(Item::Folder));
            let weights: Vec<_> = items
                .iter()
                .map(|item| match item {
                    Item::File(file) => file.weight.max(1),
                    Item::More(files) => files.iter().map(|f| f.weight.max(1)).sum(),
                    Item::Folder(child) => child.weight(),
                })
                .collect();
            let rects = squarify(&weights, bounds);
            let too_small: Vec<_> = rects[..visible.len()]
                .iter()
                .map(|rect| rect.width < MIN_TILE || rect.height < MIN_TILE)
                .collect();
            if too_small.iter().any(|small| *small) {
                let mut index = 0;
                visible.retain(|file| {
                    let small = too_small[index];
                    index += 1;
                    if small {
                        more.push(*file);
                    }
                    !small
                });
            } else if !more.is_empty()
                && !visible.is_empty()
                && rects
                    .get(visible.len())
                    .is_some_and(|rect| rect.width < MIN_TILE || rect.height < MIN_TILE)
            {
                more.push(visible.pop().unwrap());
            } else {
                break (items, rects);
            }
        };
        for (item, rect) in items.into_iter().zip(rects) {
            match item {
                Item::File(file) => result.push(Tile {
                    kind: TileKind::File(file.id),
                    bounds: rect,
                }),
                Item::More(files) => result.push(Tile {
                    kind: TileKind::More(files.iter().map(|f| f.id).collect()),
                    bounds: rect,
                }),
                Item::Folder(child) => {
                    result.push(Tile {
                        kind: TileKind::Folder(child.path.clone()),
                        bounds: rect,
                    });
                    let header = if rect.height >= 40.0 && rect.width >= 80.0 {
                        18.0
                    } else {
                        0.0
                    };
                    visit(
                        child,
                        Rect {
                            y: rect.y + header,
                            height: (rect.height - header).max(0.0),
                            ..rect
                        },
                        grouped,
                        total,
                        result,
                    );
                }
            }
        }
    }
    visit(root, bounds, count > 300, count, &mut result);
    result
}

/// Lay out nonnegative weights in input order. Zero weights receive no area.
pub(super) fn squarify(weights: &[u64], bounds: Rect) -> Vec<Rect> {
    let mut output = vec![Rect::default(); weights.len()];
    let total: u64 = weights.iter().sum();
    if total == 0 || bounds.width <= 0.0 || bounds.height <= 0.0 {
        return output;
    }
    let scale = f64::from(bounds.width) * f64::from(bounds.height) / total as f64;
    let mut items: Vec<(usize, f64)> = weights
        .iter()
        .enumerate()
        .filter(|(_, weight)| **weight > 0)
        .map(|(index, weight)| (index, *weight as f64 * scale))
        .collect();
    items.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
    let (mut x, mut y, mut w, mut h) = (
        bounds.x as f64,
        bounds.y as f64,
        bounds.width as f64,
        bounds.height as f64,
    );
    let mut start = 0;
    while start < items.len() && w > 0.0 && h > 0.0 {
        let side = w.min(h);
        let mut end = start + 1;
        let mut sum = items[start].1;
        let mut min = sum;
        let mut max = sum;
        let worst = |sum: f64, min: f64, max: f64| {
            (side * side * max / (sum * sum)).max(sum * sum / (side * side * min))
        };
        while end < items.len() {
            let area = items[end].1;
            if worst(sum + area, min.min(area), max.max(area)) > worst(sum, min, max) {
                break;
            }
            sum += area;
            min = min.min(area);
            max = max.max(area);
            end += 1;
        }
        let horizontal = w >= h;
        let thickness = (sum / side).min(if horizontal { w } else { h });
        let mut offset = 0.0;
        for &(index, area) in &items[start..end] {
            let length = area / thickness;
            output[index] = if horizontal {
                Rect {
                    x: x as f32,
                    y: (y + offset) as f32,
                    width: thickness as f32,
                    height: length as f32,
                }
            } else {
                Rect {
                    x: (x + offset) as f32,
                    y: y as f32,
                    width: length as f32,
                    height: thickness as f32,
                }
            };
            offset += length;
        }
        if horizontal {
            x += thickness;
            w -= thickness;
        } else {
            y += thickness;
            h -= thickness;
        }
        start = end;
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn conserves_area_and_does_not_overlap() {
        let bounds = Rect {
            x: 0.0,
            y: 0.0,
            width: 600.0,
            height: 400.0,
        };
        let weights = [10, 9, 8, 4, 3, 2, 1];
        let rects = squarify(&weights, bounds);
        let total: f32 = rects.iter().map(|r| r.width * r.height).sum();
        assert!((total - bounds.width * bounds.height).abs() < 0.1);
        for (i, a) in rects.iter().enumerate() {
            assert!(a.width > 0.0 && a.height > 0.0);
            for b in &rects[i + 1..] {
                let overlap_w = (a.x + a.width).min(b.x + b.width) - a.x.max(b.x);
                let overlap_h = (a.y + a.height).min(b.y + b.height) - a.y.max(b.y);
                assert!(overlap_w < 0.001 || overlap_h < 0.001);
            }
        }
        assert_eq!(rects, squarify(&weights, bounds));
    }

    #[test]
    fn empty_or_zero_bounds_are_empty() {
        assert_eq!(
            squarify(
                &[0, 0],
                Rect {
                    width: 10.0,
                    height: 10.0,
                    ..Default::default()
                }
            ),
            vec![Rect::default(); 2]
        );
        assert_eq!(squarify(&[1], Rect::default()), vec![Rect::default()]);
    }

    #[test]
    fn small_diff_is_a_row() {
        let folder = Folder {
            files: vec![Entry { id: 0, weight: 1 }, Entry { id: 1, weight: 100 }],
            ..Default::default()
        };
        let tiles = layout(
            &folder,
            Rect {
                width: 200.0,
                height: 50.0,
                ..Default::default()
            },
        );
        assert_eq!(tiles.len(), 2);
        assert_eq!(tiles[0].bounds.width, MIN_TILE);
        assert_eq!(tiles[1].bounds.x, tiles[0].bounds.width);
    }

    #[test]
    fn large_diff_groups_small_files_by_folder() {
        let folder = Folder {
            folders: vec![Folder {
                path: "src".into(),
                files: (0..1000)
                    .map(|id| Entry {
                        id,
                        weight: (id + 1) as u64,
                    })
                    .collect(),
                ..Default::default()
            }],
            ..Default::default()
        };
        let tiles = layout(
            &folder,
            Rect {
                width: 1000.0,
                height: 1000.0,
                ..Default::default()
            },
        );
        let more = tiles
            .iter()
            .find_map(|tile| match &tile.kind {
                TileKind::More(ids) => Some(ids),
                _ => None,
            })
            .unwrap();
        assert!(more.len() >= 701);
        assert!(more.contains(&0));
        assert!(
            tiles
                .iter()
                .all(|tile| tile.bounds.width > 0.0 && tile.bounds.height > 0.0)
        );
    }

    #[test]
    fn small_files_are_grouped_into_usable_tiles() {
        let folder = Folder {
            files: (0..80)
                .map(|id| Entry {
                    id,
                    weight: if id == 0 { 1000 } else { 1 },
                })
                .collect(),
            ..Default::default()
        };
        let tiles = layout(
            &folder,
            Rect {
                width: 400.0,
                height: 300.0,
                ..Default::default()
            },
        );
        assert!(
            tiles
                .iter()
                .any(|tile| matches!(tile.kind, TileKind::More(_)))
        );
        assert!(
            tiles.iter().all(|tile| tile.bounds.width >= MIN_TILE - 0.01
                && tile.bounds.height >= MIN_TILE - 0.01)
        );
    }
}
