use std::collections::HashMap;
use std::sync::Arc;

use futures_util::StreamExt;
use tonic::{Code, Request};

use super::*;
use crate::installation::{
    CredentialSource, InstallationOptions, InstallationRoot, InstallationSettings, Listeners,
    OperationId,
};
use crate::protocol::wire::profile_service_server::ProfileService;

fn op() -> String {
    OperationId::new().0.to_string()
}
async fn front(listeners: Listeners) -> FrontDoor {
    let installation = Arc::new(
        Installation::open(InstallationOptions {
            root: InstallationRoot::InMemory,
            listeners,
            credentials: CredentialSource::ProfileFiles,
            identity_http: reqwest::Client::new(),
            settings: InstallationSettings {
                host_name: "front-door-test".into(),
                prevent_idle_sleep: Some(false),
                keybinds: Default::default(),
                ui: Default::default(),
                keymaps_dir: PathBuf::new(),
                minimum_client_versions: HashMap::new(),
                update_reporter: None,
                subscription_reporter: None,
            },
        })
        .await
        .unwrap(),
    );
    let path = installation.root().join("amux.sock");
    FrontDoor::new(installation, Some(path))
}
fn client(front: &FrontDoor) -> wire::profile_service_client::ProfileServiceClient<Channel> {
    wire::profile_service_client::ProfileServiceClient::new(front.channel())
}
async fn create(
    client: &mut wire::profile_service_client::ProfileServiceClient<Channel>,
    label: &str,
) -> wire::ProfileInfo {
    client
        .create_profile(wire::CreateProfileRequest {
            operation_id: op(),
            label: Some(label.into()),
        })
        .await
        .unwrap()
        .into_inner()
}

#[tokio::test]
async fn lifecycle_replays_original_results_and_rejects_stale_and_deleted_ids() {
    let front = front(Listeners::InProcessOnly).await;
    let mut first = client(&front);
    let mut second = client(&front);
    let request = wire::CreateProfileRequest {
        operation_id: op(),
        label: Some("work".into()),
    };
    let (a, b) = tokio::join!(
        first.create_profile(request.clone()),
        second.create_profile(request.clone())
    );
    let profile = a.unwrap().into_inner();
    assert_eq!(profile, b.unwrap().into_inner());
    assert!(profile.available);
    assert_eq!(profile.intent, wire::Intent::Unbound as i32);
    let renamed = first
        .rename_profile(wire::RenameProfileRequest {
            operation_id: op(),
            profile_id: profile.id.clone(),
            expected_revision: profile.revision,
            override_name: Some("office".into()),
        })
        .await
        .unwrap()
        .into_inner();
    assert_eq!(renamed.label, "office");
    assert_eq!(
        first
            .create_profile(request.clone())
            .await
            .unwrap()
            .into_inner(),
        profile
    );
    let stale = wire::RenameProfileRequest {
        operation_id: op(),
        profile_id: profile.id.clone(),
        expected_revision: profile.revision,
        override_name: None,
    };
    for _ in 0..2 {
        assert_eq!(
            first
                .rename_profile(stale.clone())
                .await
                .unwrap_err()
                .code(),
            Code::FailedPrecondition
        );
    }
    assert_eq!(
        first
            .delete_profile(wire::DeleteProfileRequest {
                operation_id: op(),
                profile_id: profile.id.clone(),
                confirm_revision: profile.revision
            })
            .await
            .unwrap_err()
            .code(),
        Code::FailedPrecondition
    );
    let paused = first
        .pause_profile(wire::ProfileOperation {
            operation_id: op(),
            profile_id: profile.id.clone(),
        })
        .await
        .unwrap()
        .into_inner();
    assert_eq!(paused.intent, wire::Intent::Paused as i32);
    let resumed = first
        .resume_profile(wire::ProfileOperation {
            operation_id: op(),
            profile_id: profile.id.clone(),
        })
        .await
        .unwrap()
        .into_inner();
    assert_eq!(resumed.intent, wire::Intent::Unbound as i32);
    let logged_out = first
        .logout_profile(wire::ProfileOperation {
            operation_id: op(),
            profile_id: profile.id.clone(),
        })
        .await
        .unwrap()
        .into_inner();
    let deletion = wire::DeleteProfileRequest {
        operation_id: op(),
        profile_id: profile.id.clone(),
        confirm_revision: logged_out.revision,
    };
    first.delete_profile(deletion.clone()).await.unwrap();
    first.delete_profile(deletion).await.unwrap();
    assert_eq!(
        first.create_profile(request).await.unwrap().into_inner(),
        profile
    );
    for id in [profile.id, uuid::Uuid::new_v4().to_string()] {
        assert_eq!(
            first
                .pause_profile(wire::ProfileOperation {
                    operation_id: op(),
                    profile_id: id.clone()
                })
                .await
                .unwrap_err()
                .code(),
            Code::NotFound
        );
        assert_eq!(
            first
                .list_peers(wire::ProfileRequest {
                    profile_id: id.clone()
                })
                .await
                .unwrap_err()
                .code(),
            Code::NotFound
        );
        assert_eq!(
            first
                .get_pairing_status(wire::ProfilePairingStatusRequest { profile_id: id })
                .await
                .unwrap_err()
                .code(),
            Code::NotFound
        );
    }
    assert!(
        first
            .list_profiles(wire::ListProfilesRequest {})
            .await
            .unwrap()
            .into_inner()
            .profiles
            .is_empty()
    );
    println!(
        "Two gRPC callers replay one create; rename revision checked; pause/resume/logout preserve the device; deleted and unknown profiles return NOT_FOUND."
    );
    front
        .installation
        .stop(crate::server::ShutdownReason::UserRequested)
        .await;
}

