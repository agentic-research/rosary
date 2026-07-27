//! `rsry graph` — emit the bead lattice as graph text for visual inspection.
//!
//! Rosary stores every edge needed to draw its own structure (decades →
//! threads → beads, plus dependency edges) but has had no way to render it,
//! so structure was only inspectable via ad-hoc SQL. This module closes that
//! gap (rosary-8a58bc).
//!
//! **No new dependencies.** DOT and mermaid are plain text; graphviz and
//! mermaid are *renderers* that live outside the binary.
//!
//! The model/render split is deliberate: [`GraphModel`] is pure data, so the
//! emitters are testable without a store. Only [`build`] touches the backend.

use std::collections::{BTreeMap, BTreeSet};

use anyhow::Result;

use crate::store::HierarchyStore;

/// Which tier a node belongs to. Drives shape + colour so the three tiers are
/// distinguishable at a glance rather than inferred from id spelling.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tier {
    Decade,
    Thread,
    Bead,
}

/// How deep to render. A full bead-level graph of the whole fleet is an
/// unreadable hairball (~3k nodes), while decade+thread is ~300 and reads
/// fine — so depth is a first-class control, not an afterthought.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Depth {
    /// Decades only — the fleet shape.
    Decade,
    /// Decades + threads — the "full export" view.
    Thread,
    /// Everything down to individual beads. Scope it (`--decade`/`--orphans`)
    /// or expect soup.
    Bead,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Dot,
    Mermaid,
}

/// A rendered node. `label` is already truncated; escaping happens per-format.
#[derive(Debug, Clone)]
pub struct Node {
    pub id: String,
    pub label: String,
    pub tier: Tier,
    /// Bead priority (0-3) when known. Drives fill colour for bead nodes.
    pub priority: Option<u8>,
    /// Bead status when known. Terminal beads render muted.
    pub status: Option<String>,
}

impl Node {
    fn is_terminal(&self) -> bool {
        matches!(self.status.as_deref(), Some("done" | "closed"))
    }
}

/// Pure, renderable graph. Built once, emitted in any format.
#[derive(Debug, Default, Clone)]
pub struct GraphModel {
    pub nodes: Vec<Node>,
    /// (from, to) — containment edges (decade→thread→bead).
    pub edges: Vec<(String, String)>,
    /// (from, to) — dependency edges, rendered distinctly from containment.
    pub dep_edges: Vec<(String, String)>,
    /// Human-readable description of what was rendered, for the graph title.
    pub caption: String,
}

impl GraphModel {
    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    pub fn render(&self, format: Format) -> String {
        match format {
            Format::Dot => self.render_dot(),
            Format::Mermaid => self.render_mermaid(),
        }
    }

