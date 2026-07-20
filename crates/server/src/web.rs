//! Native Axum integration for the server-rendered and hydrated first-party UI.

use std::{
    future::Future,
    pin::Pin,
    sync::{Arc, OnceLock},
};

use axum::{
    Router,
    body::Body,
    extract::State,
    http::{HeaderMap, HeaderValue, Request, StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Redirect, Response},
};
use leptos::prelude::provide_context;
use leptos_axum::{AxumRouteListing, LeptosRoutes, generate_route_list};
use roosty_web_ui::{App, UiAccount, UiBackend, UiBootstrap, UiServerContext, shell};
use tower_http::services::ServeDir;

use crate::{auth::account_id_from_session, http::AppState};

static UI_ROUTES: OnceLock<Vec<AxumRouteListing>> = OnceLock::new();

fn ui_routes() -> Vec<AxumRouteListing> {
    UI_ROUTES.get_or_init(|| generate_route_list(App)).clone()
}

/// Mount explicit UI routes, internal server functions, and generated browser assets.
pub fn router(state: &AppState) -> Router<AppState> {
    let routes = ui_routes();
    let options = state.leptos_options.clone();
    let context = UiServerContext(Arc::new(RoostyUiBackend {
        state: state.clone(),
    }));
    let assets =
        ServeDir::new(std::path::Path::new(&*options.site_root).join(&*options.site_pkg_dir));

    Router::new()
        .leptos_routes_with_context(
            state,
            routes,
            move || provide_context(context.clone()),
            move || shell(options.clone()),
        )
        .nest_service("/pkg", assets)
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            protect_password_form,
        ))
}

async fn protect_password_form(
    State(state): State<AppState>,
    request: Request<Body>,
    next: Next,
) -> Response {
    if request.uri().path() != "/auth/edit" {
        return next.run(request).await;
    }

    match account_id_from_session(&state, request.headers()) {
        Ok(Some(_)) => next.run(request).await,
        Ok(None) => {
            let mut location = state.config.public_base_url.clone();
            location.set_path("/login");
            location.set_query(Some("next=%2Fauth%2Fedit"));
            location.set_fragment(None);
            Redirect::to(location.as_str()).into_response()
        }
        Err(error) => {
            tracing::error!(%error, "failed to validate browser session");
            (StatusCode::INTERNAL_SERVER_ERROR, "internal server error\n").into_response()
        }
    }
}

#[derive(Clone)]
struct RoostyUiBackend {
    state: AppState,
}

impl UiBackend for RoostyUiBackend {
    fn bootstrap(
        &self,
        cookie_header: Option<String>,
    ) -> Pin<Box<dyn Future<Output = Result<UiBootstrap, String>> + Send + 'static>> {
        let state = self.state.clone();
        Box::pin(async move {
            let mut headers = HeaderMap::new();
            if let Some(cookie_header) = cookie_header {
                let value =
                    HeaderValue::from_str(&cookie_header).map_err(|error| error.to_string())?;
                headers.insert(header::COOKIE, value);
            }
            let account = match account_id_from_session(&state, &headers)
                .map_err(|error| error.to_string())?
            {
                Some(account_id) => roosty_db::find_local_account_by_id(&state.db, account_id)
                    .await
                    .map_err(|error| error.to_string())?
                    .map(|account| UiAccount {
                        id: account.id.0,
                        username: account.username,
                        display_name: account.display_name,
                    }),
                None => None,
            };
            Ok(UiBootstrap {
                instance_name: state.config.instance_name.clone(),
                instance_description: state.config.instance_description.clone(),
                public_base_url: state.config.public_base_url.to_string(),
                server_version: env!("CARGO_PKG_VERSION").to_owned(),
                account,
            })
        })
    }
}

#[cfg(test)]
mod tests {
    use std::{future::Future, pin::Pin, sync::Arc};

    use axum::{
        Router,
        body::{Body, to_bytes},
        extract::FromRef,
        http::{Request, StatusCode, header},
    };
    use leptos::{config::LeptosOptions, prelude::provide_context};
    use leptos_axum::LeptosRoutes;
    use roosty_web_ui::{UiAccount, UiBackend, UiBootstrap, UiServerContext, shell};
    use tower::ServiceExt;
    use uuid::Uuid;

    /// Given the first UI slice, when Leptos enumerates routes, then both direct entry points are
    /// registered with Axum rather than relying on a catch-all fallback.
    #[tokio::test]
    async fn generated_routes_include_welcome_and_about() {
        let paths = super::ui_routes()
            .into_iter()
            .map(|route| route.path().to_owned())
            .collect::<Vec<_>>();

        assert!(paths.iter().any(|path| path == "/"));
        assert!(paths.iter().any(|path| path == "/about"));
        assert!(paths.iter().any(|path| path == "/login"));
        assert!(paths.iter().any(|path| path == "/auth/edit"));
    }