#[tokio::test]
async fn watch_delivers_snapshot_boundary_ordered_changes_and_removal() {
    use wire::watch_profiles_response::Event;
    let front = front(Listeners::InProcessOnly).await;
    let mut client = client(&front);
    let profile = create(&mut client, "personal").await;
    let mut watch = client
        .watch_profiles(wire::WatchProfilesRequest {})
        .await
        .unwrap()
        .into_inner();
    let snapshot = watch.message().await.unwrap().unwrap();
    assert_eq!(snapshot.event, Some(Event::Upserted(profile.clone())));
    let boundary = watch.message().await.unwrap().unwrap();
    assert!(matches!(boundary.event, Some(Event::SnapshotComplete(_))));
    assert_eq!(snapshot.sequence, boundary.sequence);
    let renamed = client
        .rename_profile(wire::RenameProfileRequest {
            operation_id: op(),
            profile_id: profile.id.clone(),
            expected_revision: profile.revision,
            override_name: Some("home".into()),
        })
        .await
        .unwrap()
        .into_inner();
    let change = watch.message().await.unwrap().unwrap();
    assert!(change.sequence > boundary.sequence);
    assert_eq!(change.event, Some(Event::Upserted(renamed.clone())));
    client
        .delete_profile(wire::DeleteProfileRequest {
            operation_id: op(),
            profile_id: profile.id.clone(),
            confirm_revision: renamed.revision,
        })
        .await
        .unwrap();
    let mut sequence = change.sequence;
    loop {
        let event = watch.message().await.unwrap().unwrap();
        assert!(event.sequence > sequence);
        sequence = event.sequence;
        if let Some(Event::RemovedId(id)) = event.event {
            assert_eq!(id, profile.id);
            break;
        }
    }
    println!("WatchProfiles: snapshot -> SnapshotComplete -> increasing upserts -> Removed.");
    front
        .installation
        .stop(crate::server::ShutdownReason::UserRequested)
        .await;
}

#[tokio::test]
async fn lag_ends_the_service_stream_with_aborted_and_resubscription_recovers() {
    let front = front(Listeners::InProcessOnly).await;
    let mut client = client(&front);
    let mut profile = create(&mut client, "work").await;
    // Read the actual handler stream without tonic's eager HTTP/2 buffering so
    // overflow is controlled by the producer, independent of machine speed.
    let mut watch = front
        .watch_profiles(Request::new(wire::WatchProfilesRequest {}))
        .await
        .unwrap()
        .into_inner();
    watch.next().await.unwrap().unwrap();
    watch.next().await.unwrap().unwrap();
    for n in 0..260 {
        profile = client
            .rename_profile(wire::RenameProfileRequest {
                operation_id: op(),
                profile_id: profile.id.clone(),
                expected_revision: profile.revision,
                override_name: Some(format!("work-{n}")),
            })
            .await
            .unwrap()
            .into_inner();
    }
    assert_eq!(
        watch.next().await.unwrap().unwrap_err().code(),
        Code::Aborted
    );
    assert!(watch.next().await.is_none());
    let mut fresh = client
        .watch_profiles(wire::WatchProfilesRequest {})
        .await
        .unwrap()
        .into_inner();
    assert_eq!(
        fresh.message().await.unwrap().unwrap().event,
        Some(wire::watch_profiles_response::Event::Upserted(profile))
    );
    println!(
        "A lagged service stream yields ABORTED once and ends; a new gRPC watch returns the current snapshot."
    );
    front
        .installation
        .stop(crate::server::ShutdownReason::UserRequested)
        .await;
}

