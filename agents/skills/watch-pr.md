---
name: watch-pr
description: Poll CI check runs and review status on a PR, resolve addressed review threads, dismiss stale bot reviews, and report merge-readiness. Use after opening or updating a PR that needs monitoring.
---

# Watch PR

Monitor a pull request until it is merge-ready by polling CI and review status, resolving addressed threads, and dismissing stale bot reviews.

## Inputs

The user (or dispatching agent) provides:
- **PR identifier**: a PR number, URL, or `owner/repo#number`

If not provided, detect from the current branch:
```bash
gh pr view --json number,url,headRefName --jq '.number'
```

## Workflow

### 1. Gather PR metadata

```bash
gh pr view <PR> --json number,url,title,headRefName,baseRefName,state,reviewDecision,statusCheckRollup,reviews,reviewThreads
```

Store `headRefName` and `baseRefName` for later diffing.

### 2. Poll loop

Repeat on the configured interval (default: 2 minutes) until merge-ready or timeout (default: 30 minutes):

#### 2a. Check CI status

```bash
gh pr checks <PR> --json name,state,conclusion
```

Classify each check:
- **passing**: conclusion = SUCCESS or NEUTRAL
- **failing**: conclusion = FAILURE or CANCELLED
- **pending**: state = QUEUED or IN_PROGRESS

Report any newly failing or newly passing checks since last poll.

#### 2b. Check review status

```bash
gh pr view <PR> --json reviews,reviewThreads,reviewDecision
```

- Count approvals, changes-requested, and pending reviews.
- List unresolved review threads.

#### 2c. Auto-resolve addressed threads

For each unresolved review thread, check if the file+line was modified in commits after the review:

```bash
gh api repos/{owner}/{repo}/pulls/{pr}/reviews --jq '.[].submitted_at'
gh pr diff <PR> --name-only
```

If the thread's file has commits newer than the review, resolve it:
```bash
gh api graphql -f query='mutation { resolveReviewThread(input: {threadId: "<ID>"}) { thread { isResolved } } }'
```

#### 2d. Dismiss stale bot reviews

If a bot review (e.g., security scanner) requested changes, and fix commits landed after that review:

```bash
gh api repos/{owner}/{repo}/pulls/{pr}/reviews/<review_id>/dismissals -f message="Fix commits landed after this review" -f event="DISMISS"
```

Only dismiss reviews from known bot accounts (author association = BOT, or login matches common patterns like `*[bot]`, `*-bot`).

### 3. Report merge-readiness

When ALL of these are true:
- All required checks pass
- No unresolved review threads
- reviewDecision = APPROVED (or no reviews required)

Report:
```
PR #<number> is merge-ready.
- Checks: <N>/<N> passing
- Reviews: <approvals> approved, 0 changes requested
- Threads: all resolved
```

If timeout is reached without merge-readiness, report the current blockers.

## Exit conditions

- **Merge-ready**: all checks green + reviews resolved + approved → report and exit
- **Timeout**: default 30 minutes → report blockers and exit
- **PR closed/merged**: detected during poll → report and exit
- **Unrecoverable failure**: CI check in terminal failure state with no pending re-runs → report and exit

## Notes

- This skill uses only `gh` CLI — no direct GitHub API tokens needed beyond what `gh auth` provides.
- Thread resolution is conservative: only resolves threads on files that were modified after the review, not all threads.
- Bot dismissal only targets accounts with `[bot]` suffix or BOT association — never dismisses human reviews.
