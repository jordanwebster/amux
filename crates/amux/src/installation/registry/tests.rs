use super::*;
use crate::installation::ProfilePaths;
use crate::installation::paths::validate_socket_path;

fn root() -> tempfile::TempDir {
    // macOS's default temp directory consumes most of sockaddr_un before a UUID.
    tempfile::Builder::new()
        .prefix("ar-")
        .tempdir_in("/tmp")
        .unwrap()
}

fn open(path: &Path) -> Registry {
    Registry::open(InstallationRoot::OnDisk(path.to_owned())).unwrap()
}

fn label(name: &str) -> ProfileLabel {
    ProfileLabel {
        override_name: Some(name.into()),
        ..Default::default()
    }
}

#[test]
fn root_lock_is_exclusive_and_released_on_drop() {
    let root = root();
    let registry = open(root.path());
    assert!(matches!(
        Registry::open(InstallationRoot::OnDisk(root.path().join("."))),
        Err(InstallationError::RootBusy(_))
    ));
    let other = root.path().join("other");
    let independent = open(&other);
    drop(registry);
    let reopened = open(root.path());
    assert!(root.path().join("lock").is_file());
    assert_ne!(independent.path(), reopened.path());
    println!("second open: RootBusy; separate root: open; owner drop: root reopens");
}

#[cfg(unix)]
#[test]
fn root_alias_cannot_bypass_lock_and_lock_symlinks_are_refused() {
    use std::os::unix::fs::symlink;
    let temporary = root();
    let actual = temporary.path().join("actual");
    let _owner = open(&actual);
    let alias = temporary.path().join("alias");
    symlink(&actual, &alias).unwrap();
    assert!(matches!(
        Registry::open(InstallationRoot::OnDisk(alias)),
        Err(InstallationError::RootBusy(_))
    ));
    let other = temporary.path().join("other");
    fs::create_dir(&other).unwrap();
    symlink(actual.join("lock"), other.join("lock")).unwrap();
    assert!(matches!(
        Registry::open(InstallationRoot::OnDisk(other)),
        Err(InstallationError::InvalidPath(_))
    ));
}

#[test]
fn persisted_records_round_trip_with_independent_revisions() {
    let root = root();
    let mut registry = open(root.path());
    let a = registry
        .create(ProfileId::new(), label("Personal"))
        .unwrap();
    let b = registry.create(ProfileId::new(), label("Work")).unwrap();
    let mut changed = a.clone();
    changed.label.account_name = Some("Alice".into());
    changed.label.email = Some("alice@example.test".into());
    changed.binding = Some(Binding {
        account: AccountId {
            service: "https://cloud.example.test".into(),
            subject: "opaque-subject".into(),
        },
        bound_at: DateTime::from_timestamp(1_700_000_000, 0).unwrap(),
    });
    changed.paused = true;
    let changed = registry.replace(changed).unwrap();
    assert_eq!(changed.revision, 2);
    assert_eq!(registry.get(b.id).unwrap(), &b);
    assert!(matches!(
        registry.replace(a.clone()),
        Err(InstallationError::RevisionMismatch {
            expected: 1,
            actual: 2
        })
    ));
    assert!(matches!(
        registry.remove(a.id, 1),
        Err(InstallationError::RevisionMismatch { .. })
    ));
    assert!(registry.create(a.id, label("replacement")).is_err());
    println!(
        "registry.yaml after binding and pause:\n{}",
        fs::read_to_string(root.path().join("registry.yaml")).unwrap()
    );
    drop(registry);
    let mut reopened = open(root.path());
    assert_eq!(reopened.get(a.id).unwrap(), &changed);
    assert_eq!(reopened.get(b.id).unwrap(), &b);
    reopened.remove(b.id, b.revision).unwrap();
    drop(reopened);
    let reopened = open(root.path());
    assert_eq!(reopened.profiles().count(), 1);
    assert!(matches!(
        reopened.get(b.id),
        Err(InstallationError::UnknownProfile(_))
    ));
}

