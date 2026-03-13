# Golden Rules of ART

These rules are **active defaults** — every rosary-dispatched agent operates under them. An agent MAY waive a rule for a specific finding, but must tag the waiver explicitly:

```bash
bd create "..." --labels "waiver:rule-4,reason:legacy-test-infra"
```

A finding that violates a rule without a waiver is a bug in the agent, not the code.

---

## 1. No Versioned Files
Never create files with version numbers in their names. Use configuration to manage variants. Git provides version control.

**Agent enforcement**: Flag any file matching `*_v[0-9]*.*`, `*_final.*`, `*_old.*`, `*_backup.*`.

## 2. Keep Files Under 200 Lines
Refactor when approaching this limit. Forces single-responsibility, makes code reviewable.

**Agent enforcement**: Flag files over 200 lines. Action type: `refactor`.

## 3. Every Module is Runnable
Include a smoke test or example in the entry point. Proves the module works in isolation.

**Agent enforcement**: Flag modules with no runnable entry point and no test file. Action type: `docs`.

## 4. Test Reality, Not Mocks
Never write tests that only validate mocked behavior. Include integration tests that touch real systems.

**Agent enforcement**: This is staging-agent's primary mission. Flag tests where the mock IS the test.

## 5. Use Tools That Excel
Search for prior art before building custom solutions. No amount of optimization fixes the wrong algorithm.

**Agent enforcement**: Flag reimplementations of standard library functionality. Action type: `negate`.

## 6. Validate Recursively
Good methodologies prove themselves through use. A methodology that can't validate itself is incomplete.

**Agent enforcement**: Meta-level — rosary should validate itself. Not directly enforced per-finding.

## 7. Guide, Don't Gatekeep
Every enforcement mechanism must include its own escape hatch. Methodology should illuminate, not block.

**Agent enforcement**: This rule governs how rules are applied. Agents must allow conscious waiver of any rule.

## 8. Cite Your Sources
Create explicit trails back to evidence. Never make claims without citation paths.

**Agent enforcement**: Every bead must include a code citation (`file/path:line`). Beads without citations are invalid.

## 9. Integrity Beats Intelligence
Admit when you haven't solved the problem. Never claim success by changing success criteria.

**Agent enforcement**: Agents must not downgrade severity to avoid creating beads. If it's broken, say it's broken.

## 10. Ship Good Enough + Honest
Ship working solutions that are honest about limitations. Don't let theoretical perfection block practical progress.

**Agent enforcement**: When proposing fixes, prefer the working solution over the perfect one. Tag aspirational improvements as `action:docs` (document the ideal, ship the adequate).

---

## How Agents Use These Rules

Each agent prompt includes: `Reference: loom/agents/rules/GOLDEN_RULES.md`

When creating a bead, agents check:
1. Does this finding relate to a Golden Rule? If yes, tag it: `--labels "rule:<number>"`
2. Does the fix violate a Golden Rule? If yes, tag the waiver: `--labels "waiver:rule-<number>,reason:<why>"`
3. If unsure whether a rule applies, don't tag it — false positives are worse than missed tags

## Rule Compliance Report

After a survey, rosary can generate:
```
Rule Compliance: mache (2026-03-13 survey)
  Rule 1 (no versioned files): PASS — 0 violations
  Rule 2 (200-line limit): 3 violations (engine.go:847, mount.go:734, graphfs.go:412)
  Rule 4 (test reality): 2 violations (staging-agent found 2 mock-only tests)
  Rule 8 (cite sources): PASS — all beads have code citations
  Waivers: 1 (rule-2 waived for mount.go — "single entry point, splitting would fragment mount lifecycle")
```
