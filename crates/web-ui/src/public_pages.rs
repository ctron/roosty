use std::{
    borrow::Cow,
    fmt::{self, Display, Formatter},
    future::Future,
    pin::Pin,
};

use leptos::{
    prelude::*,
    server_fn::{
        Http,
        codec::{GetUrl, Json},
    },
};
use leptos_router::{PartialPathMatch, PathSegment, PossibleRouteMatch};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[cfg(feature = "ssr")]
use crate::bootstrap::{UiServerContext, request_cookie};

/// A route segment that captures `@username` as `username`.
///
/// Leptos sees a typed matcher while Axum receives the compatible
/// `/{username}` parameter with the `@` prefix retained in the route pattern.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AtUsernameSegment(pub &'static str);

impl PossibleRouteMatch for AtUsernameSegment {
    fn optional(&self) -> bool {
        false
    }

    fn test<'a>(&self, path: &'a str) -> Option<PartialPathMatch<'a>> {
        let segment = path.strip_prefix('/').unwrap_or(path);
        let value = segment.split('/').next()?;
        let username = value.strip_prefix('@')?;
        if username.is_empty() {
            return None;
        }
        let matched_len = path.len() - segment.len() + value.len();
        let (matched, remaining) = path.split_at(matched_len);
        Some(PartialPathMatch::new(
            remaining,
            vec![(Cow::Borrowed(self.0), username.to_owned())],
            matched,
        ))
    }

    fn generate_path(&self, path: &mut Vec<PathSegment>) {
        path.push(PathSegment::Static(format!("/@{{{}}}", self.0).into()));
    }
}

/// Closed set of public profile timeline filters.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum UiProfileTab {
    Posts,
    WithReplies,
    Media,
    Tagged,
}

impl UiProfileTab {
    pub fn path_suffix(&self) -> &'static str {
        match self {
            Self::Posts => "",
            Self::WithReplies => "/with_replies",
            Self::Media => "/media",
            Self::Tagged => "/tagged",
        }
    }
}

/// A typed profile metadata field.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct UiProfileField {
    pub name: String,
    pub value: String,
}

/// Public local-account data used by profile and status pages.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct UiPublicAccount {
    pub id: Uuid,
    pub username: String,
    pub display_name: String,
    pub bio: String,
    pub avatar_url: Option<String>,
    pub header_url: Option<String>,
    pub fields: Vec<UiProfileField>,
    pub created_at: String,
    pub followers_count: u64,
    pub following_count: u64,
    pub statuses_count: u64,
    pub limited: bool,
    pub discoverable: bool,
}

/// A featured hashtag and its public status count.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct UiFeaturedTag {
    pub name: String,
    pub statuses_count: u64,
}

/// Closed media categories rendered by public pages.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum UiMediaKind {
    Image,
    Video,
    Audio,
    Unknown,
}

/// Read-only status attachment.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct UiMedia {
    pub kind: UiMediaKind,
    pub url: String,
    pub preview_url: Option<String>,
    pub description: Option<String>,
}

/// One read-only poll option.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct UiPollOption {
    pub title: String,
    pub votes_count: Option<u64>,
}

/// A read-only poll projection.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct UiPoll {
    pub multiple: bool,
    pub expired: bool,
    pub voters_count: Option<u64>,
    pub options: Vec<UiPollOption>,
}

/// Link preview data safe to expose in public HTML.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct UiPreviewCard {
    pub url: String,
    pub title: String,
    pub description: String,
    pub provider_name: String,
    pub image_url: Option<String>,
}

/// Closed Mastodon visibility values.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum UiStatusVisibility {
    Public,
    Unlisted,
    Private,
    Direct,
}

/// Read-only status data shared by profile timelines and threads.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct UiStatus {
    pub id: Uuid,
    pub author: UiStatusAuthor,
    pub url: String,
    pub activitypub_url: String,
    pub content_html: String,
    pub spoiler_text: String,
    pub sensitive: bool,
    pub visibility: UiStatusVisibility,
    pub created_at: String,
    pub edited_at: Option<String>,
    pub media: Vec<UiMedia>,
    pub poll: Option<UiPoll>,
    pub card: Option<UiPreviewCard>,
    pub quote: Option<Box<UiStatus>>,
    pub replies_count: u64,
    pub reblogs_count: u64,
    pub favourites_count: u64,
    pub pinned: bool,
}

/// Local or cached-remote author information shown beside a status.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct UiStatusAuthor {
    pub display_name: String,
    pub handle: String,
    pub url: String,
    pub avatar_url: Option<String>,
    pub local: bool,
}

/// Cursor-paginated status result.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct UiStatusPage {
    pub statuses: Vec<UiStatus>,
    pub next_cursor: Option<Uuid>,
}

/// Profile identity shared by every public timeline tab.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct UiProfileHeader {
    pub account: UiPublicAccount,
    pub featured_tags: Vec<UiFeaturedTag>,
    pub profile_url: String,
    pub activitypub_url: String,
}

/// One selected public profile timeline and its document metadata.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct UiProfileTimeline {
    pub tab: UiProfileTab,
    pub hashtag: Option<String>,
    pub pinned_statuses: Vec<UiStatus>,
    pub timeline: UiStatusPage,
}

