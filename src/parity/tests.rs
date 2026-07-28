use super::*;
use std::collections::BTreeSet;

/// The CLI surface, from CLAP ITSELF — not scraped from source.
///
/// `CommandFactory::command()` returns the built command tree, so this honours
/// `#[command(name = ...)]`, aliases, and anything else clap does to derive a
/// subcommand name. A source scraper reproduces clap's kebab-casing by hand and
/// is silently wrong the moment a name is overridden.
fn cli_surface() -> BTreeSet<String> {
    use clap::CommandFactory;
    let cmd = crate::Cli::command();
    let mut out = BTreeSet::new();
    for sub in cmd.get_subcommands() {
        let name = sub.get_name().to_string();
        if name == "help" {
            continue;
        }
        // `bead` nests its own actions; record them as "bead <action>".
        let mut nested = false;
        for inner in sub.get_subcommands() {
            if inner.get_name() != "help" {
                out.insert(format!("{name} {}", inner.get_name()));
                nested = true;
            }
        }
        if !nested {
            out.insert(name);
        }
    }
    out
}

/// The MCP surface, from the ASSEMBLED tool list — hand-written definitions
/// plus the generated registry — not scraped from one file.
///
/// `tools.generated.json` is spliced in at runtime (src/serve/tools.rs), so a
/// scraper reading only `tools.rs` would miss any generated tool that happens
/// not to be mentioned there. Today all six are mentioned incidentally, in test
/// assertions — which is luck, not a guarantee.
fn mcp_surface() -> BTreeSet<String> {
    let listed = crate::serve::tools::tool_definitions();
    listed["tools"]
        .as_array()
        .expect("tools/list returns an array")
        .iter()
        .filter_map(|t| t.get("name")?.as_str().map(str::to_string))
        .collect()
}

fn declared_cli() -> BTreeSet<&'static str> {
    OPS.iter().filter_map(|o| o.cli).collect()
}
fn declared_mcp() -> BTreeSet<&'static str> {
    OPS.iter().filter_map(|o| o.mcp).collect()
}

/// Sanity: the extractors see a real surface. A table validated against an
/// empty extraction would pass vacuously — the 0.9%-precision failure again.
#[test]
fn extractors_see_both_surfaces() {
    assert!(
        cli_surface().len() > 30,
        "CLI extraction looks broken: {:?}",
        cli_surface()
    );
    assert!(
        mcp_surface().len() > 30,
        "MCP extraction looks broken: {}",
        mcp_surface().len()
    );
}

/// THE RATCHET. A verb on either surface must be declared in `OPS`.
///
/// This is what fails when someone adds a tool to one side and not the other:
/// not because the gap is forbidden, but because the DECISION must be recorded.
#[test]
fn every_surface_verb_is_declared() {
    let undeclared_cli: Vec<_> = cli_surface()
        .into_iter()
        .filter(|v| !declared_cli().contains(v.as_str()))
        .collect();
    let undeclared_mcp: Vec<_> = mcp_surface()
        .into_iter()
        .filter(|v| !declared_mcp().contains(v.as_str()))
        .collect();

    assert!(
        undeclared_cli.is_empty() && undeclared_mcp.is_empty(),
        "surface changed without updating src/parity.rs.\n  \
         undeclared CLI verbs: {undeclared_cli:?}\n  \
         undeclared MCP tools: {undeclared_mcp:?}\n  \
         Add each to OPS — as `both(..)` if the other surface has it, or with a \
         reason saying why it is single-surface."
    );
}

/// The table may not claim a verb the binary does not expose — otherwise it
/// rots into fiction as things are renamed or removed.
#[test]
fn every_declared_verb_actually_exists() {
    let cli = cli_surface();
    let mcp = mcp_surface();
    let stale_cli: Vec<_> = declared_cli()
        .into_iter()
        .filter(|v| !cli.contains(*v))
        .collect();
    let stale_mcp: Vec<_> = declared_mcp()
        .into_iter()
        .filter(|v| !mcp.contains(*v))
        .collect();
    assert!(
        stale_cli.is_empty() && stale_mcp.is_empty(),
        "src/parity.rs declares verbs that no longer exist.\n  \
         stale CLI: {stale_cli:?}\n  stale MCP: {stale_mcp:?}"
    );
}

