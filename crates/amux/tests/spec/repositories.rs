//! Host-owned project lists cross the same authenticated links as agent RPCs.

use std::path::{Path, PathBuf};

use amux::testnet::{TestNet, Via};
use amux::{ListRepositoriesRequest, ListRepositoriesResponse};

fn checkout(root: &Path, name: &str) -> PathBuf {
    let path = root.join(name);
    std::fs::create_dir_all(path.join(".git")).unwrap();
    path.canonicalize().unwrap()
}

fn paths(response: &ListRepositoriesResponse) -> Vec<PathBuf> {
    response
        .recent
        .iter()
        .chain(&response.repositories)
        .map(|entry| entry.path.clone())
        .collect()
}

/// A cloud-only viewer receives only repositories in the host's declared roots,
/// plus directories the host actually used to create agents. Search and a shared
/// result cap apply at the host; recents survive deleting agents and restarting.
#[tokio::test]
async fn repositories_through_relay_are_scoped_searchable_and_keep_recent_projects() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("declared");
    let alpha = checkout(&root, "alpha");
    let beta = checkout(&root, "group/beta");
    let undeclared = checkout(temp.path(), "undeclared");
    let typed = temp.path().join("typed-project");
    std::fs::create_dir(&typed).unwrap();
    let typed = typed.canonicalize().unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink(&undeclared, root.join("escape")).unwrap();

    let net = TestNet::builder()
        .cloud()
        .daemon("viewer")
        .cloud_only()
        .daemon("host")
        .cloud_only()
        .repository_roots(vec![root.clone(), root.clone()])
        .daemon("stranger")
        .cloud_only()
        .paired("viewer", "host", Via::Cloud)
        .start()
        .await;
    let [viewer, host, stranger] = net.daemons(["viewer", "host", "stranger"]);
    viewer.can_call(&host).await;
    let client = viewer.admin_client().await;
    let request = ListRepositoriesRequest {
        host: host.host_id(),
        query: None,
        limit: 20,
    };
    let initial = client.list_repositories(request.clone()).await.unwrap();
    assert_eq!(paths(&initial), vec![alpha.clone(), beta.clone()]);
    assert_eq!(initial.roots, vec![root.canonicalize().unwrap()]);
    assert!(initial.recent.is_empty());
    assert!(
        initial
            .repositories
            .iter()
            .all(|entry| entry.last_used.is_none())
    );
    assert!(!paths(&initial).contains(&undeclared));
    println!(
        "repositories relay initial: {}",
        serde_json::to_string(&initial).unwrap()
    );

    let first = host.spawn_echo_agent_in("first", &alpha).await;
    let second = host.spawn_echo_agent_in("second", &typed).await;
    host.spawn_echo_agent_in("third", &alpha).await;
    let recent = client.list_repositories(request.clone()).await.unwrap();
    assert_eq!(
        recent
            .recent
            .iter()
            .map(|entry| &entry.path)
            .collect::<Vec<_>>(),
        vec![&alpha, &typed]
    );
    assert_eq!(
        recent
            .repositories
            .iter()
            .map(|entry| &entry.path)
            .collect::<Vec<_>>(),
        vec![&beta]
    );
    assert!(recent.recent.iter().all(|entry| entry.last_used.is_some()));
    let filtered = client
        .list_repositories(ListRepositoriesRequest {
            query: Some("BETA".into()),
            ..request.clone()
        })
        .await
        .unwrap();
    assert_eq!(paths(&filtered), vec![beta.clone()]);
    let filtered_recent = client
        .list_repositories(ListRepositoriesRequest {
            query: Some("TYPED-PROJECT".into()),
            ..request.clone()
        })
        .await
        .unwrap();
    assert_eq!(paths(&filtered_recent), vec![typed.clone()]);
    for limit in 0..=3 {
        let result = client
            .list_repositories(ListRepositoriesRequest {
                limit,
                ..request.clone()
            })
            .await
            .unwrap();
        assert_eq!(paths(&result).len(), limit as usize);
        assert_eq!(paths(&result), paths(&recent)[..limit as usize]);
    }
    let no_matches = client
        .list_repositories(ListRepositoriesRequest {
            query: Some("absent-project".into()),
            ..request.clone()
        })
        .await
        .unwrap();
    assert!(paths(&no_matches).is_empty());

    let admin = host.admin_client().await;
    admin.delete_agent(first.id).await.unwrap();
    admin.delete_agent(second.id).await.unwrap();
    net.restart_daemon(&host).await;
    let restored = client.list_repositories(request.clone()).await.unwrap();
    assert_eq!(restored, recent);
    println!(
        "repositories relay after delete and restart: {}",
        serde_json::to_string(&restored).unwrap()
    );
    assert!(
        stranger
            .admin_client()
            .await
            .list_repositories(request)
            .await
            .is_err()
    );
}

/// The same request works locally and over direct mTLS; a .git file marks a
/// worktree and an unconfigured host exposes no directories by default.
#[tokio::test]
async fn repositories_use_direct_and_local_links_and_default_to_no_roots() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("worktree");
    std::fs::create_dir(&root).unwrap();
    std::fs::write(root.join(".git"), "gitdir: /unused/in/this/discovery/test").unwrap();
    let net = TestNet::builder()
        .daemon("viewer")
        .daemon("host")
        .repository_roots(vec![root.clone()])
        .paired("viewer", "host", Via::Tcp)
        .start()
        .await;
    let [viewer, host] = net.daemons(["viewer", "host"]);
    viewer.can_call(&host).await;
    let request = ListRepositoriesRequest {
        host: host.host_id(),
        query: None,
        limit: 10,
    };
    let remote = viewer
        .admin_client()
        .await
        .list_repositories(request.clone())
        .await
        .unwrap();
    let local = host
        .admin_client()
        .await
        .list_repositories(request)
        .await
        .unwrap();
    assert_eq!(remote, local);
    assert_eq!(paths(&remote), vec![root.canonicalize().unwrap()]);
    let empty = viewer
        .admin_client()
        .await
        .list_repositories(ListRepositoriesRequest {
            host: viewer.host_id(),
            query: None,
            limit: u32::MAX,
        })
        .await
        .unwrap();
    assert!(empty.roots.is_empty());
    assert!(paths(&empty).is_empty());
    println!(
        "repositories direct: {}",
        serde_json::to_string(&remote).unwrap()
    );
}