/// Bounded status thread with the focused local permalink.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct UiStatusThread {
    pub account: UiPublicAccount,
    pub ancestors: Vec<UiStatus>,
    pub status: UiStatus,
    pub descendants: Vec<UiStatus>,
    pub canonical_url: String,
    pub activitypub_url: String,
    pub noindex: bool,
}

/// Error class returned by the native public-page backend.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UiPublicPageError {
    BadRequest,
    NotFound,
    Internal,
}

impl Display for UiPublicPageError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::BadRequest => "invalid public page request",
            Self::NotFound => "public page not found",
            Self::Internal => "public page is unavailable",
        })
    }
}

pub(crate) type ProfileHeaderFuture =
    Pin<Box<dyn Future<Output = Result<UiProfileHeader, UiPublicPageError>> + Send + 'static>>;
pub(crate) type ProfileTimelineFuture =
    Pin<Box<dyn Future<Output = Result<UiProfileTimeline, UiPublicPageError>> + Send + 'static>>;
pub(crate) type StatusPageFuture =
    Pin<Box<dyn Future<Output = Result<UiStatusPage, UiPublicPageError>> + Send + 'static>>;
pub(crate) type ThreadFuture =
    Pin<Box<dyn Future<Output = Result<UiStatusThread, UiPublicPageError>> + Send + 'static>>;

/// Load profile identity that remains stable while timeline tabs change.
#[server(prefix = "/api/web", protocol = Http<GetUrl, Json>)]
pub async fn load_profile_header(username: String) -> Result<UiProfileHeader, ServerFnError> {
    #[cfg(feature = "ssr")]
    {
        let backend = expect_context::<UiServerContext>();
        public_result(
            backend
                .0
                .profile_header(request_cookie().await?, username)
                .await,
        )
    }
    #[cfg(not(feature = "ssr"))]
    unreachable!("the browser build uses the generated server-function client")
}

/// Load the selected initial profile timeline.
#[server(prefix = "/api/web", protocol = Http<GetUrl, Json>)]
pub async fn load_profile_timeline(
    username: String,
    tab: UiProfileTab,
    hashtag: Option<String>,
    max_id: Option<String>,
) -> Result<UiProfileTimeline, ServerFnError> {
    #[cfg(feature = "ssr")]
    {
        let backend = expect_context::<UiServerContext>();
        public_result(
            backend
                .0
                .profile_timeline(request_cookie().await?, username, tab, hashtag, max_id)
                .await,
        )
    }
    #[cfg(not(feature = "ssr"))]
    unreachable!("the browser build uses the generated server-function client")
}

/// Load another profile timeline page for hydrated “Load more.”
#[server(prefix = "/api/web", protocol = Http<GetUrl, Json>)]
pub async fn load_profile_statuses(
    username: String,
    tab: UiProfileTab,
    hashtag: Option<String>,
    max_id: String,
) -> Result<UiStatusPage, ServerFnError> {
    #[cfg(feature = "ssr")]
    {
        let backend = expect_context::<UiServerContext>();
        public_result(
            backend
                .0
                .profile_statuses(request_cookie().await?, username, tab, hashtag, max_id)
                .await,
        )
    }
    #[cfg(not(feature = "ssr"))]
    unreachable!("the browser build uses the generated server-function client")
}

/// Load a local status permalink and its bounded visible context.
#[server(prefix = "/api/web", protocol = Http<GetUrl, Json>)]
pub async fn load_status_thread(
    username: String,
    status_id: String,
) -> Result<UiStatusThread, ServerFnError> {
    #[cfg(feature = "ssr")]
    {
        let backend = expect_context::<UiServerContext>();
        public_result(
            backend
                .0
                .status_thread(request_cookie().await?, username, status_id)
                .await,
        )
    }
    #[cfg(not(feature = "ssr"))]
    unreachable!("the browser build uses the generated server-function client")
}

#[cfg(feature = "ssr")]
fn public_result<T>(result: Result<T, UiPublicPageError>) -> Result<T, ServerFnError> {
    use axum::http::StatusCode;

    match result {
        Ok(value) => Ok(value),
        Err(error) => {
            expect_context::<leptos_axum::ResponseOptions>().set_status(match error {
                UiPublicPageError::BadRequest => StatusCode::BAD_REQUEST,
                UiPublicPageError::NotFound => StatusCode::NOT_FOUND,
                UiPublicPageError::Internal => StatusCode::INTERNAL_SERVER_ERROR,
            });
            Err(ServerFnError::new(error))
        }
    }
}

#[cfg(test)]
mod tests {
    use leptos_router::{PathSegment, PossibleRouteMatch};

    use super::AtUsernameSegment;

    #[test]
    fn at_username_matches_and_generates_an_axum_prefixed_parameter() {
        let matcher = AtUsernameSegment("username");
        let matched = matcher.test("/@alice/media").unwrap();
        assert_eq!(matched.remaining(), "/media");
        assert_eq!(matched.params(), &[("username".into(), "alice".to_owned())]);

        let mut path = Vec::new();
        matcher.generate_path(&mut path);
        assert_eq!(
            path,
            vec![PathSegment::Static("/@{username}".to_owned().into())]
        );
        assert!(matcher.test("/api/v1/accounts").is_none());
        assert!(matcher.test("/users/alice").is_none());
    }
}
