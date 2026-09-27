//! A small parser for iRacing's session-info YAML subset, plus extraction of [`SessionInfo`].

use crate::sim::{SessionInfo, Sim};
use std::collections::BTreeMap;

#[derive(Clone, Debug, PartialEq)]
pub enum Node {
    Scalar(String),
    Map(Vec<(String, Node)>),
    List(Vec<Node>),
}

impl Node {
    pub fn get(&self, key: &str) -> Option<&Node> {
        match self {
            Node::Map(entries) => entries.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    pub fn path(&self, keys: &[&str]) -> Option<&Node> {
        keys.iter().try_fold(self, |node, key| node.get(key))
    }

    pub fn str(&self) -> Option<&str> {
        match self {
            Node::Scalar(s) => Some(s),
            _ => None,
        }
    }

    pub fn list(&self) -> &[Node] {
        match self {
            Node::List(items) => items,
            _ => &[],
        }
    }
}

struct Line {
    /// Column of the `-` if this line starts a list item.
    dash_indent: Option<usize>,
    /// Column where the key starts.
    key_indent: usize,
    key: String,
    value: String,
}

fn unquote(v: &str) -> String {
    let v = v.trim();
    for q in ['"', '\''] {
        if v.len() >= 2 && v.starts_with(q) && v.ends_with(q) {
            return v[1..v.len() - 1].to_string();
        }
    }
    v.to_string()
}

fn tokenize(src: &str) -> Vec<Line> {
    src.lines()
        .filter_map(|raw| {
            let line = raw.trim_end();
            let indent = line.len() - line.trim_start().len();
            let mut rest = line.trim_start();
            if rest.is_empty() || rest == "---" || rest == "..." || rest.starts_with('#') {
                return None;
            }
            let mut dash_indent = None;
            let mut key_indent = indent;
            if let Some(after) = rest.strip_prefix("- ") {
                let inner = after.trim_start();
                dash_indent = Some(indent);
                key_indent = indent + 2 + (after.len() - inner.len());
                rest = inner;
            }
            let (key, value) = rest.split_once(':')?;
            Some(Line { dash_indent, key_indent, key: key.trim().to_string(), value: unquote(value) })
        })
        .collect()
}

/// Caps recursion depth so a pathologically indented input (this comes from another
/// process's shared memory) can't overflow the stack. iRacing's real session info
/// nests at most ~5 levels deep, so this is far above anything legitimate.
const MAX_DEPTH: usize = 32;

/// Skip every line that belongs to the (uncaptured) child block of a key at `indent`:
/// deeper map keys, or list items whose dash sits at or past `indent`. Leaves `*pos`
/// at the next sibling/parent line, preserving the loop-progress guarantee that every
/// caller relies on.
fn skip_block(lines: &[Line], pos: &mut usize, indent: usize) {
    while let Some(line) = lines.get(*pos) {
        let is_child = match line.dash_indent {
            Some(d) => d >= indent,
            None => line.key_indent > indent,
        };
        if !is_child {
            break;
        }
        *pos += 1;
    }
}

fn parse_map(
    lines: &[Line],
    pos: &mut usize,
    indent: usize,
    mut item_head: bool,
    depth: usize,
) -> Vec<(String, Node)> {
    let mut entries = Vec::new();
    while let Some(line) = lines.get(*pos) {
        if item_head {
            item_head = false; // the "- key: value" line that opens a list item
        } else if line.dash_indent.is_some() || line.key_indent != indent {
            if line.dash_indent.is_none() && line.key_indent > indent {
                *pos += 1; // stray over-indented line: skip it
                continue;
            }
            break;
        }
        *pos += 1;
        let node = if !line.value.is_empty() {
            Node::Scalar(line.value.clone())
        } else {
            match lines.get(*pos) {
                Some(next) if next.dash_indent.is_some_and(|d| d >= indent) => {
                    if depth >= MAX_DEPTH {
                        skip_block(lines, pos, indent);
                        Node::Scalar(String::new())
                    } else {
                        let d = next.dash_indent.expect("checked");
                        Node::List(parse_list(lines, pos, d, depth + 1))
                    }
                }
                Some(next) if next.dash_indent.is_none() && next.key_indent > indent => {
                    if depth >= MAX_DEPTH {
                        skip_block(lines, pos, indent);
                        Node::Scalar(String::new())
                    } else {
                        let child_indent = next.key_indent;
                        Node::Map(parse_map(lines, pos, child_indent, false, depth + 1))
                    }
                }
                _ => Node::Scalar(String::new()),
            }
        };
        entries.push((line.key.clone(), node));
    }
    entries
}

fn parse_list(lines: &[Line], pos: &mut usize, dash_indent: usize, depth: usize) -> Vec<Node> {
    let mut items = Vec::new();
    while let Some(line) = lines.get(*pos) {
        if line.dash_indent != Some(dash_indent) {
            break;
        }
        let key_indent = line.key_indent;
        items.push(Node::Map(parse_map(lines, pos, key_indent, true, depth + 1)));
    }
    items
}

/// Parse iRacing's YAML subset. Never panics; unparseable lines are skipped.
pub fn parse(src: &str) -> Node {
    let lines = tokenize(src);
    let mut pos = 0;
    let mut root = Vec::new();
    while pos < lines.len() {
        let before = pos;
        let indent = lines[pos].key_indent;
        let is_item_head = lines[pos].dash_indent.is_some();
        root.extend(parse_map(&lines, &mut pos, indent, is_item_head, 0));
        if pos == before {
            pos += 1;
        }
    }
    Node::Map(root)
}

pub struct ParsedSession {
    pub info: SessionInfo,
    /// Identity of the session; a change means a new session.
    pub key: String,
}

fn parse_track_length_m(s: &str) -> f32 {
    let mut parts = s.split_whitespace();
    let value: f64 = parts.next().and_then(|v| v.parse().ok()).unwrap_or(0.0);
    let metres = match parts.next() {
        Some("mi") => value * 1609.344,
        Some("m") => value,
        _ => value * 1000.0, // iRacing reports km
    };
    metres as f32
}

pub fn session_from_yaml(yaml: &str, session_num: i32) -> ParsedSession {
    let root = parse(yaml);
    let weekend = |key: &str| {
        root.path(&["WeekendInfo", key]).and_then(Node::str).unwrap_or("").to_string()
    };

    let driver_idx = root.path(&["DriverInfo", "DriverCarIdx"]).and_then(Node::str).unwrap_or("");
    let driver = root
        .path(&["DriverInfo", "Drivers"])
        .map(Node::list)
        .unwrap_or(&[])
        .iter()
        .find(|d| d.get("CarIdx").and_then(Node::str) == Some(driver_idx));
    let car = |key: &str| driver.and_then(|d| d.get(key)).and_then(Node::str).unwrap_or("").to_string();

    let num = session_num.to_string();
    let session_type = root
        .path(&["SessionInfo", "Sessions"])
        .map(Node::list)
        .unwrap_or(&[])
        .iter()
        .find(|s| s.get("SessionNum").and_then(Node::str) == Some(num.as_str()))
        .and_then(|s| s.get("SessionType"))
        .and_then(Node::str)
        .unwrap_or("Unknown")
        .to_string();

    let mut sector_start_pcts: Vec<f32> = root
        .path(&["SplitTimeInfo", "Sectors"])
        .map(Node::list)
        .unwrap_or(&[])
        .iter()
        .filter_map(|s| s.get("SectorStartPct")?.str()?.parse().ok())
        .collect();
    sector_start_pcts.sort_by(|a, b| a.total_cmp(b));

    let mut conditions = BTreeMap::new();
    for (yaml_key, name) in [
        ("TrackAirTemp", "air_temp"),
        ("TrackSurfaceTemp", "track_temp"),
        ("TrackSkies", "skies"),
        ("TrackWeatherType", "weather"),
    ] {
        let v = weekend(yaml_key);
        if !v.is_empty() {
            conditions.insert(name.to_string(), v);
        }
    }

    let display = weekend("TrackDisplayName");
    let info = SessionInfo {
        sim: Sim::Iracing,
        track_id: weekend("TrackID"),
        track_name: if display.is_empty() { weekend("TrackName") } else { display },
        track_config: weekend("TrackConfigName"),
        track_length_m: parse_track_length_m(&weekend("TrackLength")),
        car_id: car("CarID"),
        car_name: car("CarScreenName"),
        session_type,
        sector_start_pcts,
        conditions,
    };
    let key = format!(
        "{}/{}/{}/{}/{}",
        weekend("SubSessionID"),
        session_num,
        info.track_id,
        info.track_config,
        info.car_id
    );
    ParsedSession { info, key }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sim::iracing::testimage::SAMPLE_YAML;

    #[test]
    fn extracts_session_info() {
        let p = session_from_yaml(SAMPLE_YAML, 0);
        let i = &p.info;
        assert_eq!(i.sim, Sim::Iracing);
        assert_eq!(i.track_id, "525");
        assert_eq!(i.track_name, "Circuit de Spa-Francorchamps");
        assert_eq!(i.track_config, "Grand Prix");
        assert!((i.track_length_m - 6930.0).abs() < 0.5);
        assert_eq!(i.car_id, "170");
        assert_eq!(i.car_name, "Porsche 963 GTP");
        assert_eq!(i.session_type, "Practice");
        assert_eq!(i.sector_start_pcts, vec![0.0, 0.33, 0.66]);
        assert_eq!(i.conditions.get("air_temp").map(String::as_str), Some("22.40 C"));
        assert_eq!(i.conditions.get("track_temp").map(String::as_str), Some("31.20 C"));
    }

    #[test]
    fn session_type_follows_session_num() {
        assert_eq!(session_from_yaml(SAMPLE_YAML, 1).info.session_type, "Race");
        assert_eq!(session_from_yaml(SAMPLE_YAML, 7).info.session_type, "Unknown");
    }

    #[test]
    fn key_changes_with_session_num_car_or_track() {
        let a = session_from_yaml(SAMPLE_YAML, 0).key;
        assert_eq!(a, session_from_yaml(SAMPLE_YAML, 0).key);
        assert_ne!(a, session_from_yaml(SAMPLE_YAML, 1).key);
        let other_car = SAMPLE_YAML.replace("CarID: 170", "CarID: 171");
        assert_ne!(a, session_from_yaml(&other_car, 0).key);
    }

    #[test]
    fn nested_lists_stay_nested() {
        let root = parse(SAMPLE_YAML);
        let sessions = root.path(&["SessionInfo", "Sessions"]).unwrap().list();
        assert_eq!(sessions.len(), 2);
        assert_eq!(sessions[0].get("ResultsPositions").unwrap().list().len(), 2);
        assert_eq!(
            root.path(&["WeekendInfo", "WeekendOptions", "StartingGrid"]).and_then(Node::str),
            Some("single file")
        );
    }

    #[test]
    fn values_with_colons_and_quotes() {
        let root = parse(SAMPLE_YAML);
        let drivers = root.path(&["DriverInfo", "Drivers"]).unwrap().list();
        assert_eq!(drivers[1].get("UserName").and_then(Node::str), Some("Test Driver: One"));
    }

    #[test]
    fn missing_sections_give_defaults() {
        let p = session_from_yaml("---\nWeekendInfo:\n TrackName: x\n", 0);
        assert_eq!(p.info.track_name, "x"); // falls back to TrackName
        assert_eq!(p.info.track_length_m, 0.0);
        assert_eq!(p.info.car_name, "");
        assert_eq!(p.info.session_type, "Unknown");
        assert!(p.info.sector_start_pcts.is_empty());
        // Total garbage must not panic either.
        let _ = session_from_yaml("::::\n  - - -\n\t\u{0}", 0);
        let _ = parse("");
    }

    #[test]
    fn deep_nesting_is_capped_not_a_stack_overflow() {
        let mut s = String::new();
        for i in 0..10_000 {
            s.push_str(&" ".repeat(i));
            s.push_str("k:\n");
        }
        s.push_str("Tail: yes\n");
        let root = parse(&s);
        assert_eq!(root.get("Tail").and_then(Node::str), Some("yes"));
    }
}
