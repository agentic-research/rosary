use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn provenance_rsry_binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_rsry"))
}

fn run_provenance_git(repo: &Path, args: &[&str]) -> Output {
    Command::new("git")
        .args(args)
        .current_dir(repo)
        .output()
        .expect("run git")
}

fn init_provenance_repo(repo: &Path) {
    let output = run_provenance_git(repo, &["init", "-q", "-b", "main"]);
    assert!(output.status.success(), "{output:?}");
}

fn run_provenance_rsry(repo: &Path, args: &[&str]) -> Output {
    Command::new(provenance_rsry_binary())
        .args(args)
        .current_dir(repo)
        .env("NO_COLOR", "1")
        .output()
        .expect("run rsry")
}

fn run_provenance_rsry_with_home(repo: &Path, home: &Path, args: &[&str]) -> Output {
    Command::new(provenance_rsry_binary())
        .args(args)
        .current_dir(repo)
        .env("HOME", home)
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env("NO_COLOR", "1")
        .output()
        .expect("run rsry")
}

fn assert_provenance_success(context: &str, output: &Output) {
    assert!(
        output.status.success(),
        "{context} failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn hooks_status_distinguishes_current_and_stale_template_provenance() {
    let temp = tempfile::tempdir().unwrap();
    let repo = temp.path();
    init_provenance_repo(repo);

    let install = run_provenance_rsry(repo, &["hooks", "--repo", ".", "install"]);
    assert_provenance_success("hooks install", &install);

    for name in ["post-push", "post-merge", "pre-commit", "commit-msg"] {
        let installed = std::fs::read_to_string(repo.join(".git/hooks").join(name)).unwrap();
        assert!(
            !installed.contains(provenance_rsry_binary().to_string_lossy().as_ref()),
            "{name} must not pin the ephemeral test/build binary path:\n{installed}"
        );
        let prefix = format!("# rsry-hook {name} v{} sha256:", env!("CARGO_PKG_VERSION"));
        let stamp = installed
            .lines()
            .find(|line| line.starts_with(&prefix))
            .unwrap_or_else(|| {
                panic!("installed hook must disclose version + digest:\n{installed}")
            });
        let digest = stamp.strip_prefix(&prefix).unwrap();
        assert_eq!(digest.len(), 64, "{name} must carry a SHA-256 digest");
        assert!(
            digest.bytes().all(|byte| byte.is_ascii_hexdigit()),
            "{name} digest must be hexadecimal: {digest}"
        );
    }

    let current = run_provenance_rsry(repo, &["hooks", "--repo", ".", "status"]);
    assert_provenance_success("current hooks status", &current);
    let current_stdout = String::from_utf8_lossy(&current.stdout);
    assert!(
        current_stdout.contains("✓ current") && current_stdout.contains("pre-commit"),
        "current hook should be identified as current:\n{current_stdout}"
    );

    let hook = repo.join(".git/hooks/pre-commit");
    let installed = std::fs::read_to_string(&hook).unwrap();
    let stale = installed.replacen(
        &format!("# rsry-hook pre-commit v{}", env!("CARGO_PKG_VERSION")),
        "# rsry-hook pre-commit v0.0.0",
        1,
    );
    std::fs::write(&hook, stale).unwrap();

    let status = run_provenance_rsry(repo, &["hooks", "--repo", ".", "status"]);
    assert_provenance_success("stale hooks status", &status);
    let stdout = String::from_utf8_lossy(&status.stdout);
    assert!(
        stdout.contains("STALE") && stdout.contains("pre-commit"),
        "stale hook must be reported loudly:\n{stdout}"
    );
    assert!(
        stdout.contains(env!("CARGO_PKG_VERSION")),
        "status should disclose the expected installed-binary version:\n{stdout}"
    );
}

#[cfg(unix)]
#[test]
fn installed_hook_warns_when_runtime_rsry_version_differs() {
    use std::os::unix::fs::PermissionsExt;

    let temp = tempfile::tempdir().unwrap();
    let repo = temp.path().join("repo");
    std::fs::create_dir(&repo).unwrap();
    init_provenance_repo(&repo);

    let install = run_provenance_rsry(&repo, &["hooks", "--repo", ".", "install"]);
    assert_provenance_success("hooks install", &install);

    let fake_rsry = temp.path().join("rsry");
    std::fs::write(
        &fake_rsry,
        "#!/bin/sh\nif [ \"$1\" = \"--version\" ]; then echo 'rsry 99.0.0'; fi\nexit 0\n",
    )
    .unwrap();
    let mut permissions = std::fs::metadata(&fake_rsry).unwrap().permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&fake_rsry, permissions).unwrap();

    let output = Command::new(repo.join(".git/hooks/post-merge"))
        .current_dir(&repo)
        .env("RSRY_BIN", &fake_rsry)
        .output()
        .expect("run installed post-merge hook");
    assert!(output.status.success(), "{output:?}");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains(&format!(
            "rsry hook drift: hook v{}, runtime v99.0.0",
            env!("CARGO_PKG_VERSION")
        )),
        "version mismatch must be visible:\n{stderr}"
    );
}

