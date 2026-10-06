//! Every agent has a name: the one it was given, else a word pair the
//! daemon chose that no other agent on the host has and no branch in the
//! folder's repository has, so a branch can later be named after it.

mod support;

use std::path::Path;
use std::process::Command;

use prost::Message as _;
use support::*;
use wire::AgentSpec;

fn spec_name(install: &Install, agent: &wire::Agent) -> String {
    let bytes = std::fs::read(install.agent_dir(id_of(agent)).join("spec.1")).unwrap();
    AgentSpec::decode(bytes.as_slice()).unwrap().name
}

fn unnamed(cwd: &Path) -> wire::CreateAgentRequest {
    wire::CreateAgentRequest {
        name: None,
        ..create(cwd, "", None)
    }
}

fn git(cwd: &Path, args: &[&str], stdin: Option<&str>) -> String {
    use std::io::Write as _;
    let mut child = Command::new("git")
        .args([
            "-c",
            "user.name=amux",
            "-c",
            "user.email=amux@example.invalid",
        ])
        .args(args)
        .current_dir(cwd)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(stdin.unwrap_or_default().as_bytes())
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success(), "git {args:?}");
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_given_name_is_kept_and_a_missing_one_is_a_word_pair() {
    let install = Install::new();
    let daemon = install
        .start("boot-1", install.launch("idle", vec![]))
        .await;
    let runtime = runtime(&daemon, &install);

    let given = runtime
        .spawn(create(&install.work, "release-notes", None), None)
        .await
        .unwrap();
    assert_eq!(given.name, "release-notes");
    assert_eq!(spec_name(&install, &given), "release-notes");

    let assigned = runtime.spawn(unnamed(&install.work), None).await.unwrap();
    assert!(
        node::word_pairs().any(|pair| pair == assigned.name),
        "{:?} is a word pair",
        assigned.name
    );
    assert_eq!(spec_name(&install, &assigned), assigned.name);
    assert_eq!(
        runtime.agent(id_of(&assigned)).await.unwrap().name,
        assigned.name
    );
    println!("given: {}, assigned: {}", given.name, assigned.name);

    let emptied = runtime.rename(id_of(&assigned), "").await;
    assert!(
        matches!(emptied, Err(node::RegistryError::EmptyName)),
        "a name cannot be taken away: {emptied:?}"
    );

    kill_all(&runtime).await;
    drop(runtime);
    daemon.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn an_assigned_name_avoids_other_agents_and_the_repositorys_branches() {
    let install = Install::new();
    let repo = install.path("repo");
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main"], None);
    git(
        &repo,
        &["commit", "-q", "--allow-empty", "-m", "start"],
        None,
    );
    let head = git(&repo, &["rev-parse", "HEAD"], None);

    // Three pairs stay free of branches: the first is another agent's
    // name, the second a folder a branch sits under; only the third is
    // left to take.
    let pairs: Vec<String> = node::word_pairs().collect();
    let (agent_named, folder, free) = (&pairs[100], &pairs[1000], &pairs[2000]);
    let mut refs: String = pairs
        .iter()
        .filter(|pair| ![agent_named, folder, free].contains(pair))
        .map(|pair| format!("create refs/heads/{pair} {head}\n"))
        .collect();
    refs.push_str(&format!("create refs/heads/{folder}/feature {head}\n"));
    git(&repo, &["update-ref", "--stdin"], Some(&refs));

    let daemon = install
        .start("boot-1", install.launch("idle", vec![]))
        .await;
    let runtime = runtime(&daemon, &install);
    runtime
        .spawn(create(&install.work, agent_named, None), None)
        .await
        .unwrap();

    let assigned = runtime.spawn(unnamed(&repo), None).await.unwrap();
    assert_eq!(
        &assigned.name, free,
        "every other pair is a branch, a branch's folder or another agent's name"
    );

    let numbered = runtime.spawn(unnamed(&repo), None).await.unwrap();
    let (pair, number) = numbered.name.rsplit_once('-').unwrap();
    assert!(
        pairs.iter().any(|known| known == pair) && number == "2",
        "with every pair taken a number follows one: {:?}",
        numbered.name
    );
    println!(
        "{} branches; other agent {agent_named}; assigned {} then {}",
        pairs.len() - 2,
        assigned.name,
        numbered.name
    );

    kill_all(&runtime).await;
    drop(runtime);
    daemon.shutdown().await.unwrap();
}
