//! The driver's view of one frame: the a11y tree joined with `debug_selector`
//! bounds, snapshot-local ids, locators and the indented text rendering.
//!
//! Pure data in, pure data out: nothing here touches gpui, so the gate for
//! locators and output format runs as ordinary unit tests.

use serde_json::Value;
use sha2::{Digest, Sha256};

/// Bounds in logical pixels: x, y, width, height.
pub type Rect = [f32; 4];

#[derive(Debug, Clone, PartialEq)]
pub struct Node {
    /// Snapshot-local id (`e1`, `e2`, ...), assigned in depth-first order.
    pub id: String,
    pub role: String,
    pub label: Option<String>,
    pub value: Option<String>,
    pub placeholder: Option<String>,
    pub selected: bool,
    pub expanded: Option<bool>,
    pub toggled: Option<String>,
    /// The element id's name, which equals its `debug_selector` when it has one.
    pub selector: Option<String>,
    pub source: Option<String>,
    pub bounds: Option<Rect>,
    pub children: Vec<usize>,
    pub depth: usize,
    pub focused: bool,
}

#[derive(Debug, Clone, Default)]
pub struct Frame {
    pub nodes: Vec<Node>,
    /// Selectors that have bounds but no a11y node.
    pub loose_bounds: Vec<(String, Rect)>,
    pub viewport: (f32, f32),
    pub frame_number: u64,
    pub title: Option<String>,
}

/// `Name("x")` or `NamedInteger("x", 7)` -> `x`.
fn element_name(element_id: &str) -> Option<String> {
    let rest = element_id
        .strip_prefix("Name(\"")
        .or_else(|| element_id.strip_prefix("NamedInteger(\""))?;
    let end = rest.find('"')?;
    Some(rest[..end].to_owned())
}

/// Long text is cut for display only; locators still match the full text.
fn shorten(text: &str) -> String {
    const LIMIT: usize = 100;
    let flat = text.replace('\n', " ⏎ ");
    if flat.chars().count() <= LIMIT {
        return flat;
    }
    let cut: String = flat.chars().take(LIMIT).collect();
    format!("{cut}… ({} chars)", text.chars().count())
}

impl Frame {
    /// Build a frame from `Window::debug_a11y_tree_json` plus
    /// `Window::debug_bounds_all`.
    pub fn parse(json: &str, bounds: &[(String, Rect)]) -> Result<Self, String> {
        let root: Value = serde_json::from_str(json).map_err(|e| e.to_string())?;
        let table = root["nodes"].as_object().ok_or("tree has no nodes")?;
        let root_key = root["root"].as_str().ok_or("tree has no root")?.to_owned();
        let focus = root["gpui_focus"].as_str().map(str::to_owned);
        let mut order: Vec<String> = Vec::new();
        let mut depths: Vec<usize> = Vec::new();
        let mut stack = vec![(root_key, 0usize)];
        let mut seen = std::collections::HashSet::new();
        while let Some((key, depth)) = stack.pop() {
            if !seen.insert(key.clone()) || !table.contains_key(&key) {
                continue;
            }
            order.push(key.clone());
            depths.push(depth);
            let children = table[&key]["children"].as_array().cloned().unwrap_or_default();
            for child in children.iter().rev().filter_map(Value::as_str) {
                stack.push((child.to_owned(), depth + 1));
            }
        }
        // Unreachable nodes still show up, after the walk, in key order.
        for key in table.keys() {
            if !seen.contains(key) {
                order.push(key.clone());
                depths.push(1);
            }
        }
        let index: std::collections::HashMap<&str, usize> = order
            .iter()
            .enumerate()
            .map(|(i, k)| (k.as_str(), i))
            .collect();
        let mut nodes = Vec::with_capacity(order.len());
        let mut used_bounds = std::collections::HashSet::new();
        for (i, key) in order.iter().enumerate() {
            let raw = &table[key];
            let aria = &raw["aria"];
            let selector = raw["element_id"].as_str().and_then(element_name);
            let rect = selector.as_ref().and_then(|name| {
                bounds.iter().find(|(k, _)| k == name).map(|(k, r)| {
                    used_bounds.insert(k.clone());
                    *r
                })
            });
            let text = |name: &str| aria[name].as_str().map(str::to_owned);
            nodes.push(Node {
                id: format!("e{}", i + 1),
                role: aria["role"].as_str().unwrap_or("Unknown").to_owned(),
                label: text("label"),
                value: text("value"),
                placeholder: text("placeholder"),
                selected: aria["selected"].as_bool().unwrap_or(false),
                expanded: aria["expanded"].as_bool(),
                toggled: text("toggled"),
                selector,
                source: raw["source_location"].as_str().map(str::to_owned),
                bounds: rect,
                children: raw["children"]
                    .as_array()
                    .map(|c| {
                        c.iter()
                            .filter_map(Value::as_str)
                            .filter_map(|k| index.get(k).copied())
                            .collect()
                    })
                    .unwrap_or_default(),
                depth: depths[i],
                focused: focus.as_deref() == Some(key.as_str()),
            });
        }
        let loose_bounds = bounds
            .iter()
            .filter(|(k, _)| !used_bounds.contains(k))
            .cloned()
            .collect();
        let size = &root["frame"]["viewport_size"];
        Ok(Self {
            nodes,
            loose_bounds,
            viewport: (
                size["width"].as_f64().unwrap_or(0.0) as f32,
                size["height"].as_f64().unwrap_or(0.0) as f32,
            ),
            frame_number: root["frame"]["frame_number"].as_u64().unwrap_or(0),
            title: root["frame"]["window_title"].as_str().map(str::to_owned),
        })
    }