    /// Fill colour for a bead node. Terminal beads go green regardless of
    /// priority — "done" is the more useful signal once it's true.
    fn bead_fill(node: &Node) -> &'static str {
        if node.is_terminal() {
            return "#d9f2d9";
        }
        match node.priority {
            Some(0) => "#ffd6d6",
            Some(1) => "#ffe8cc",
            Some(2) => "#fdfdfd",
            _ => "#f4f4f4",
        }
    }

    fn render_dot(&self) -> String {
        let mut out = String::new();
        out.push_str("digraph rosary {\n");
        out.push_str("  rankdir=LR;\n");
        out.push_str("  graph [fontname=\"Helvetica\", labelloc=\"t\", label=");
        out.push_str(&dot_quote(&self.caption));
        out.push_str("];\n");
        out.push_str("  node [fontname=\"Helvetica\", fontsize=10];\n");
        out.push_str("  edge [color=\"#888888\"];\n\n");

        for node in &self.nodes {
            let (shape, fill, size) = match node.tier {
                Tier::Decade => ("folder", "#cfe3ff", 13),
                Tier::Thread => ("box3d", "#e8e8e8", 11),
                Tier::Bead => ("box", Self::bead_fill(node), 10),
            };
            let style = if node.tier == Tier::Bead {
                "rounded,filled"
            } else {
                "filled"
            };
            out.push_str(&format!(
                "  {} [label={}, shape={}, style=\"{}\", fillcolor=\"{}\", fontsize={}];\n",
                dot_quote(&node.id),
                dot_quote(&node.label),
                shape,
                style,
                fill,
                size
            ));
        }

        if !self.edges.is_empty() {
            out.push('\n');
        }
        for (from, to) in &self.edges {
            out.push_str(&format!("  {} -> {};\n", dot_quote(from), dot_quote(to)));
        }
        for (from, to) in &self.dep_edges {
            out.push_str(&format!(
                "  {} -> {} [style=dashed, color=\"#cc4444\", constraint=false];\n",
                dot_quote(from),
                dot_quote(to)
            ));
        }

        out.push_str("}\n");
        out
    }

    fn render_mermaid(&self) -> String {
        let mut out = String::new();
        out.push_str("graph LR\n");
        if !self.caption.is_empty() {
            out.push_str(&format!("  %% {}\n", self.caption.replace('\n', " ")));
        }

        // Mermaid node ids can't contain most punctuation, so index-alias them
        // and keep the real id in the label.
        let alias: BTreeMap<&str, String> = self
            .nodes
            .iter()
            .enumerate()
            .map(|(i, n)| (n.id.as_str(), format!("n{i}")))
            .collect();

        for node in &self.nodes {
            let a = &alias[node.id.as_str()];
            let label = mermaid_escape(&node.label);
            // Distinct bracket syntax per tier: hexagon / parallelogram /
            // rounded — so tier survives even without CSS classes.
            let shaped = match node.tier {
                Tier::Decade => format!("{a}{{{{\"{label}\"}}}}"),
                Tier::Thread => format!("{a}[/\"{label}\"/]"),
                Tier::Bead => format!("{a}(\"{label}\")"),
            };
            out.push_str(&format!("  {shaped}\n"));
            let class = match node.tier {
                Tier::Decade => "decade",
                Tier::Thread => "thread",
                Tier::Bead => {
                    if node.is_terminal() {
                        "beadDone"
                    } else {
                        match node.priority {
                            Some(0) => "beadP0",
                            Some(1) => "beadP1",
                            _ => "bead",
                        }
                    }
                }
            };
            out.push_str(&format!("  class {a} {class};\n"));
        }

        for (from, to) in &self.edges {
            if let (Some(f), Some(t)) = (alias.get(from.as_str()), alias.get(to.as_str())) {
                out.push_str(&format!("  {f} --> {t}\n"));
            }
        }
        for (from, to) in &self.dep_edges {
            if let (Some(f), Some(t)) = (alias.get(from.as_str()), alias.get(to.as_str())) {
                out.push_str(&format!("  {f} -.-> {t}\n"));
            }
        }

        out.push_str("  classDef decade fill:#cfe3ff,stroke:#5588cc;\n");
        out.push_str("  classDef thread fill:#e8e8e8,stroke:#999999;\n");
        out.push_str("  classDef bead fill:#fdfdfd,stroke:#bbbbbb;\n");
        out.push_str("  classDef beadP0 fill:#ffd6d6,stroke:#cc5555;\n");
        out.push_str("  classDef beadP1 fill:#ffe8cc,stroke:#cc9955;\n");
        out.push_str("  classDef beadDone fill:#d9f2d9,stroke:#66aa66;\n");
        out
    }
}

