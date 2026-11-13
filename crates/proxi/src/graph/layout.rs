//! Layered layout of the knowledge map: depths, rows and the implication midpoint.

use std::cmp::Ordering;
use std::collections::{BTreeMap, HashMap, HashSet};

/// Vertical distance between inference bands.
pub const ROW: f64 = 80.0;
/// Vertical distance between the wrapped sub-rows of one band.
pub const SUB_ROW: f64 = 36.0;
/// Horizontal distance between columns.
pub const COL: f64 = 64.0;
/// Columns a lesson gets before its row wraps.
pub const COLS: usize = 4;
/// Gap between two lessons of one course.
pub const LESSON_GAP: f64 = 56.0;
/// Wider gap between two courses' frames.
pub const COURSE_GAP: f64 = 200.0;
/// Ids of the synthetic implication junctions.
pub const IMP_ID_PREFIX: &str = "arg_";

/// Inference depth per node: the longest chain of directed edges from a base node.
/// Cycles are condensed (Tarjan SCC) so mutual support reads as equal footing.
pub fn inference_depths(nodes: &[String], edges: &[(String, String)]) -> HashMap<String, usize> {
    let ids: Vec<&str> = nodes.iter().map(String::as_str).collect();
    depths_of(&ids, edges)
}

fn depths_of(nodes: &[&str], edges: &[(String, String)]) -> HashMap<String, usize> {
    let mut index: HashMap<&str, usize> = HashMap::new();
    let mut order: Vec<&str> = Vec::new();
    for &id in nodes {
        if !index.contains_key(id) {
            index.insert(id, order.len());
            order.push(id);
        }
    }
    let count = order.len();

    let mut adj: Vec<Vec<usize>> = vec![Vec::new(); count];
    for (from, to) in edges {
        if from == to {
            continue;
        }
        let (Some(&a), Some(&b)) = (index.get(from.as_str()), index.get(to.as_str())) else {
            continue;
        };
        adj[a].push(b);
    }

    // Iterative Tarjan: an explicit frame stack, no recursion depth limits.
    let mut num = vec![usize::MAX; count];
    let mut low = vec![0usize; count];
    let mut on_stack = vec![false; count];
    let mut stack: Vec<usize> = Vec::new();
    let mut comp = vec![usize::MAX; count];
    let mut counter = 0usize;
    let mut comp_count = 0usize;
    for start in 0..count {
        if num[start] != usize::MAX {
            continue;
        }
        num[start] = counter;
        low[start] = counter;
        counter += 1;
        stack.push(start);
        on_stack[start] = true;
        let mut work: Vec<(usize, usize)> = vec![(start, 0)];
        while !work.is_empty() {
            let (v, i) = *work.last().unwrap();
            if i < adj[v].len() {
                let w = adj[v][i];
                work.last_mut().unwrap().1 = i + 1;
                if num[w] == usize::MAX {
                    num[w] = counter;
                    low[w] = counter;
                    counter += 1;
                    stack.push(w);
                    on_stack[w] = true;
                    work.push((w, 0));
                } else if on_stack[w] {
                    low[v] = low[v].min(num[w]);
                }
            } else {
                work.pop();
                if low[v] == num[v] {
                    loop {
                        let w = stack.pop().unwrap();
                        on_stack[w] = false;
                        comp[w] = comp_count;
                        if w == v {
                            break;
                        }
                    }
                    comp_count += 1;
                }
                if let Some(&(parent, _)) = work.last() {
                    low[parent] = low[parent].min(low[v]);
                }
            }
        }
    }

    // Tarjan emits sinks first, so descending component index is sources first.
    let mut preds: Vec<HashSet<usize>> = vec![HashSet::new(); comp_count];
    for (from, to) in edges {
        let (Some(&a), Some(&b)) = (index.get(from.as_str()), index.get(to.as_str())) else {
            continue;
        };
        if comp[a] != comp[b] {
            preds[comp[b]].insert(comp[a]);
        }
    }
    let mut depth = vec![0usize; comp_count];
    for c in (0..comp_count).rev() {
        let mut d = 0usize;
        for &p in &preds[c] {
            d = d.max(depth[p] + 1);
        }
        depth[c] = d;
    }

    let mut out = HashMap::new();
    for (i, id) in order.iter().enumerate() {
        out.insert(id.to_string(), depth[comp[i]]);
    }
    out
}