    /// Hash of the tree's structure and content: ids, roles, names, values,
    /// selectors and bounds. Not the frame number or timestamps.
    pub fn hash(&self) -> String {
        let mut hasher = Sha256::new();
        for node in &self.nodes {
            hasher.update(
                format!(
                    "{}|{}|{:?}|{:?}|{:?}|{}|{:?}|{:?}|{:?}|{:?}|{}\n",
                    node.id,
                    node.role,
                    node.label,
                    node.value,
                    node.placeholder,
                    node.selected,
                    node.expanded,
                    node.toggled,
                    node.selector,
                    node.bounds,
                    node.depth,
                )
                .as_bytes(),
            );
        }
        for (name, rect) in &self.loose_bounds {
            hasher.update(format!("loose|{name}|{rect:?}\n").as_bytes());
        }
        let digest = hasher.finalize();
        digest[..8].iter().map(|b| format!("{b:02x}")).collect()
    }

    /// Text of the nodes, for `text=` matching and transcript assertions.
    pub fn all_text(&self) -> String {
        let mut out = String::new();
        for node in &self.nodes {
            for part in [&node.label, &node.value].into_iter().flatten() {
                out.push_str(part);
                out.push('\n');
            }
        }
        out
    }
}

/// A resolved or candidate element.
#[derive(Debug, Clone, PartialEq)]
pub struct Match {
    pub id: String,
    pub description: String,
    pub bounds: Option<Rect>,
    pub node: Option<usize>,
}

#[derive(Debug, PartialEq)]
pub enum LocateError {
    Syntax(String),
    NotFound(String),
    Ambiguous { locator: String, matches: Vec<String> },
    NoBounds(String),
}

impl std::fmt::Display for LocateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Syntax(m) => write!(f, "bad locator: {m}"),
            Self::NotFound(l) => write!(f, "no element matches {l}"),
            Self::Ambiguous { locator, matches } => {
                write!(f, "ambiguous locator {locator}: {} matches", matches.len())?;
                for m in matches {
                    write!(f, "\n  {m}")?;
                }
                Ok(())
            }
            Self::NoBounds(l) => write!(
                f,
                "{l} has no bounds (the element has no debug_selector); use a #selector"
            ),
        }
    }
}

#[derive(Debug, PartialEq)]
enum Query {
    Selector(String),
    RoleName { role: String, name: String },
    Text { text: String, nth: Option<usize> },
    Id(String),
}

