//! The pre-push template: the `bead verify-pushed` ref-blob gate (rosary-e5c037).
//!
//! The gate's logic (range → named ids → pushed blob vs store rendering) is
//! unit-tested in `publish::push` and driven end to end against the real
//! binary in `tests/hooks_prepush_ref_blob.rs`. What the template itself owes
//! is proven here with a stub `rsry`: the guards that skip the block, that
//! git's stdin reaches the primitive intact, and that its verdict is the
//! push's verdict.

use super::*;
use std::io::Write as _;
use std::process::{Command, Stdio};

/// Same opt-in-by-tracking guard on the pre-push gate (rosary-9c0e6c) —
/// proven the same no-binary-needed way as pre-commit's equivalent test.
#[test]
fn hooks_run_prepush_is_a_noop_when_jsonl_not_tracked() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    init_repo(root);
    seed_commit(root);
    run(root, "pre-push").unwrap();
}

/// Same worktree guard as pre-commit's rosary-599778 fix, on the pre-push
/// hook: a linked worktree's on-demand-created empty store must not be
/// consulted (it would report every named bead as "missing" and block an
/// unrelated push).
#[test]
fn hooks_run_prepush_is_a_noop_in_a_worktree() {
    let tmp = tempfile::tempdir().unwrap();
    let main = tmp.path().join("main");
    std::fs::create_dir_all(&main).unwrap();
    init_repo(&main);
    seed_commit(&main);
    std::fs::create_dir_all(main.join(".beads")).unwrap();
    std::fs::write(main.join(".beads/beads.jsonl"), "").unwrap();
    git(&main, &["add", ".beads/beads.jsonl"]);
    git(&main, &["commit", "-q", "-m", "track beads.jsonl"]);

    let wt = tmp.path().join("wt");
    assert!(
        git(
            &main,
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                "wt-prepush",
                wt.to_str().unwrap(),
            ],
        )
        .status
        .success()
    );

    run(&wt, "pre-push").unwrap();
}

/// A stub `rsry` whose `bead verify-pushed` copies its stdin to `$0.stdin`,
/// prints `message` to stderr, and exits `code`. Every other invocation
/// exits 0, so the template's version probe (if any) stays inert.
fn fake_verify_pushed(dir: &Path, code: i32, message: &str) -> PathBuf {
    let path = dir.join("fake-rsry");
    std::fs::write(dir.join("fake-rsry.msg"), message).unwrap();
    std::fs::write(
        &path,
        format!(
            "#!/bin/sh\n\
             case \"$*\" in\n\
             \x20 *verify-pushed*) cat > \"$0.stdin\"; cat \"$0.msg\" >&2; exit {code} ;;\n\
             esac\n\
             exit 0\n"
        ),
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    path
}

/// A canonical checkout with a tracked (empty) export — the shape in which
/// the managed block actually runs the primitive.
fn tracked_repo() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    init_repo(root);
    seed_commit(root);
    std::fs::create_dir_all(root.join(".beads")).unwrap();
    std::fs::write(root.join(".beads/beads.jsonl"), "").unwrap();
    git(root, &["add", ".beads/beads.jsonl"]);
    git(root, &["commit", "-q", "-m", "track beads.jsonl"]);
    tmp
}

/// Run the pre-push template with `stdin` on its stdin, the way git does.
fn run_prepush_with_stdin(root: &Path, rsry: &Path, stdin: &str) -> std::process::Output {
    let (_, block) = HOOKS
        .iter()
        .find(|(n, _)| *n == "pre-push")
        .expect("pre-push is registered");
    let mut child = Command::new("sh")
        .arg("-c")
        .arg(render_block(block))
        .current_dir(root)
        .env("RSRY_BIN", rsry)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn hook");
    child
        .stdin
        .take()
        .unwrap()
        .write_all(stdin.as_bytes())
        .unwrap();
    child.wait_with_output().expect("hook output")
}

const REF_LINE: &str = "refs/heads/feature 1111111111111111111111111111111111111111 refs/heads/feature 0000000000000000000000000000000000000000\n";

/// The gate's whole purpose, pinned at the template level: the primitive's
/// refusal IS the push's refusal, and its diagnosis (the ids) reaches the
/// operator. Without this the block could swallow the exit status and stay
/// green.
#[test]
fn hooks_run_prepush_refuses_the_push_when_verify_pushed_refuses() {
    let tmp = tracked_repo();
    let rsry = fake_verify_pushed(
        tmp.path(),
        1,
        "stale rosary-abc123\n  fix: rsry bead publish rosary-abc123\n",
    );
    let out = run_prepush_with_stdin(tmp.path(), &rsry, REF_LINE);

    assert!(
        !out.status.success(),
        "a refusing verify-pushed must refuse the push, got {:?}",
        out.status
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("rosary-abc123") && stderr.contains("push refused"),
        "refusal must carry the primitive's ids and name the problem, got: {stderr}"
    );
}

/// The other direction — together with the refusal test this proves the exit
/// status tracks the verdict rather than being constant.
#[test]
fn hooks_run_prepush_allows_the_push_when_verify_pushed_passes() {
    let tmp = tracked_repo();
    let rsry = fake_verify_pushed(tmp.path(), 0, "");
    let out = run_prepush_with_stdin(tmp.path(), &rsry, REF_LINE);

    assert!(
        out.status.success(),
        "a passing verify-pushed must not block the push, stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// The primitive can only gate the pushed ref if it sees git's ref list:
/// nothing in the template may consume or reshape stdin before it.
#[test]
fn hooks_run_prepush_forwards_gits_stdin_to_verify_pushed_verbatim() {
    let tmp = tracked_repo();
    let rsry = fake_verify_pushed(tmp.path(), 0, "");
    let out = run_prepush_with_stdin(tmp.path(), &rsry, REF_LINE);
    assert!(out.status.success(), "{:?}", out);

    let seen = std::fs::read_to_string(tmp.path().join("fake-rsry.stdin")).unwrap();
    assert_eq!(seen, REF_LINE);
}

/// A hook newer than the binary running it: an `rsry` that predates
/// `bead verify-pushed` fails with clap's usage exit (2). That is "the check
/// cannot run", not "the check refused" — the push proceeds with a warning,
/// as the previous gate did when its export failed, instead of blocking
/// every push until the upgrade. Exit 1 (a refusal) still blocks, above.
#[test]
fn hooks_run_prepush_warns_and_allows_when_the_binary_cannot_run_the_check() {
    let tmp = tracked_repo();
    let rsry = fake_verify_pushed(
        tmp.path(),
        2,
        "error: unrecognized subcommand 'verify-pushed'\n",
    );
    let out = run_prepush_with_stdin(tmp.path(), &rsry, REF_LINE);

    assert!(
        out.status.success(),
        "a binary that cannot run the check must not block the push, stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("cannot run 'bead verify-pushed'"),
        "the skip must be loud, got: {stderr}"
    );
}