#[test]
fn in_memory_registries_are_independent() {
    let mut a = Registry::open(InstallationRoot::InMemory).unwrap();
    let b = Registry::open(InstallationRoot::InMemory).unwrap();
    a.create(ProfileId::new(), label("Personal")).unwrap();
    assert!(a.path().is_none());
    assert_eq!(a.profiles().count(), 1);
    assert_eq!(b.profiles().count(), 0);
}

#[test]
fn atomic_replacement_keeps_old_readers_whole() {
    use std::io::Read;
    let root = root();
    let mut registry = open(root.path());
    let mut record = registry.create(ProfileId::new(), label("Before")).unwrap();
    let path = root.path().join("registry.yaml");
    let original = fs::read_to_string(&path).unwrap();
    let mut old_reader = File::open(&path).unwrap();
    record.label = label("After");
    registry.replace(record).unwrap();
    let mut old_bytes = String::new();
    old_reader.read_to_string(&mut old_bytes).unwrap();
    assert_eq!(old_bytes, original);
    let replacement = fs::read_to_string(path).unwrap();
    assert_ne!(replacement, original);
    let replacement: RegistryFile = serde_yaml::from_str(&replacement).unwrap();
    assert_eq!(replacement.profiles[0].label, label("After"));
    assert_eq!(replacement.profiles[0].revision, 2);
    assert_eq!(fs::read_dir(root.path()).unwrap().count(), 2);
    println!(
        "atomic replace: an open reader keeps the complete old registry; a new reader sees revision 2"
    );
}

#[test]
fn failed_persist_preserves_memory_and_cleans_staging_file() {
    let root = root();
    let mut registry = open(root.path());
    let record = registry.create(ProfileId::new(), label("Before")).unwrap();
    let path = root.path().join("registry.yaml");
    let before = fs::read(&path).unwrap();
    let backup = root.path().join("saved.yaml");
    fs::rename(&path, &backup).unwrap();
    fs::create_dir(&path).unwrap();
    let mut candidate = record.clone();
    candidate.paused = true;
    assert!(matches!(
        registry.replace(candidate),
        Err(InstallationError::Io(_))
    ));
    assert_eq!(registry.get(record.id).unwrap(), &record);
    assert_eq!(fs::read(&backup).unwrap(), before);
    assert_eq!(fs::read_dir(root.path()).unwrap().count(), 3);
    fs::remove_dir(&path).unwrap();
    fs::rename(&backup, &path).unwrap();
    drop(registry);
    assert_eq!(open(root.path()).get(record.id).unwrap(), &record);
}

#[test]
fn invalid_registry_is_never_silently_reset() {
    let root = root();
    let path = root.path().join("registry.yaml");
    let record = ProfileRecord {
        id: ProfileId::new(),
        label: label("Personal"),
        binding: None,
        paused: false,
        revision: 1,
    };
    let duplicate = serde_yaml::to_string(&RegistryFile {
        deleting: Default::default(),
        profiles: vec![record.clone(), record.clone()],
    })
    .unwrap();
    let mut zero = record;
    zero.revision = 0;
    let zero = serde_yaml::to_string(&RegistryFile {
        deleting: Default::default(),
        profiles: vec![zero],
    })
    .unwrap();
    for invalid in [
        "profiles: [".to_owned(),
        "profiles: []\nunknown: true\n".to_owned(),
        duplicate,
        zero,
    ] {
        fs::write(&path, &invalid).unwrap();
        assert!(matches!(
            Registry::open(InstallationRoot::OnDisk(root.path().to_owned())),
            Err(InstallationError::Registry(_))
        ));
        assert_eq!(fs::read_to_string(&path).unwrap(), invalid);
    }
    fs::remove_file(path).unwrap();
    open(root.path()); // Failed loads must release their lock too.
}

#[test]
fn revision_exhaustion_does_not_wrap() {
    let root = root();
    let record = ProfileRecord {
        id: ProfileId::new(),
        label: label("Personal"),
        binding: None,
        paused: false,
        revision: u64::MAX,
    };
    let bytes = serde_yaml::to_string(&RegistryFile {
        deleting: Default::default(),
        profiles: vec![record.clone()],
    })
    .unwrap();
    fs::write(root.path().join("registry.yaml"), &bytes).unwrap();
    let mut registry = open(root.path());
    assert!(matches!(
        registry.replace(record.clone()),
        Err(InstallationError::Registry(_))
    ));
    assert_eq!(registry.get(record.id).unwrap(), &record);
    assert_eq!(
        fs::read_to_string(root.path().join("registry.yaml")).unwrap(),
        bytes
    );
}