fn parse_quoted(rest: &str) -> Result<(&str, &str), String> {
    let body = rest.strip_prefix('"').ok_or("expected a quoted string")?;
    let end = body.find('"').ok_or("unterminated string")?;
    Ok((&body[..end], &body[end + 1..]))
}

fn parse(locator: &str) -> Result<Query, String> {
    let locator = locator.trim();
    if let Some(id) = locator.strip_prefix('#') {
        return if id.is_empty() {
            Err("empty #selector".into())
        } else {
            Ok(Query::Selector(id.to_owned()))
        };
    }
    if let Some(rest) = locator.strip_prefix("text=") {
        let (text, tail) = parse_quoted(rest)?;
        let nth = if tail.is_empty() {
            None
        } else {
            let n = tail
                .strip_prefix(":nth(")
                .and_then(|t| t.strip_suffix(')'))
                .and_then(|t| t.parse::<usize>().ok())
                .ok_or("expected :nth(n) after the text")?;
            Some(n)
        };
        return Ok(Query::Text {
            text: text.to_owned(),
            nth,
        });
    }
    if let Some(open) = locator.find("[name=") {
        let role = &locator[..open];
        let (name, tail) = parse_quoted(&locator[open + "[name=".len()..])?;
        if tail != "]" || role.is_empty() {
            return Err("expected role[name=\"...\"]".into());
        }
        return Ok(Query::RoleName {
            role: role.to_owned(),
            name: name.to_owned(),
        });
    }
    if locator.len() > 1
        && locator.starts_with('e')
        && locator[1..].chars().all(|c| c.is_ascii_digit())
    {
        return Ok(Query::Id(locator.to_owned()));
    }
    Err("use #selector, role[name=\"..\"], text=\"..\"[:nth(n)] or eN".into())
}

fn role_matches(wanted: &str, actual: &str) -> bool {
    let (wanted, actual) = (wanted.to_ascii_lowercase(), actual.to_ascii_lowercase());
    wanted == actual
        || (wanted == "textbox" && actual.contains("textinput"))
        || (wanted == "textbox" && actual == "searchinput")
}

impl Frame {
    pub fn describe(&self, index: usize) -> String {
        let node = &self.nodes[index];
        let mut s = format!("{} {}", node.id, node.role.to_ascii_lowercase());
        if let Some(label) = &node.label {
            s.push_str(&format!(" {label:?}"));
        }
        if let Some(selector) = &node.selector {
            s.push_str(&format!(" sel={selector}"));
        }
        s
    }

    fn node_match(&self, index: usize) -> Match {
        Match {
            id: self.nodes[index].id.clone(),
            description: self.describe(index),
            bounds: self.nodes[index].bounds,
            node: Some(index),
        }
    }

    /// Every element the locator matches, in tree order.
    pub fn find(&self, locator: &str) -> Result<Vec<Match>, LocateError> {
        let query = parse(locator).map_err(LocateError::Syntax)?;
        let mut found: Vec<Match> = match &query {
            Query::Selector(name) => {
                let mut v: Vec<Match> = self
                    .nodes
                    .iter()
                    .enumerate()
                    .filter(|(_, n)| n.selector.as_deref() == Some(name))
                    .map(|(i, _)| self.node_match(i))
                    .collect();
                if v.is_empty()
                    && let Some((_, rect)) = self.loose_bounds.iter().find(|(k, _)| k == name)
                {
                    v.push(Match {
                        id: format!("#{name}"),
                        description: format!("#{name} (no a11y node)"),
                        bounds: Some(*rect),
                        node: None,
                    });
                }
                v
            }
            Query::RoleName { role, name } => self
                .nodes
                .iter()
                .enumerate()
                .filter(|(_, n)| role_matches(role, &n.role) && n.label.as_deref() == Some(name))
                .map(|(i, _)| self.node_match(i))
                .collect(),
            Query::Text { text, .. } => self
                .nodes
                .iter()
                .enumerate()
                .filter(|(_, n)| {
                    [&n.label, &n.value]
                        .into_iter()
                        .flatten()
                        .any(|t| t.contains(text.as_str()))
                })
                .map(|(i, _)| self.node_match(i))
                .collect(),
            Query::Id(id) => self
                .nodes
                .iter()
                .position(|n| &n.id == id)
                .map(|i| vec![self.node_match(i)])
                .unwrap_or_default(),
        };
        if let Query::Text { nth: Some(n), .. } = &query {
            found = found.into_iter().nth(*n).into_iter().collect();
        }
        Ok(found)
    }