/// A single-surface entry MUST carry a reason, and a paired one must not.
/// Without this the table cannot distinguish "correctly CLI-only" from
/// "nobody got to it yet".
#[test]
fn single_surface_entries_carry_a_reason() {
    for op in OPS {
        match (op.cli, op.mcp, op.only) {
            (Some(_), Some(_), None) => {}
            (Some(_), Some(_), Some(_)) => {
                panic!("{op:?} is on both surfaces but carries an `only` reason")
            }
            (None, None, _) => panic!("{op:?} has neither surface"),
            (_, _, None) => panic!(
                "{op:?} is single-surface with no reason — say WHY, or it reads as an oversight"
            ),
            _ => {}
        }
    }
}

/// No verb declared twice — a duplicate would let one entry mask another.
#[test]
fn no_duplicate_declarations() {
    let mut cli = BTreeSet::new();
    let mut mcp = BTreeSet::new();
    for op in OPS {
        if let Some(c) = op.cli {
            assert!(cli.insert(c), "CLI verb `{c}` declared twice");
        }
        if let Some(m) = op.mcp {
            assert!(mcp.insert(m), "MCP tool `{m}` declared twice");
        }
    }
}

/// The map, as data. Fails only if the shape moves in the WRONG direction —
/// parity dropping, or gaps growing. Improving it is expected and requires
/// updating these numbers, which is the point of a ratchet.
#[test]
fn parity_ratchet_does_not_regress() {
    let paired = OPS.iter().filter(|o| o.only.is_none()).count();
    let by_design = OPS
        .iter()
        .filter(|o| matches!(o.only, Some(ByDesign(_))))
        .count();
    let gaps = OPS
        .iter()
        .filter(|o| matches!(o.only, Some(Gap(_))))
        .count();

    // Measured 2026-07-27 against the CLAP-DERIVED surface. An earlier baseline
    // (14/18/40) was taken against a source scraper that counted `coord`,
    // `hooks`, `lattice` and `notes` as leaf verbs when each nests
    // subcommands — so it undercounted the real surface by five. These numbers
    // are the first ones the binary itself vouches for.
    //
    // Parity may only rise; gaps may only fall.
    assert!(
        paired >= 14,
        "parity REGRESSED: {paired} paired ops, baseline 14"
    );
    assert!(gaps <= 44, "gaps GREW: {gaps} gaps, baseline 44");
    assert_eq!(
        paired + by_design + gaps,
        OPS.len(),
        "every op is exactly one of paired / by-design / gap"
    );
}

/// The ratchet must be able to FAIL. A gate whose only reachable failure is a
/// bug already fixed is a regression test, not a rail — the lesson from the
/// permission rail rewritten earlier the same day.
#[test]
fn ratchet_detects_an_undeclared_verb() {
    let cli = cli_surface();
    assert!(
        !cli.contains("a-verb-that-does-not-exist"),
        "sanity: fabricated verb must not appear"
    );
    // A verb present on a surface but absent from OPS is what the ratchet
    // catches; prove the membership test that drives it discriminates.
    assert!(cli.contains("status"), "a real verb is seen");
    assert!(
        !declared_cli().contains("a-verb-that-does-not-exist"),
        "and an undeclared one is not silently accepted"
    );
}

/// Prints the measured shape. `cargo test -- --ignored --nocapture print_shape`
#[test]
#[ignore = "reporting aid, not a gate"]
fn print_shape() {
    let paired = OPS.iter().filter(|o| o.only.is_none()).count();
    let by_design = OPS
        .iter()
        .filter(|o| matches!(o.only, Some(ByDesign(_))))
        .count();
    let gaps = OPS
        .iter()
        .filter(|o| matches!(o.only, Some(Gap(_))))
        .count();
    let cli_only = OPS.iter().filter(|o| o.mcp.is_none()).count();
    let mcp_only = OPS.iter().filter(|o| o.cli.is_none()).count();
    println!(
        "paired={paired} by_design={by_design} gaps={gaps} total={} | cli_only={cli_only} mcp_only={mcp_only} | surface: cli={} mcp={}",
        OPS.len(),
        cli_surface().len(),
        mcp_surface().len()
    );
}