/// DOT string literal. Escapes backslashes first, then quotes — order matters.
fn dot_quote(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

/// Mermaid labels live inside quotes; the quote itself is the only real
/// hazard, and `#` starts an entity reference.
fn mermaid_escape(s: &str) -> String {
    s.replace('#', "&num;").replace('"', "&quot;")
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let head: String = s.chars().take(max.saturating_sub(1)).collect();
    format!("{head}…")
}

/// What to render.
#[derive(Debug, Clone)]
pub struct Spec {
    pub depth: Depth,
    /// Restrict to a single decade.
    pub decade: Option<String>,
    /// Render only beads with no thread assignment. Implies `Depth::Bead`.
    pub orphans: bool,
}

/// Minimal bead facts the renderer needs. Kept separate from `Bead` so the
/// builder can degrade gracefully when a bead's store isn't reachable.
#[derive(Debug, Clone)]
pub struct BeadFacts {
    pub title: String,
    pub priority: u8,
    pub status: String,
}

/// Build the model from the hierarchy store.
///
/// `bead_facts` supplies bead metadata (title/priority/status) for the ids the
/// hierarchy references. Callers that can't resolve a bead simply omit it —
/// the node still renders, labelled by id.
pub async fn build(
    hierarchy: &dyn HierarchyStore,
    spec: &Spec,
    bead_facts: &BTreeMap<String, BeadFacts>,
) -> Result<GraphModel> {
    let mut model = GraphModel::default();

    if spec.orphans {
        let mut routed: BTreeSet<String> = BTreeSet::new();
        for decade in hierarchy.list_decades(None).await? {
            for thread in hierarchy.list_threads(&decade.id).await? {
                for wr in hierarchy.list_beads_in_thread(&thread.id).await? {
                    routed.insert(wr.bead_id);
                }
            }
        }
        for (id, facts) in bead_facts {
            if routed.contains(id) {
                continue;
            }
            model.nodes.push(Node {
                id: id.clone(),
                label: format!("{id}\nP{} {}", facts.priority, truncate(&facts.title, 34)),
                tier: Tier::Bead,
                priority: Some(facts.priority),
                status: Some(facts.status.clone()),
            });
        }
        model.caption = format!("rosary — {} orphaned beads (no thread)", model.nodes.len());
        return Ok(model);
    }

    let decades = match &spec.decade {
        Some(id) => hierarchy.get_decade(id).await?.into_iter().collect(),
        None => hierarchy.list_decades(None).await?,
    };

    for decade in &decades {
        model.nodes.push(Node {
            id: decade.id.clone(),
            label: truncate(&decade.title, 44),
            tier: Tier::Decade,
            priority: None,
            status: Some(decade.status.clone()),
        });

        if spec.depth == Depth::Decade {
            continue;
        }

        for thread in hierarchy.list_threads(&decade.id).await? {
            let members = hierarchy.list_beads_in_thread(&thread.id).await?;
            let label = if spec.depth == Depth::Thread {
                format!("{} ({})", truncate(&thread.name, 38), members.len())
            } else {
                truncate(&thread.name, 38)
            };
            model.nodes.push(Node {
                id: thread.id.clone(),
                label,
                tier: Tier::Thread,
                priority: None,
                status: None,
            });
            model.edges.push((decade.id.clone(), thread.id.clone()));

            if spec.depth != Depth::Bead {
                continue;
            }

            for wr in members {
                let (label, priority, status) = match bead_facts.get(&wr.bead_id) {
                    Some(f) => (
                        format!("{}\nP{} {}", wr.bead_id, f.priority, truncate(&f.title, 30)),
                        Some(f.priority),
                        Some(f.status.clone()),
                    ),
                    None => (wr.bead_id.clone(), None, None),
                };
                model.nodes.push(Node {
                    id: wr.bead_id.clone(),
                    label,
                    tier: Tier::Bead,
                    priority,
                    status,
                });
                model.edges.push((thread.id.clone(), wr.bead_id.clone()));
            }
        }
    }

    model.caption = match (&spec.decade, spec.depth) {
        (Some(d), _) => format!("rosary — decade {d}"),
        (None, Depth::Decade) => format!("rosary — {} decades", decades.len()),
        (None, _) => format!("rosary — {} decades, full lattice", decades.len()),
    };
    Ok(model)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bead(id: &str, priority: u8, status: &str) -> Node {
        Node {
            id: id.into(),
            label: id.into(),
            tier: Tier::Bead,
            priority: Some(priority),
            status: Some(status.into()),
        }
    }

    fn sample() -> GraphModel {
        GraphModel {
            nodes: vec![
                Node {
                    id: "dec-1".into(),
                    label: "Decade One".into(),
                    tier: Tier::Decade,
                    priority: None,
                    status: Some("active".into()),
                },
                Node {
                    id: "dec-1/thread".into(),
                    label: "Thread A".into(),
                    tier: Tier::Thread,
                    priority: None,
                    status: None,
                },
                bead("rosary-aaaaaa", 0, "open"),
                bead("rosary-bbbbbb", 2, "done"),
            ],
            edges: vec![
                ("dec-1".into(), "dec-1/thread".into()),
                ("dec-1/thread".into(), "rosary-aaaaaa".into()),
                ("dec-1/thread".into(), "rosary-bbbbbb".into()),
            ],
            dep_edges: vec![("rosary-aaaaaa".into(), "rosary-bbbbbb".into())],
            caption: "test graph".into(),
        }
    }

    #[test]
    fn dot_is_well_formed_and_denotes_each_tier() {
        let dot = sample().render(Format::Dot);
        assert!(dot.starts_with("digraph rosary {"));
        assert!(dot.trim_end().ends_with('}'));
        // Each tier gets a distinct shape — the whole point of the render.
        assert!(dot.contains("shape=folder"), "decade shape missing");
        assert!(dot.contains("shape=box3d"), "thread shape missing");
        assert!(dot.contains("shape=box,"), "bead shape missing");
        // One line per node + one per edge.
        assert_eq!(dot.matches(" -> ").count(), 4, "3 containment + 1 dep");
        assert!(dot.contains("style=dashed"), "dep edge not distinguished");
    }

    #[test]
    fn dot_colours_by_priority_and_terminal_status() {
        let dot = sample().render(Format::Dot);
        assert!(dot.contains("#ffd6d6"), "P0 fill missing");
        // A done bead renders green even though its priority is P2.
        assert!(dot.contains("#d9f2d9"), "terminal fill missing");
    }

    #[test]
    fn mermaid_is_well_formed_and_denotes_each_tier() {
        let mm = sample().render(Format::Mermaid);
        assert!(mm.starts_with("graph LR"));
        assert!(mm.contains("{{"), "decade hexagon missing");
        assert!(mm.contains("[/"), "thread parallelogram missing");
        assert!(mm.contains("classDef decade"));
        assert_eq!(mm.matches(" --> ").count(), 3);
        assert_eq!(mm.matches(" -.-> ").count(), 1, "dep edge style");
    }

    #[test]
    fn empty_model_still_emits_valid_graphs() {
        let empty = GraphModel::default();
        assert!(empty.is_empty());
        let dot = empty.render(Format::Dot);
        assert!(dot.starts_with("digraph rosary {"));
        assert!(dot.trim_end().ends_with('}'));
        assert_eq!(dot.matches(" -> ").count(), 0);
        let mm = empty.render(Format::Mermaid);
        assert!(mm.starts_with("graph LR"));
    }

    #[test]
    fn labels_with_quotes_and_backslashes_are_escaped() {
        let model = GraphModel {
            nodes: vec![Node {
                id: "a\"b".into(),
                label: "say \"hi\" c:\\path".into(),
                tier: Tier::Bead,
                priority: Some(1),
                status: None,
            }],
            ..Default::default()
        };
        let dot = model.render(Format::Dot);
        assert!(dot.contains("\\\"hi\\\""), "quotes not escaped: {dot}");
        assert!(dot.contains("c:\\\\path"), "backslash not escaped: {dot}");
    }

    #[test]
    fn mermaid_escapes_hash_and_quote() {
        assert_eq!(mermaid_escape("a#b\"c"), "a&num;b&quot;c");
    }

    #[test]
    fn truncate_is_char_safe_on_multibyte() {
        // Byte-slicing here would panic; assert we count chars.
        assert_eq!(truncate("ααααα", 3), "αα…");
        assert_eq!(truncate("abc", 10), "abc");
    }
}