    /// Exactly one element, or an error naming every candidate.
    pub fn resolve(&self, locator: &str) -> Result<Match, LocateError> {
        let mut found = self.find(locator)?;
        match found.len() {
            0 => Err(LocateError::NotFound(locator.to_owned())),
            1 => Ok(found.remove(0)),
            _ => Err(LocateError::Ambiguous {
                locator: locator.to_owned(),
                matches: found.into_iter().map(|m| m.description).collect(),
            }),
        }
    }

    /// Resolve and require bounds (for pointer input).
    pub fn resolve_with_bounds(&self, locator: &str) -> Result<(Match, Rect), LocateError> {
        let found = self.resolve(locator)?;
        let rect = found
            .bounds
            .ok_or_else(|| LocateError::NoBounds(locator.to_owned()))?;
        Ok((found, rect))
    }

    /// The indented tree, box-drawing prefixes, one node per line.
    pub fn render(&self) -> String {
        let mut out = String::new();
        if self.nodes.is_empty() {
            return out;
        }
        self.render_node(0, &mut Vec::new(), &mut out);
        // Nodes the walk never reached are listed flat so nothing hides.
        let reached = self.reachable();
        for (i, _) in self.nodes.iter().enumerate().filter(|(i, _)| !reached[*i]) {
            out.push_str(&format!("? {}\n", self.line(i)));
        }
        out
    }

    /// Regions that have a selector (so `#name` clicks work) but no a11y node.
    pub fn render_selectors(&self) -> String {
        let mut out = String::new();
        for (name, [x, y, w, h]) in &self.loose_bounds {
            out.push_str(&format!("# sel={name} [{x:.0},{y:.0} {w:.0}x{h:.0}]\n"));
        }
        out
    }

    fn reachable(&self) -> Vec<bool> {
        let mut seen = vec![false; self.nodes.len()];
        let mut stack = vec![0usize];
        while let Some(i) = stack.pop() {
            if std::mem::replace(&mut seen[i], true) {
                continue;
            }
            stack.extend(self.nodes[i].children.iter().copied());
        }
        seen
    }

    fn line(&self, index: usize) -> String {
        let node = &self.nodes[index];
        let role = node.role.to_ascii_lowercase();
        let mut s = format!("{} {}", node.id, role);
        if let Some(label) = node.label.as_ref().filter(|l| !l.is_empty()) {
            s.push_str(&format!(" {:?}", shorten(label)));
        }
        if let Some(value) = node.value.as_ref().filter(|v| !v.is_empty()) {
            s.push_str(&format!(" value={:?}", shorten(value)));
        }
        if let Some(selector) = &node.selector {
            s.push_str(&format!(" sel={selector}"));
        }
        if let Some([x, y, w, h]) = node.bounds {
            s.push_str(&format!(" [{x:.0},{y:.0} {w:.0}x{h:.0}]"));
        }
        if node.selected {
            s.push_str(" selected");
        }
        if node.focused {
            s.push_str(" focused");
        }
        if let Some(expanded) = node.expanded {
            s.push_str(if expanded { " expanded" } else { " collapsed" });
        }
        if let Some(toggled) = &node.toggled {
            s.push_str(&format!(" toggled={}", toggled.to_ascii_lowercase()));
        }
        s
    }

