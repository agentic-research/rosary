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
        warnings: Vec::new(),
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

/// A raw newline inside a quoted DOT string is whitespace, not a line
/// break — multi-line labels (which every degraded graph has) would
/// silently collapse onto one line.
#[test]
fn dot_newlines_become_line_break_escapes() {
    assert_eq!(dot_quote("a\nb"), "\"a\\nb\"");
    let mut model = sample();
    model.warnings.push("store unreadable".into());
    let dot = model.render(Format::Dot);
    let label_line = dot
        .lines()
        .find(|l| l.contains("labelloc"))
        .expect("graph label present");
    assert!(
        label_line.contains("INCOMPLETE"),
        "the whole label must stay on one physical line: {label_line}"
    );
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

#[test]
fn degraded_graph_declares_itself_in_both_formats() {
    // A silently-degraded graph reads as an accurate one — especially once
    // it's a PNG someone else opened, where stderr no longer exists.
    let mut model = sample();
    model.warnings.push("bead store unreadable: boom".into());

    let dot = model.render(Format::Dot);
    assert!(
        dot.contains("INCOMPLETE"),
        "dot must declare degradation: {dot}"
    );
    assert!(dot.contains("boom"), "dot must carry the cause");

    let mm = model.render(Format::Mermaid);
    assert!(
        mm.contains("INCOMPLETE"),
        "mermaid must declare degradation"
    );
    assert!(mm.contains("boom"), "mermaid must carry the cause");
}

#[test]
fn clean_graph_carries_no_incomplete_marker() {
    let dot = sample().render(Format::Dot);
    assert!(!dot.contains("INCOMPLETE"), "must not cry wolf");
}

#[test]
fn mermaid_declares_edges_it_could_not_draw() {
    let model = GraphModel {
        nodes: vec![bead("rosary-aaaaaa", 1, "open")],
        // Endpoint that was never emitted as a node.
        edges: vec![("rosary-aaaaaa".into(), "rosary-missing".into())],
        ..Default::default()
    };
    let mm = model.render(Format::Mermaid);
    assert!(
        mm.contains("1 edge(s) omitted"),
        "dropped edges must be declared, not silently skipped: {mm}"
    );
}