#[test]
fn installing_custom_hooks_path_neutralizes_dormant_managed_standard_hook() {
    let temp = tempfile::tempdir().unwrap();
    let repo = temp.path();
    init_provenance_repo(repo);

    let standard_hook = repo.join(".git/hooks/pre-commit");
    std::fs::write(
        &standard_hook,
        "#!/bin/sh\necho user-before\n\
         # >>> rsry-managed (do not edit between these markers; `rsry hooks install` regenerates) >>>\n\
         echo stale-dangerous-rsry-block\n\
         # <<< rsry-managed <<<\n\
         echo user-after\n",
    )
    .unwrap();
    let config = run_provenance_git(repo, &["config", "core.hooksPath", ".rsry-hooks"]);
    assert!(config.status.success(), "{config:?}");

    let install = run_provenance_rsry(repo, &["hooks", "--repo", ".", "install"]);
    assert_provenance_success("custom hooks install", &install);

    let active = std::fs::read_to_string(repo.join(".rsry-hooks/pre-commit")).unwrap();
    assert!(
        active.contains("# rsry-hook pre-commit v"),
        "active hook must have provenance:\n{active}"
    );

    let dormant = std::fs::read_to_string(&standard_hook).unwrap();
    assert!(dormant.contains("echo user-before"));
    assert!(dormant.contains("echo user-after"));
    assert!(
        !dormant.contains("stale-dangerous-rsry-block") && !dormant.contains(">>> rsry-managed"),
        "inactive standard hook must not retain a dormant rsry block:\n{dormant}"
    );
}

#[test]
fn task_install_refreshes_compiled_hook_templates() {
    let taskfile =
        std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("Taskfile.yml"))
            .unwrap();
    assert!(
        taskfile.contains("- ~/.local/bin/rsry hooks --repo . install"),
        "`task install` must refresh hooks using the binary it just installed"
    );
}

#[cfg(unix)]
#[test]
fn pre_commit_managed_block_runs_before_framework_exec() {
    use std::os::unix::fs::PermissionsExt;

    let temp = tempfile::tempdir().unwrap();
    let repo = temp.path().join("repo");
    std::fs::create_dir(&repo).unwrap();
    init_provenance_repo(&repo);

    let framework = temp.path().join("framework");
    let framework_ran = temp.path().join("framework-ran");
    std::fs::write(
        &framework,
        format!("#!/bin/sh\ntouch '{}'\n", framework_ran.display()),
    )
    .unwrap();
    std::fs::set_permissions(&framework, std::fs::Permissions::from_mode(0o755)).unwrap();

    let hook = repo.join(".git/hooks/pre-commit");
    std::fs::write(
        &hook,
        format!(
            "#!/bin/sh\nINSTALL_PYTHON='{}'\nif [ -x \"$INSTALL_PYTHON\" ]; then\n\
             exec \"$INSTALL_PYTHON\" -mpre_commit \"$@\"\nelif command -v pre-commit >/dev/null; then\n\
             exec pre-commit \"$@\"\nfi\n",
            framework.display()
        ),
    )
    .unwrap();

    std::fs::create_dir(repo.join(".beads")).unwrap();
    std::fs::write(repo.join(".beads/beads.jsonl"), "").unwrap();
    assert!(
        run_provenance_git(&repo, &["config", "user.email", "test@example.com"])
            .status
            .success()
    );
    assert!(
        run_provenance_git(&repo, &["config", "user.name", "Test User"])
            .status
            .success()
    );
    assert!(
        run_provenance_git(&repo, &["add", ".beads/beads.jsonl"])
            .status
            .success()
    );
    assert!(
        run_provenance_git(
            &repo,
            &[
                "-c",
                "commit.gpgsign=false",
                "commit",
                "--no-verify",
                "-qm",
                "opt in",
            ],
        )
        .status
        .success()
    );

    let install = run_provenance_rsry(&repo, &["hooks", "--repo", ".", "install"]);
    assert_provenance_success("hooks install", &install);

    let rsry_ran = temp.path().join("rsry-ran");
    let fake_rsry = temp.path().join("rsry");
    std::fs::write(
        &fake_rsry,
        format!(
            "#!/bin/sh\nif [ \"$1\" = \"--version\" ]; then echo 'rsry {}'; exit 0; fi\n\
             touch '{}'\nwhile [ \"$#\" -gt 0 ]; do\n\
             if [ \"$1\" = \"-o\" ]; then shift; : > \"$1\"; fi\nshift\ndone\n",
            env!("CARGO_PKG_VERSION"),
            rsry_ran.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&fake_rsry, std::fs::Permissions::from_mode(0o755)).unwrap();

    let output = Command::new(&hook)
        .current_dir(&repo)
        .env("RSRY_BIN", &fake_rsry)
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    assert!(rsry_ran.exists(), "rsry export block was unreachable");
    assert!(framework_ran.exists(), "pre-commit framework did not run");

    std::fs::remove_file(&rsry_ran).unwrap();
    std::fs::remove_file(&framework_ran).unwrap();
    let linked = temp.path().join("linked");
    let linked_output = run_provenance_git(
        &repo,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "linked-test",
            linked.to_str().unwrap(),
            "HEAD",
        ],
    );
    assert!(linked_output.status.success(), "{linked_output:?}");

    let linked_hook = Command::new(&hook)
        .current_dir(&linked)
        .env("RSRY_BIN", &fake_rsry)
        .output()
        .unwrap();
    assert!(linked_hook.status.success(), "{linked_hook:?}");
    assert!(
        !rsry_ran.exists(),
        "linked worktree must skip the rsry export"
    );
    assert!(
        framework_ran.exists(),
        "linked-worktree skip must continue into the framework hook"
    );
}