    fn render_node(&self, index: usize, ancestors: &mut Vec<bool>, out: &mut String) {
        let node = &self.nodes[index];
        if index == 0 {
            let title = self.title.as_deref().unwrap_or("");
            out.push_str(&format!("{} {title:?}\n", node.role.to_ascii_lowercase()));
        } else {
            let mut prefix = String::new();
            for last in &ancestors[..ancestors.len() - 1] {
                prefix.push_str(if *last { "  " } else { "│ " });
            }
            let last = *ancestors.last().unwrap_or(&true);
            prefix.push_str(if last { "└ " } else { "├ " });
            out.push_str(&format!("{prefix}{}\n", self.line(index)));
            if let Some(source) = &node.source {
                let mut cont = String::new();
                for last in ancestors.iter() {
                    cont.push_str(if *last { "  " } else { "│ " });
                }
                out.push_str(&format!("{cont}  src: {source}\n"));
            }
        }
        let count = node.children.len();
        for (n, child) in node.children.iter().enumerate() {
            ancestors.push(n + 1 == count);
            if ancestors.len() > 64 {
                ancestors.pop();
                continue;
            }
            self.render_node(*child, ancestors, out);
            ancestors.pop();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TREE: &str = r#"{"root":"z","gpui_focus":"b","frame":{"frame_number":3,"window_title":"Mast","viewport_size":{"width":1200.0,"height":800.0}},
      "nodes":{
       "a":{"element_id":"Name(\"send\")","source_location":"x.rs:1:1","aria":{"role":"Button","label":"Send"}},
       "b":{"element_id":"NamedInteger(\"composer-input\", 5)","aria":{"role":"MultilineTextInput","label":"Reply…","value":"hello"}},
       "c":{"element_id":"Name(\"chat-row-1\")","aria":{"role":"Button","label":"Review","selected":true}},
       "d":{"element_id":"Name(\"chat-row-2\")","aria":{"role":"Button","label":"Review more"}},
       "z":{"children":["a","b","c","d"],"aria":{"role":"Window"}}}}"#;

    fn frame() -> Frame {
        let r = |x| [x, 0.0, 10.0, 10.0];
        Frame::parse(
            TREE,
            &[
                ("send".into(), r(1.0)),
                ("chat-row-1".into(), r(2.0)),
                ("loose".into(), r(3.0)),
            ],
        )
        .unwrap()
    }

    #[test]
    fn locators() {
        let f = frame();
        assert_eq!(f.resolve("#send").unwrap().id, "e2");
        assert_eq!(f.resolve("button[name=\"Send\"]").unwrap().id, "e2");
        assert_eq!(f.resolve("textbox[name=\"Reply…\"]").unwrap().id, "e3");
        assert_eq!(f.resolve("e4").unwrap().id, "e4");
        assert_eq!(f.resolve("#loose").unwrap().node, None);
        assert_eq!(f.resolve("text=\"Review more\"").unwrap().id, "e5");
        assert_eq!(f.resolve("text=\"Review\":nth(0)").unwrap().id, "e4");
        assert_eq!(f.resolve("text=\"Review\":nth(1)").unwrap().id, "e5");
    }

    #[test]
    fn ambiguous_lists_matches_and_never_picks() {
        let f = frame();
        let err = f.resolve("text=\"Review\"").unwrap_err();
        let LocateError::Ambiguous { matches, .. } = &err else {
            panic!("{err:?}")
        };
        assert_eq!(matches.len(), 2);
        let text = err.to_string();
        assert!(text.contains("e4") && text.contains("e5"), "{text}");
        assert!(matches!(f.resolve("#nope"), Err(LocateError::NotFound(_))));
        assert!(matches!(
            f.resolve_with_bounds("e5"),
            Err(LocateError::NoBounds(_))
        ));
        assert!(matches!(f.resolve("bogus"), Err(LocateError::Syntax(_))));
    }

    #[test]
    fn render_and_hash_are_stable() {
        let f = frame();
        let text = f.render();
        assert!(text.starts_with("window \"Mast\"\n"), "{text}");
        assert!(text.contains("├ e2 button \"Send\" sel=send [1,0 10x10]"), "{text}");
        assert!(text.contains("src: x.rs:1:1"), "{text}");
        assert!(text.contains("value=\"hello\" sel=composer-input"), "{text}");
        assert!(text.contains("focused"), "{text}");
        assert!(text.contains("selected"), "{text}");
        assert_eq!(f.hash(), frame().hash());
    }
}