/// Deterministic layered layout: inference bands bottom-up, one column block per
/// birth lesson, courses side by side, implication junctions between premises and conclusion.
pub fn layered_layout(
    nodes: &[String],
    logical_edges: &[(String, String)],
    imp_links: &[(String, Vec<String>, String)],
    lesson_of: &dyn Fn(&str) -> String,
) -> Vec<(String, f64, f64)> {
    let props: Vec<&str> = nodes
        .iter()
        .map(String::as_str)
        .filter(|id| !id.starts_with(IMP_ID_PREFIX))
        .collect();
    let depth = depths_of(&props, logical_edges);

    let mut premises: HashMap<&str, Vec<&str>> = HashMap::new();
    for (from, to) in logical_edges {
        premises.entry(to.as_str()).or_default().push(from.as_str());
    }

    let mut lessons: Vec<String> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    for id in &props {
        let lesson = lesson_of(id);
        if seen.insert(lesson.clone()) {
            lessons.push(lesson);
        }
    }
    lessons.sort();

    let mut x: HashMap<&str, f64> = HashMap::new();
    let mut slot: Vec<(&str, usize, usize)> = Vec::new(); // id, band, sub-row
    let mut slot_index: HashMap<&str, usize> = HashMap::new();
    let mut sub_rows: BTreeMap<usize, usize> = BTreeMap::new();
    let mut cursor = 0.0_f64;
    let mut course: Option<String> = None;

    for lesson in &lessons {
        let c = lesson.split('/').next().unwrap_or("");
        if course.as_deref().is_some_and(|prev| prev != c) {
            cursor += COURSE_GAP - LESSON_GAP;
        }
        course = Some(c.to_string());

        let mut rows: BTreeMap<usize, Vec<&str>> = BTreeMap::new();
        for id in &props {
            if lesson_of(id) != *lesson {
                continue;
            }
            rows.entry(depth.get(*id).copied().unwrap_or(0))
                .or_default()
                .push(id);
        }

        let width = rows
            .values()
            .map(Vec::len)
            .max()
            .unwrap_or(1)
            .min(COLS)
            .max(1);
        for (&d, row) in rows.iter_mut() {
            row.sort_by(|a, b| {
                let diff = mean_x(a, &premises, &x) - mean_x(b, &premises, &x);
                if diff != 0.0 && !diff.is_nan() {
                    if diff < 0.0 {
                        Ordering::Less
                    } else {
                        Ordering::Greater
                    }
                } else {
                    a.cmp(b)
                }
            });
            let mut i = 0;
            while i < row.len() {
                let line = &row[i..(i + width).min(row.len())];
                let left = cursor + (width - line.len()) as f64 * COL / 2.0;
                for (j, id) in line.iter().enumerate() {
                    x.insert(*id, left + j as f64 * COL);
                    match slot_index.get(*id) {
                        Some(&existing) => {
                            slot[existing].1 = d;
                            slot[existing].2 = i / width;
                        }
                        None => {
                            slot_index.insert(*id, slot.len());
                            slot.push((*id, d, i / width));
                        }
                    }
                }
                i += width;
            }
            let entry = sub_rows.entry(d).or_insert(1);
            *entry = (*entry).max(row.len().div_ceil(width));
        }
        cursor += width as f64 * COL + LESSON_GAP;
    }

    // Band bottoms from the foundations up: y grows downwards, so up is negative.
    let mut base: BTreeMap<usize, f64> = BTreeMap::new();
    let mut y = 0.0_f64;
    for (&d, &rows) in &sub_rows {
        base.insert(d, y);
        y -= (rows - 1) as f64 * SUB_ROW + ROW;
    }

    let mut out: Vec<(String, f64, f64)> = Vec::new();
    let mut out_index: HashMap<&str, usize> = HashMap::new();
    for (id, d, sub) in &slot {
        out_index.insert(id, out.len());
        out.push((id.to_string(), x[id], base[d] - *sub as f64 * SUB_ROW));
    }

    for (id, link_premises, conclusion) in imp_links {
        let mut px = 0.0;
        let mut placed = 0usize;
        let mut top = f64::INFINITY;
        for p in link_premises {
            if let Some(&idx) = out_index.get(p.as_str()) {
                px += out[idx].1;
                top = top.min(out[idx].2);
                placed += 1;
            }
        }
        let Some(&ci) = out_index.get(conclusion.as_str()) else {
            continue;
        };
        if placed == 0 {
            continue;
        }
        let x_mid = (px / placed as f64 + out[ci].1) / 2.0;
        let y_mid = (top + out[ci].2) / 2.0;
        match out_index.get(id.as_str()) {
            Some(&existing) => {
                out[existing].1 = x_mid;
                out[existing].2 = y_mid;
            }
            None => {
                out_index.insert(id.as_str(), out.len());
                out.push((id.clone(), x_mid, y_mid));
            }
        }
    }

    // A junction whose premises were all dropped is never drawn but still needs a slot.
    for id in nodes {
        if out_index.contains_key(id.as_str()) {
            continue;
        }
        out_index.insert(id.as_str(), out.len());
        out.push((id.clone(), cursor, 0.0));
    }
    out
}