#[tokio::test]
async fn pairing_targets_one_profile_and_replays_its_original_secret() {
    let front = front(Listeners::InProcessOnly).await;
    let mut client = client(&front);
    let a = create(&mut client, "personal").await;
    let b = create(&mut client, "work").await;
    let request = wire::ProfileStartPairingRequest {
        operation_id: op(),
        profile_id: a.id.clone(),
        pairing: Some(wire::StartPairingRequest {
            mode: wire::start_pairing_request::Mode::Pin.into(),
            require_lan_direct: false,
            demo: None,
        }),
    };
    let pairing = client
        .start_pairing(request.clone())
        .await
        .unwrap()
        .into_inner();
    let another_front = FrontDoor::new(front.installation.clone(), None);
    let mut another_client = self::client(&another_front);
    assert_eq!(
        another_client
            .start_pairing(request.clone())
            .await
            .unwrap()
            .into_inner(),
        pairing
    );
    assert!(
        client
            .get_pairing_status(wire::ProfilePairingStatusRequest {
                profile_id: a.id.clone()
            })
            .await
            .unwrap()
            .into_inner()
            .active
    );
    assert!(
        !client
            .get_pairing_status(wire::ProfilePairingStatusRequest { profile_id: b.id })
            .await
            .unwrap()
            .into_inner()
            .active
    );
    client
        .cancel_pairing(wire::ProfileOperation {
            operation_id: op(),
            profile_id: a.id.clone(),
        })
        .await
        .unwrap();
    assert_eq!(
        client.start_pairing(request).await.unwrap().into_inner(),
        pairing
    );
    assert!(
        !client
            .get_pairing_status(wire::ProfilePairingStatusRequest { profile_id: a.id })
            .await
            .unwrap()
            .into_inner()
            .active
    );
    println!(
        "Pairing belongs to personal only; replay after cancellation returns its original secret without opening a new window."
    );
    front
        .installation
        .stop(crate::server::ShutdownReason::UserRequested)
        .await;
}

#[tokio::test]
async fn rejects_invalid_ids_and_operation_reuse_across_methods() {
    let front = front(Listeners::InProcessOnly).await;
    let mut client = client(&front);
    assert_eq!(
        client
            .create_profile(wire::CreateProfileRequest {
                operation_id: String::new(),
                label: None
            })
            .await
            .unwrap_err()
            .code(),
        Code::InvalidArgument
    );
    assert_eq!(
        client
            .pause_profile(wire::ProfileOperation {
                operation_id: op(),
                profile_id: "bad".into()
            })
            .await
            .unwrap_err()
            .code(),
        Code::InvalidArgument
    );
    let operation_id = op();
    let profile = client
        .create_profile(wire::CreateProfileRequest {
            operation_id: operation_id.clone(),
            label: None,
        })
        .await
        .unwrap()
        .into_inner();
    assert_eq!(
        client
            .pause_profile(wire::ProfileOperation {
                operation_id,
                profile_id: profile.id
            })
            .await
            .unwrap_err()
            .code(),
        Code::FailedPrecondition
    );
    front
        .installation
        .stop(crate::server::ShutdownReason::UserRequested)
        .await;
}

