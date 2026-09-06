use tonic::Status;
use uuid::Uuid;

use crate::installation::{
    BindError, InstallationError, Intent, Observed, OperationId, ProfileEvent, ProfileId,
    ProfileStatus,
};
use crate::protocol::wire;

pub(super) fn operation_id(value: &str) -> Result<OperationId, Status> {
    Uuid::parse_str(value)
        .map(OperationId)
        .map_err(|_| Status::invalid_argument("operation_id must be a UUID"))
}
pub(super) fn profile_id(value: &str) -> Result<ProfileId, Status> {
    Uuid::parse_str(value)
        .map(ProfileId)
        .map_err(|_| Status::invalid_argument("profile_id must be a UUID"))
}
pub(super) fn installation_error(error: InstallationError) -> Status {
    let message = error.to_string();
    match error {
        InstallationError::UnknownProfile(_) | InstallationError::Deleted(_) => {
            Status::not_found(message)
        }
        InstallationError::RevisionMismatch { .. } => Status::failed_precondition(message),
        InstallationError::Unavailable(_) => Status::unavailable(message),
        InstallationError::RootBusy(_) | InstallationError::SocketOccupied(_) => {
            Status::already_exists(message)
        }
        InstallationError::InvalidPath(_)
        | InstallationError::SocketPathTooLong(_)
        | InstallationError::Config(_) => Status::invalid_argument(message),
        InstallationError::Io(_) | InstallationError::Registry(_) => Status::internal(message),
    }
}
pub(super) fn bind_error(error: BindError) -> Status {
    let message = error.to_string();
    match error {
        BindError::Installation(error) => installation_error(error),
        BindError::AccountAlreadyBound { profile } => {
            let mut status = Status::already_exists(message);
            status
                .metadata_mut()
                .insert("amux-profile-id", profile.to_string().parse().unwrap());
            status
        }
        BindError::ProfileBoundToOtherAccount { profile } => {
            let mut status = Status::failed_precondition(message);
            status
                .metadata_mut()
                .insert("amux-profile-id", profile.to_string().parse().unwrap());
            status
        }
        BindError::AdoptionNeedsConfirmation { profile, .. } => {
            let mut status = Status::failed_precondition(message);
            status.metadata_mut().insert(
                "amux-bind-error",
                "adoption-needs-confirmation".parse().unwrap(),
            );
            status
                .metadata_mut()
                .insert("amux-profile-id", profile.to_string().parse().unwrap());
            status
        }
        BindError::UserinfoUnavailable(_) => Status::unavailable(message),
        BindError::MissingSubject | BindError::TokenRejected(_) => Status::unauthenticated(message),
        BindError::Persist(_) => Status::internal(message),
        BindError::Cancelled => Status::aborted(message),
        BindError::InvalidCloudUrl(_) => Status::invalid_argument(message),
    }
}
fn clean(value: &str) -> String {
    value
        .chars()
        .filter(|c| !c.is_control())
        .collect::<String>()
        .trim()
        .to_owned()
}
pub(super) fn profile_info(status: ProfileStatus) -> wire::ProfileInfo {
    let record = status.record;
    let email = clean(record.label.email.as_deref().unwrap_or_default());
    let account_name = clean(record.label.account_name.as_deref().unwrap_or_default());
    let label = record
        .label
        .override_name
        .as_deref()
        .map(clean)
        .filter(|s| !s.is_empty())
        .or_else(|| (!account_name.is_empty()).then(|| account_name.clone()))
        .or_else(|| (!email.is_empty()).then(|| email.clone()))
        .unwrap_or_else(|| {
            record
                .binding
                .as_ref()
                .map(|b| clean(&b.account.subject).chars().take(8).collect())
                .filter(|s: &String| !s.is_empty())
                .unwrap_or_else(|| record.id.to_string()[..8].into())
        });
    let minimum_version = match &status.observed {
        Observed::UpdateRequired { minimum_version } => minimum_version.clone(),
        _ => None,
    };
    wire::ProfileInfo {
        id: record.id.to_string(),
        label,
        email,
        account_name,
        socket_path: status
            .socket_path
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default(),
        host_id: status.host_id.to_string(),
        intent: match status.intent {
            Intent::Unbound => wire::Intent::Unbound,
            Intent::Bound => wire::Intent::Bound,
            Intent::LoggedOut => wire::Intent::LoggedOut,
            Intent::Paused => wire::Intent::Paused,
        }
        .into(),
        observed: match status.observed {
            Observed::Local => wire::Observed::Local,
            Observed::Connecting => wire::Observed::Connecting,
            Observed::Connected => wire::Observed::Connected,
            Observed::Retrying => wire::Observed::Retrying,
            Observed::AuthenticationRequired => wire::Observed::AuthenticationRequired,
            Observed::SubscriptionRequired => wire::Observed::SubscriptionRequired,
            Observed::UpdateRequired { .. } => wire::Observed::UpdateRequired,
            Observed::StartupFailed => wire::Observed::StartupFailed,
        }
        .into(),
        revision: record.revision,
        startup_error: status.startup_error.unwrap_or_default(),
        available: status.available,
        minimum_version,
    }
}
pub(super) fn watch_event(event: ProfileEvent) -> Result<wire::WatchProfilesResponse, Status> {
    use wire::watch_profiles_response::Event;
    let (sequence, event) = match event {
        ProfileEvent::Upserted { sequence, profile } => {
            (sequence, Event::Upserted(profile_info(*profile)))
        }
        ProfileEvent::SnapshotComplete { sequence } => {
            (sequence, Event::SnapshotComplete(wire::SnapshotComplete {}))
        }
        ProfileEvent::Removed { sequence, id } => (sequence, Event::RemovedId(id.to_string())),
        ProfileEvent::Lagged => {
            return Err(Status::aborted(
                "profile watch lagged; subscribe again for a fresh snapshot",
            ));
        }
    };
    Ok(wire::WatchProfilesResponse {
        sequence,
        event: Some(event),
    })
}