    /// Given a failed credential submission, when the redirected login page renders, then the new
    /// shell preserves the safe return path and displays an accessible error beside the form.
    #[tokio::test]
    async fn renders_login_form_with_redirect_state() {
        let response = test_router()
            .oneshot(
                Request::get("/login?next=%2Fabout&error=invalid_credentials")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let html = String::from_utf8(body.to_vec()).unwrap();
        assert!(html.contains("<h1>Sign in</h1>"));
        assert!(html.contains("action=\"/login\""));
        assert!(html.contains("name=\"next\" value=\"/about\""));
        assert!(html.contains("Invalid username or password."));
        assert!(html.contains("role=\"alert\""));
    }

    /// Given a signed-in visitor, when the password form is requested, then all fields retain the
    /// existing server handler names and a typed redirect result is presented accessibly.
    #[tokio::test]
    async fn renders_authenticated_password_form_and_result() {
        let response = test_router()
            .oneshot(
                Request::get("/auth/edit?result=current_password_incorrect")
                    .header(header::COOKIE, "roosty_session=test-session")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let html = String::from_utf8(body.to_vec()).unwrap();
        assert!(html.contains("<h1>Change password</h1>"));
        assert!(html.contains("action=\"/auth\""));
        assert!(html.contains("name=\"user[current_password]\""));
        assert!(html.contains("name=\"user[password]\""));
        assert!(html.contains("name=\"user[password_confirmation]\""));
        assert!(html.contains("Current password is incorrect."));
        assert!(html.contains("role=\"alert\""));
    }

    /// Given an anonymous visitor, when either UI route is requested directly, then the initial
    /// HTML contains route-specific content, SEO metadata, hydration, and a safe login return path.
    #[tokio::test]
    async fn renders_deep_links_with_metadata_and_session_navigation() {
        let app = test_router();
        for (path, marker, title, login_next) in [
            ("/", "Welcome to", "Welcome · Test Roosty", "/"),
            (
                "/about",
                "About this instance",
                "About · Test Roosty",
                "/about",
            ),
        ] {
            let response = app
                .clone()
                .oneshot(Request::get(path).body(Body::empty()).unwrap())
                .await
                .unwrap();

            assert_eq!(response.status(), StatusCode::OK);
            let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
            let html = String::from_utf8(body.to_vec()).unwrap();
            assert!(html.contains(marker), "missing page marker in {path}");
            assert!(html.contains("<h1>Test Roosty</h1>"));
            assert!(html.contains("class=\"brand\">Test Roosty</a>"));
            assert!(html.contains("A test social server"));
            assert!(html.contains("Powered by Roosty v"));
            assert!(html.contains("1.2.3"));
            assert!(html.contains(&format!("<title>{title}</title>")));
            assert!(html.contains(&format!(
                "href=\"https://roosty.test{path}\" rel=\"canonical\""
            )));
            assert!(html.contains(&format!("href=\"/login?next={login_next}\"")));
            assert!(html.contains(&format!(
                "href=\"/login?next={login_next}\" rel=\"external\""
            )));
            assert!(html.contains("/pkg/roosty-web.js"));
            if path == "/" {
                assert!(html.contains(">About this instance</a>"));
            }
        }
    }

    /// Given an instance without an operator description, when its welcome page is rendered, then
    /// visitors see neutral instance copy rather than project marketing or an empty lead.
    #[tokio::test]
    async fn renders_neutral_missing_description_fallback() {
        let response = test_router_with_description(None)
            .oneshot(Request::get("/").body(Body::empty()).unwrap())
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let html = String::from_utf8(body.to_vec()).unwrap();
        assert!(html.contains("A place to connect on the social web."));
        assert!(html.contains(
            "<meta name=\"description\" content=\"A place to connect on the social web.\">"
        ));
        assert!(!html.contains("built in Rust"));
    }

    /// Given a session cookie, when the welcome page is rendered, then the server-side bootstrap
    /// passes the request cookie to the backend and renders authenticated navigation immediately.
    #[tokio::test]
    async fn renders_authenticated_session_navigation() {
        let response = test_router()
            .oneshot(
                Request::get("/")
                    .header(header::COOKIE, "roosty_session=test-session")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let html = String::from_utf8(body.to_vec()).unwrap();
        assert!(html.contains("Welcome, "));
        assert!(html.contains("alice"));
        assert!(html.contains("href=\"/auth/edit\" rel=\"external\""));
        assert!(!html.contains("/login?next="));
    }

    #[derive(Clone)]
    struct TestState {
        options: LeptosOptions,
    }

    impl FromRef<TestState> for LeptosOptions {
        fn from_ref(state: &TestState) -> Self {
            state.options.clone()
        }
    }

    #[derive(Clone)]
    struct TestBackend {
        instance_description: Option<String>,
    }

    impl UiBackend for TestBackend {
        fn bootstrap(
            &self,
            cookie_header: Option<String>,
        ) -> Pin<Box<dyn Future<Output = Result<UiBootstrap, String>> + Send + 'static>> {
            let instance_description = self.instance_description.clone();
            Box::pin(async move {
                let account = cookie_header
                    .filter(|value| value.contains("roosty_session=test-session"))
                    .map(|_| UiAccount {
                        id: Uuid::nil(),
                        username: "alice".to_owned(),
                        display_name: "Alice".to_owned(),
                    });
                Ok(UiBootstrap {
                    instance_name: "Test Roosty".to_owned(),
                    instance_description,
                    public_base_url: "https://roosty.test".to_owned(),
                    server_version: "1.2.3".to_owned(),
                    account,
                })
            })
        }
    }

    fn test_router() -> Router {
        test_router_with_description(Some("A test social server".to_owned()))
    }

    fn test_router_with_description(instance_description: Option<String>) -> Router {
        let options = LeptosOptions::builder()
            .output_name("roosty-web")
            .site_root("target/site")
            .site_pkg_dir("pkg")
            .build();
        let state = TestState {
            options: options.clone(),
        };
        let context = UiServerContext(Arc::new(TestBackend {
            instance_description,
        }));

        Router::new()
            .leptos_routes_with_context(
                &state,
                super::ui_routes(),
                move || provide_context(context.clone()),
                move || shell(options.clone()),
            )
            .with_state(state)
    }
}