/// Mean x of the already-placed premises; `inf` when none is placed, so the row orders by id.
fn mean_x(id: &str, premises: &HashMap<&str, Vec<&str>>, x: &HashMap<&str, f64>) -> f64 {
    let Some(list) = premises.get(id) else {
        return f64::INFINITY;
    };
    let mut sum = 0.0;
    let mut count = 0usize;
    for p in list {
        if let Some(v) = x.get(*p) {
            sum += v;
            count += 1;
        }
    }
    if count == 0 {
        f64::INFINITY
    } else {
        sum / count as f64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strings(ids: &[&str]) -> Vec<String> {
        ids.iter().map(|id| id.to_string()).collect()
    }

    fn edges(list: &[(&str, &str)]) -> Vec<(String, String)> {
        list.iter()
            .map(|(from, to)| (from.to_string(), to.to_string()))
            .collect()
    }

    fn at(pos: &[(String, f64, f64)], id: &str) -> (f64, f64) {
        let found = pos
            .iter()
            .find(|(node, _, _)| node == id)
            .unwrap_or_else(|| panic!("no position for {id}"));
        (found.1, found.2)
    }

    #[test]
    fn depths_collapse_cycles_and_ignore_self_loops_and_unknowns() {
        let nodes = strings(&["a", "b", "c", "d"]);
        let depths = inference_depths(&nodes, &edges(&[("a", "b"), ("b", "a"), ("b", "c")]));
        assert_eq!(depths["a"], 0);
        assert_eq!(depths["b"], 0);
        assert_eq!(depths["c"], 1);
        assert_eq!(depths["d"], 0);

        let depths = inference_depths(
            &nodes,
            &edges(&[("a", "a"), ("x", "a"), ("c", "missing"), ("a", "c")]),
        );
        assert_eq!(depths["a"], 0);
        assert_eq!(depths["c"], 1);

        let depths = inference_depths(
            &nodes,
            &edges(&[("a", "b"), ("b", "c"), ("c", "a"), ("c", "d")]),
        );
        assert_eq!(
            (depths["a"], depths["b"], depths["c"], depths["d"]),
            (0, 0, 0, 1)
        );
    }

    #[test]
    fn places_bands_lessons_and_the_implication_junction() {
        let nodes = strings(&["n_a", "n_b", "n_c", "n_d", "n_e", "n_g", "arg_1"]);
        let edges = edges(&[("n_a", "n_c"), ("n_b", "n_c"), ("n_c", "n_d"), ("n_e", "n_g")]);
        let imps = vec![(
            "arg_1".to_string(),
            strings(&["n_a", "n_b"]),
            "n_c".to_string(),
        )];
        let lesson = |id: &str| match id {
            "n_a" | "n_b" | "n_c" => "a/0001-x".to_string(),
            "n_d" => "a/0002-y".to_string(),
            _ => "b/0001-z".to_string(),
        };

        let pos = layered_layout(&nodes, &edges, &imps, &lesson);
        assert_eq!(
            pos,
            vec![
                ("n_a".to_string(), 0.0, 0.0),
                ("n_b".to_string(), 64.0, 0.0),
                ("n_c".to_string(), 32.0, -80.0),
                ("n_d".to_string(), 184.0, -160.0),
                ("n_e".to_string(), 448.0, 0.0),
                ("n_g".to_string(), 448.0, -80.0),
                ("arg_1".to_string(), 32.0, -40.0),
            ]
        );
    }

    #[test]
    fn a_long_row_wraps_into_sub_rows_inside_its_band() {
        let nodes = strings(&["n_1", "n_2", "n_3", "n_4", "n_5"]);
        let pos = layered_layout(&nodes, &[], &[], &|_: &str| "a/0001-x".to_string());
        assert_eq!(at(&pos, "n_1"), (0.0, 0.0));
        assert_eq!(at(&pos, "n_2"), (64.0, 0.0));
        assert_eq!(at(&pos, "n_3"), (128.0, 0.0));
        assert_eq!(at(&pos, "n_4"), (192.0, 0.0));
        assert_eq!(at(&pos, "n_5"), (96.0, -36.0));
    }

    #[test]
    fn the_layout_is_deterministic() {
        let nodes = strings(&["n_c", "n_a", "n_b", "arg_1"]);
        let edges = edges(&[("n_a", "n_c"), ("n_b", "n_c")]);
        let imps = vec![(
            "arg_1".to_string(),
            strings(&["n_a", "n_b"]),
            "n_c".to_string(),
        )];
        let lesson = |_: &str| "a/0001-x".to_string();
        let first = layered_layout(&nodes, &edges, &imps, &lesson);
        let second = layered_layout(&nodes, &edges, &imps, &lesson);
        assert_eq!(first, second);
    }

    #[test]
    fn an_implication_without_a_placed_premise_lands_at_the_end() {
        let nodes = strings(&["n_a", "arg_x"]);
        let imps = vec![(
            "arg_x".to_string(),
            strings(&["n_missing"]),
            "n_a".to_string(),
        )];
        let pos = layered_layout(&nodes, &[], &imps, &|_: &str| "a/0001-x".to_string());
        assert_eq!(
            pos,
            vec![
                ("n_a".to_string(), 0.0, 0.0),
                ("arg_x".to_string(), 120.0, 0.0),
            ]
        );
    }
}