#[cfg(unix)]
#[test]
fn profile_owned_paths_are_disjoint_and_directories_are_private() {
    use std::os::unix::fs::PermissionsExt;
    let root = root();
    let mut registry = open(root.path());
    let root_path = registry.path().unwrap().to_owned();
    let a = registry
        .create(ProfileId::new(), label("same label"))
        .unwrap();
    let b = registry
        .create(ProfileId::new(), label("same label"))
        .unwrap();
    let a_paths = ProfilePaths::for_id(&root_path, a.id).unwrap();
    let b_paths = ProfilePaths::for_id(&root_path, b.id).unwrap();
    let shared = ["keymaps", "logs", "reports", "update"];
    for name in shared {
        private_directory(&root_path.join(name)).unwrap();
        fs::write(root_path.join(name).join("sentinel"), name).unwrap();
    }
    let owned_files = |paths: &ProfilePaths| {
        vec![
            paths.config_path.clone().unwrap(),
            paths.credentials_path().unwrap(),
            paths.socket_path.clone(),
            crate::installation::adjacent_codex_socket_path(&paths.socket_path),
            paths.state_path.clone(),
            paths.state_path.with_file_name("suspended.yaml"),
            paths.data_dir.join("device.key"),
            paths.data_dir.join("host_id"),
            paths.data_dir.join("trust.json"),
            paths.data_dir.join("agents/agent"),
            paths.data_dir.join("cache/artifacts/artifact"),
            paths.reports_dir.join("report"),
        ]
    };
    let a_files = owned_files(&a_paths);
    let b_files = owned_files(&b_paths);
    for (id, paths, contents) in [(a.id, &a_files, "personal"), (b.id, &b_files, "work")] {
        let directory = root_path.join("profiles").join(id.to_string());
        for path in paths {
            assert!(path.starts_with(&directory));
            for shared in shared {
                assert!(!path.starts_with(root_path.join(shared)));
            }
            fs::write(path, contents).unwrap();
        }
        for suffix in [
            "",
            "state",
            "data",
            "data/agents",
            "data/cache",
            "data/cache/artifacts",
            "data/reports",
        ] {
            let dir = directory.join(suffix);
            assert_eq!(
                fs::metadata(&dir).unwrap().permissions().mode() & 0o777,
                0o700
            );
        }
        println!("profile {id}: {} (directories 0700)", directory.display());
        for path in paths {
            println!("  {}", path.strip_prefix(&directory).unwrap().display());
        }
    }
    for path in &a_files {
        assert_eq!(fs::read_to_string(path).unwrap(), "personal");
        for other in &b_files {
            assert!(!path.starts_with(other) && !other.starts_with(path));
        }
    }
    for path in &b_files {
        assert_eq!(fs::read_to_string(path).unwrap(), "work");
    }
    for name in shared {
        assert_eq!(
            fs::read_to_string(root_path.join(name).join("sentinel")).unwrap(),
            name
        );
    }
    for name in ["lock", "registry.yaml"] {
        assert_eq!(
            fs::metadata(root_path.join(name))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }
    assert_eq!(
        fs::metadata(&root_path).unwrap().permissions().mode() & 0o777,
        0o700
    );
    assert_eq!(
        fs::metadata(root_path.join("profiles"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
}

#[cfg(unix)]
#[test]
fn symlinks_cannot_redirect_profile_or_registry_writes() {
    use std::os::unix::fs::symlink;
    for suffix in [
        "",
        "data",
        "data/cache",
        "data/cache/artifacts",
        "state",
        "data/reports",
        "credentials.yaml",
        "config.yaml",
        "data/trust.json",
        "amux.sock",
    ] {
        let root = root();
        let registry = open(root.path());
        let id = ProfileId::new();
        let paths = ProfilePaths::for_id(registry.path().unwrap(), id).unwrap();
        let directory = paths.config_path.unwrap().parent().unwrap().to_owned();
        let alias = if suffix.is_empty() {
            directory
        } else {
            directory.join(suffix)
        };
        if alias.is_dir() {
            fs::remove_dir_all(&alias).unwrap();
        }
        let shared = root.path().join("keymaps");
        fs::create_dir(&shared).unwrap();
        fs::write(shared.join("sentinel"), "shared").unwrap();
        symlink(&shared, &alias).unwrap();
        assert!(
            matches!(
                ProfilePaths::for_id(registry.path().unwrap(), id),
                Err(InstallationError::InvalidPath(_))
            ),
            "{suffix}"
        );
        assert_eq!(
            fs::read_to_string(shared.join("sentinel")).unwrap(),
            "shared"
        );
        assert_eq!(fs::read_dir(shared).unwrap().count(), 1);
    }
    let root = root();
    let mut registry = open(root.path());
    let target = root.path().join("foreign");
    fs::write(&target, "foreign").unwrap();
    symlink(&target, root.path().join("registry.yaml")).unwrap();
    assert!(matches!(
        registry.create(ProfileId::new(), label("a")),
        Err(InstallationError::InvalidPath(_))
    ));
    assert_eq!(registry.profiles().count(), 0);
    assert_eq!(fs::read_to_string(target).unwrap(), "foreign");
}

#[cfg(unix)]
#[test]
fn socket_allocation_checks_platform_byte_limit_without_truncation() {
    use std::os::unix::ffi::{OsStrExt, OsStringExt};
    use std::os::unix::net::UnixListener;
    let address: libc::sockaddr_un = unsafe { std::mem::zeroed() };
    let limit = address.sun_path.len() - 1;
    let root = root();
    let prefix_len = root.path().as_os_str().as_bytes().len() + 1;
    let path = root.path().join("x".repeat(limit - prefix_len));
    validate_socket_path(&path).unwrap();
    let _listener = UnixListener::bind(&path).unwrap();
    let too_long = root.path().join("x".repeat(limit - prefix_len + 1));
    assert!(
        matches!(validate_socket_path(&too_long), Err(InstallationError::SocketPathTooLong(path)) if path == too_long)
    );
    let unicode = root.path().join("é".repeat(limit / 2));
    assert!(matches!(
        validate_socket_path(&unicode),
        Err(InstallationError::SocketPathTooLong(_))
    ));
    let nul = PathBuf::from(std::ffi::OsString::from_vec(b"bad\0socket".to_vec()));
    assert!(matches!(
        validate_socket_path(&nul),
        Err(InstallationError::InvalidPath(_))
    ));
    let long_root = root.path().join("x".repeat(70));
    let registry = open(&long_root);
    assert!(matches!(
        ProfilePaths::for_id(registry.path().unwrap(), ProfileId::new()),
        Err(InstallationError::SocketPathTooLong(_))
    ));
    assert!(!long_root.join("profiles").exists());
    // A valid amux socket must still be refused if Codex would escape to /tmp.
    let id = ProfileId::new();
    let canonical = fs::canonicalize(root.path()).unwrap();
    let overhead = canonical.as_os_str().as_bytes().len()
        + 1
        + "/profiles/".len()
        + id.to_string().len()
        + "/amux.sock".len();
    let codex_only_overflow = canonical.join("y".repeat(limit - overhead));
    let registry = open(&codex_only_overflow);
    let socket = registry
        .path()
        .unwrap()
        .join("profiles")
        .join(id.to_string())
        .join("amux.sock");
    validate_socket_path(&socket).unwrap();
    let codex = crate::installation::adjacent_codex_socket_path(&socket);
    assert!(
        matches!(ProfilePaths::for_id(registry.path().unwrap(), id), Err(InstallationError::SocketPathTooLong(path)) if path == codex)
    );
    assert!(!codex_only_overflow.join("profiles").exists());
    println!(
        "Unix socket byte limit: {limit}; exact limit binds; longer and multibyte overflow paths are refused without truncation"
    );
}