#[tokio::test]
async fn installation_info_debug_and_shutdown_are_separate_from_client_service() {
    let front = front(Listeners::InProcessOnly).await;
    let profile = create(&mut client(&front), "personal").await;
    let retained = front
        .installation
        .client(mapping::profile_id(&profile.id).unwrap())
        .unwrap();
    let channel = front.channel();
    let mut installation =
        wire::installation_service_client::InstallationServiceClient::new(channel.clone());
    let info = installation
        .get_info(wire::GetInfoRequest {})
        .await
        .unwrap()
        .into_inner();
    assert_eq!(info.root, front.installation.root().to_string_lossy());
    assert_eq!(
        info.front_door_path,
        front.path.as_ref().unwrap().to_string_lossy()
    );
    assert!(!info.version.is_empty());
    println!("GetInfo gRPC response: {info:#?}");
    let dump = installation
        .debug_installation(wire::DebugRequest {
            verbose: false,
            format: wire::DebugFormat::Json.into(),
        })
        .await
        .unwrap()
        .into_inner()
        .dump;
    let dump: serde_json::Value = serde_json::from_str(&dump).unwrap();
    assert_eq!(dump["profiles"][0]["id"], profile.id);
    let mut absent = wire::client_service_client(channel);
    assert_eq!(
        absent
            .list_agents(wire::ListAgentsRequest {})
            .await
            .unwrap_err()
            .code(),
        Code::Unimplemented
    );
    let shutdown = wire::InstallationShutdownRequest { operation_id: op() };
    installation.shutdown(shutdown.clone()).await.unwrap();
    installation.shutdown(shutdown).await.unwrap();
    assert!(retained.list_agents().await.is_err());
    println!(
        "Front door serves installation info/debug/shutdown and rejects ClientService with UNIMPLEMENTED; shutdown closes retained profile clients."
    );
}