#[test]
fn hooks_status_rejects_pre_commit_block_after_exec() {
    let temp = tempfile::tempdir().unwrap();
    let repo = temp.path();
    init_provenance_repo(repo);

    let install = run_provenance_rsry(repo, &["hooks", "--repo", ".", "install"]);
    assert_provenance_success("hooks install", &install);

    let hook = repo.join(".git/hooks/pre-commit");
    let installed = std::fs::read_to_string(&hook).unwrap();
    let start = installed.find("# >>> rsry-managed").unwrap();
    let end = installed.find("# <<< rsry-managed <<<").unwrap() + "# <<< rsry-managed <<<".len();
    let block = &installed[start..end];
    std::fs::write(
        &hook,
        format!("#!/bin/sh\nexec pre-commit \"$@\"\n\n{block}\n"),
    )
    .unwrap();

    let status = run_provenance_rsry(repo, &["hooks", "--repo", ".", "status"]);
    assert_provenance_success("hooks status", &status);
    let stdout = String::from_utf8_lossy(&status.stdout);
    assert!(
        stdout.contains("UNREACHABLE") && stdout.contains("pre-commit"),
        "unreachable managed block must not report current:\n{stdout}"
    );
}

#[test]
fn hooks_all_installs_every_globally_registered_repo() {
    let temp = tempfile::tempdir().unwrap();
    let repo_a = temp.path().join("alpha");
    let repo_b = temp.path().join("beta");
    std::fs::create_dir(&repo_a).unwrap();
    std::fs::create_dir(&repo_b).unwrap();
    init_provenance_repo(&repo_a);
    init_provenance_repo(&repo_b);

    let home = temp.path().join("home");
    std::fs::create_dir_all(home.join(".rsry")).unwrap();
    std::fs::write(
        home.join(".rsry/config.toml"),
        format!(
            "[[repo]]\nname = \"alpha\"\npath = \"{}\"\n\n\
             [[repo]]\nname = \"beta\"\npath = \"{}\"\n",
            repo_a.display(),
            repo_b.display()
        ),
    )
    .unwrap();

    let install = run_provenance_rsry_with_home(temp.path(), &home, &["hooks", "--all", "install"]);
    assert_provenance_success("hooks --all install", &install);
    for repo in [&repo_a, &repo_b] {
        assert!(
            repo.join(".git/hooks/pre-commit").exists(),
            "{} was not refreshed",
            repo.display()
        );
    }

    let status = run_provenance_rsry_with_home(temp.path(), &home, &["hooks", "--all", "status"]);
    assert_provenance_success("hooks --all status", &status);
    let stdout = String::from_utf8_lossy(&status.stdout);
    assert!(
        stdout.contains("alpha") && stdout.contains("beta"),
        "{stdout}"
    );
}