#[cfg(unix)]
#[tokio::test]
async fn unix_front_door_discovers_profile_socket_and_refuses_socket_theft() {
    use std::os::unix::fs::PermissionsExt;
    let front = front(Listeners::Sockets).await;
    let listener = front.listen().unwrap();
    let path = front.path.as_ref().unwrap();
    assert_eq!(
        std::fs::metadata(path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert!(front.listen().is_err());
    let stream = tokio::net::UnixStream::connect(path).await.unwrap();
    let channel = transport::channel_from_single_io(
        tonic::transport::Endpoint::from_static("http://localhost"),
        "front door",
        GrpcIo::new(stream),
    );
    let mut directory = wire::profile_service_client::ProfileServiceClient::new(channel.clone());
    let profile = create(&mut directory, "work").await;
    assert!(!profile.socket_path.is_empty());
    let profiles = directory
        .list_profiles(wire::ListProfilesRequest {})
        .await
        .unwrap()
        .into_inner()
        .profiles;
    assert_eq!(profiles.as_slice(), std::slice::from_ref(&profile));
    println!("ListProfiles gRPC response: {profiles:#?}");
    let mut info =
        wire::installation_service_client::InstallationServiceClient::new(channel.clone());
    assert_eq!(
        info.get_info(wire::GetInfoRequest {})
            .await
            .unwrap()
            .into_inner()
            .front_door_path,
        path.to_string_lossy()
    );
    assert_eq!(
        wire::client_service_client(channel)
            .list_agents(wire::ListAgentsRequest {})
            .await
            .unwrap_err()
            .code(),
        Code::Unimplemented
    );
    let stream = tokio::net::UnixStream::connect(&profile.socket_path)
        .await
        .unwrap();
    let profile_channel = transport::channel_from_single_io(
        tonic::transport::Endpoint::from_static("http://localhost"),
        "profile",
        GrpcIo::new(stream),
    );
    assert!(
        wire::client_service_client(profile_channel.clone())
            .list_agents(wire::ListAgentsRequest {})
            .await
            .unwrap()
            .into_inner()
            .agents
            .is_empty()
    );
    assert_eq!(
        wire::profile_service_client::ProfileServiceClient::new(profile_channel.clone())
            .list_profiles(wire::ListProfilesRequest {})
            .await
            .unwrap_err()
            .code(),
        Code::Unimplemented
    );
    assert_eq!(
        wire::installation_service_client::InstallationServiceClient::new(profile_channel)
            .get_info(wire::GetInfoRequest {})
            .await
            .unwrap_err()
            .code(),
        Code::Unimplemented
    );
    listener.stop().await;
    assert!(!path.exists());
    assert!(
        front
            .installation
            .client(mapping::profile_id(&profile.id).unwrap())
            .unwrap()
            .list_agents()
            .await
            .is_ok()
    );
    println!(
        "Plain Unix gRPC: list profile -> connect returned socket -> list agents. Front door is 0600, refuses a second bind, and registers only administration services; profile registers neither."
    );
    front
        .installation
        .stop(crate::server::ShutdownReason::UserRequested)
        .await;
}

#[tokio::test]
async fn binding_reports_identity_labels_and_named_account_refusals_over_grpc() {
    use crate::test_fixtures::{IdentityServer, TestAccount};
    let identity = IdentityServer::start(
        ["alice", "bob"]
            .into_iter()
            .map(|sub| TestAccount {
                sub: sub.into(),
                name: Some(format!("{sub} Example")),
                email: Some(format!("{sub}@example.test")),
            })
            .collect(),
        None,
    )
    .await;
    let front = front(Listeners::InProcessOnly).await;
    let mut client = client(&front);
    let request = wire::BindProfileRequest {
        operation_id: op(),
        profile_id: None,
        cloud_url: identity.url(),
        staged_refresh_token: identity.refresh_token_for("alice"),
        adopt_non_pristine: false,
    };
    let bound = client
        .bind_profile(request.clone())
        .await
        .unwrap()
        .into_inner();
    println!("BindProfile gRPC response: {bound:#?}");
    assert_eq!(bound.label, "alice Example");
    assert_eq!(bound.account_name, "alice Example");
    assert_eq!(bound.email, "alice@example.test");
    assert_eq!(bound.intent, wire::Intent::Bound as i32);
    assert_eq!(
        client.bind_profile(request).await.unwrap().into_inner(),
        bound
    );
    let wrong = client
        .bind_profile(wire::BindProfileRequest {
            operation_id: op(),
            profile_id: Some(bound.id.clone()),
            cloud_url: identity.url(),
            staged_refresh_token: identity.refresh_token_for("bob"),
            adopt_non_pristine: false,
        })
        .await
        .unwrap_err();
    assert_eq!(wrong.code(), Code::FailedPrecondition);
    assert!(wrong.message().contains(&bound.id));
    let other = create(&mut client, "other").await;
    let duplicate = client
        .bind_profile(wire::BindProfileRequest {
            operation_id: op(),
            profile_id: Some(other.id),
            cloud_url: identity.url(),
            staged_refresh_token: identity.refresh_token_for("alice"),
            adopt_non_pristine: false,
        })
        .await
        .unwrap_err();
    assert_eq!(duplicate.code(), Code::AlreadyExists);
    assert!(duplicate.message().contains(&bound.id));
    println!(
        "BindProfile userinfo: label=alice Example, email=alice@example.test; retries replay the accepted result, wrong-account and duplicate-account requests identify the reserved profile."
    );
    front
        .installation
        .stop(crate::server::ShutdownReason::UserRequested)
        .await;
}

#[tokio::test]
async fn accepted_mutation_survives_the_request_future_being_dropped() {
    let ledger = Arc::new(ledger::Ledger::default());
    let id = OperationId::new();
    let (started_tx, started_rx) = tokio::sync::oneshot::channel();
    let (finish_tx, finish_rx) = tokio::sync::oneshot::channel();
    let request = {
        let ledger = ledger.clone();
        tokio::spawn(async move {
            ledger
                .run(id, "create", vec![1], async move {
                    started_tx.send(()).unwrap();
                    finish_rx.await.unwrap();
                    Ok(wire::ProfileInfo {
                        label: "completed".into(),
                        ..Default::default()
                    })
                })
                .await
        })
    };
    started_rx.await.unwrap();
    request.abort();
    let _ = request.await;
    finish_tx.send(()).unwrap();
    let result: wire::ProfileInfo = ledger
        .run(id, "create", vec![1], async {
            panic!("retry must not execute again")
        })
        .await
        .unwrap();
    assert_eq!(result.label, "completed");
}

#[cfg(unix)]
#[tokio::test]
async fn stopping_old_front_door_preserves_a_replacement_socket() {
    use std::os::unix::fs::MetadataExt;
    let front = front(Listeners::InProcessOnly).await;
    let listener = front.listen().unwrap();
    let path = front.path.as_ref().unwrap();
    std::fs::remove_file(path).unwrap();
    let replacement = tokio::net::UnixListener::bind(path).unwrap();
    let inode = std::fs::metadata(path).unwrap().ino();
    listener.stop().await;
    assert_eq!(std::fs::metadata(path).unwrap().ino(), inode);
    drop(replacement);
    front
        .installation
        .stop(crate::server::ShutdownReason::UserRequested)
        .await;
}
