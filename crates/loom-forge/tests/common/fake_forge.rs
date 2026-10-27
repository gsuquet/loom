//! A fake forge for the client tests: an HTTP server on `127.0.0.1` that answers each request
//! from a list of routes, mostly `fixtures/forge/` files, and records every request.
//!
//! Self-contained (axum, tokio, std): another crate's tests use it with
//! `#[path = "../../loom-forge/tests/common/fake_forge.rs"] mod fake_forge;`.

#![allow(
    dead_code,
    clippy::unwrap_used,
    reason = "each test crate uses part of the server; a failing server must fail the test"
)]

use std::net::TcpListener;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::thread;

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::extract::{Request, State};
use axum::http::{HeaderName, HeaderValue, Response, StatusCode};

/// What replaces `{base}` in the headers and bodies of the routes: the base URL of the server.
pub const BASE: &str = "{base}";

/// The bytes of `fixtures/forge/<path>`.
pub fn fixture(path: &str) -> Vec<u8> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/forge")
        .join(path);
    std::fs::read(&path).unwrap_or_else(|error| panic!("{}: {error}", path.display()))
}

/// One answer of the fake forge, for the requests with its method and path (with the query,
/// exact), and the headers of `when`.
#[derive(Debug, Clone)]
pub struct Route {
    method: String,
    path: String,
    when: Vec<(String, String)>,
    body_contains: Vec<String>,
    times: Option<usize>,
    status: u16,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl Route {
    /// `200` with an empty JSON object for `method path`.
    pub fn new(method: &str, path: &str) -> Self {
        Self {
            method: method.to_owned(),
            path: path.to_owned(),
            when: Vec::new(),
            body_contains: Vec::new(),
            times: None,
            status: 200,
            headers: Vec::new(),
            body: b"{}".to_vec(),
        }
    }

    /// `GET path`.
    pub fn get(path: &str) -> Self {
        Self::new("GET", path)
    }

    /// `POST path`.
    pub fn post(path: &str) -> Self {
        Self::new("POST", path)
    }

    /// Answers with this status. Checked here, on the test thread, so that a bad route fails
    /// the test instead of the server.
    pub fn status(mut self, status: u16) -> Self {
        StatusCode::from_u16(status).unwrap();
        self.status = status;
        self
    }

    /// Answers with the fixture `path` as the body.
    pub fn fixture(mut self, path: &str) -> Self {
        self.body = fixture(path);
        self
    }

    /// Answers with `body`.
    pub fn body(mut self, body: impl Into<Vec<u8>>) -> Self {
        self.body = body.into();
        self
    }

    /// Answers with this header too. `{base}` in the value is the base URL of the server.
    pub fn header(mut self, name: &str, value: &str) -> Self {
        HeaderName::from_bytes(name.as_bytes()).unwrap();
        HeaderValue::from_str(&value.replace(BASE, "http://127.0.0.1:1")).unwrap();
        self.headers.push((name.to_owned(), value.to_owned()));
        self
    }

    /// Matches only the requests with this header value.
    pub fn when(mut self, name: &str, value: &str) -> Self {
        self.when
            .push((name.to_ascii_lowercase(), value.to_owned()));
        self
    }

    /// Matches only the requests whose body contains `text`, such as the name of a GraphQL
    /// field: every GraphQL call is `POST /graphql`.
    pub fn when_body(mut self, text: &str) -> Self {
        self.body_contains.push(text.to_owned());
        self
    }

    /// Matches only the first `times` requests.
    pub const fn times(mut self, times: usize) -> Self {
        self.times = Some(times);
        self
    }
}

/// A request the fake forge received.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Recorded {
    /// The method.
    pub method: String,
    /// The path with the query, as sent.
    pub path: String,
    /// The headers, names in lowercase.
    pub headers: Vec<(String, String)>,
    /// The body.
    pub body: Vec<u8>,
}

impl Recorded {
    /// The value of the header `name` (lowercase).
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    }

    /// `METHOD /path?query`.
    pub fn line(&self) -> String {
        format!("{} {}", self.method, self.path)
    }

    /// The body as JSON.
    pub fn json(&self) -> serde_json::Value {
        serde_json::from_slice(&self.body).unwrap()
    }
}

#[derive(Default)]
struct Shared {
    base: String,
    routes: Vec<Route>,
    requests: Vec<Recorded>,
}

/// The fake forge. It stops with the test process.
pub struct FakeForge {
    base: String,
    shared: Arc<Mutex<Shared>>,
}

impl FakeForge {
    /// Starts a fake forge with no route: every request gets a 501.
    pub fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let shared = Arc::new(Mutex::new(Shared {
            base: base.clone(),
            ..Shared::default()
        }));
        let state = Arc::clone(&shared);
        thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            runtime.block_on(async move {
                let listener = tokio::net::TcpListener::from_std(listener).unwrap();
                let app = Router::new().fallback(answer).with_state(state);
                axum::serve(listener, app).await.unwrap();
            });
        });
        Self { base, shared }
    }

    /// The base URL, such as `http://127.0.0.1:41234`, without a trailing `/`.
    pub fn base(&self) -> &str {
        &self.base
    }

    /// Adds a route. Routes are tried in the order they were added.
    pub fn route(&self, route: Route) -> &Self {
        self.shared.lock().unwrap().routes.push(route);
        self
    }

    /// The requests received so far, in order.
    pub fn requests(&self) -> Vec<Recorded> {
        self.shared.lock().unwrap().requests.clone()
    }

    /// `METHOD /path?query` of each request received so far.
    pub fn lines(&self) -> Vec<String> {
        self.requests().iter().map(Recorded::line).collect()
    }
}

async fn answer(State(shared): State<Arc<Mutex<Shared>>>, request: Request) -> Response<Body> {
    let (parts, body) = request.into_parts();
    let body = to_bytes(body, usize::MAX).await.unwrap().to_vec();
    let recorded = Recorded {
        method: parts.method.as_str().to_owned(),
        path: parts.uri.path_and_query().map_or_else(
            || parts.uri.path().to_owned(),
            |path| path.as_str().to_owned(),
        ),
        headers: parts
            .headers
            .iter()
            .map(|(name, value)| {
                (
                    name.as_str().to_owned(),
                    value.to_str().unwrap_or("").to_owned(),
                )
            })
            .collect(),
        body,
    };
    let mut shared = shared.lock().unwrap();
    let base = shared.base.clone();
    let found = shared.routes.iter_mut().find(|route| {
        route.method == recorded.method
            && route.path == recorded.path
            && route.times != Some(0)
            && route
                .when
                .iter()
                .all(|(name, value)| recorded.header(name) == Some(value.as_str()))
            && route
                .body_contains
                .iter()
                .all(|text| String::from_utf8_lossy(&recorded.body).contains(text.as_str()))
    });
    let Some(route) = found else {
        let mut response = Response::new(Body::from(format!(
            r#"{{"message":"no route for {}"}}"#,
            recorded.line()
        )));
        *response.status_mut() = StatusCode::NOT_IMPLEMENTED;
        shared.requests.push(recorded);
        return response;
    };
    if let Some(times) = route.times.as_mut() {
        *times -= 1;
    }
    let mut response = Response::new(Body::from(
        String::from_utf8_lossy(&route.body).replace(BASE, &base),
    ));
    *response.status_mut() = StatusCode::from_u16(route.status).unwrap();
    let headers = response.headers_mut();
    headers.insert("content-type", HeaderValue::from_static("application/json"));
    for (name, value) in &route.headers {
        headers.append(
            HeaderName::from_bytes(name.as_bytes()).unwrap(),
            HeaderValue::from_str(&value.replace(BASE, &base)).unwrap(),
        );
    }
    shared.requests.push(recorded);
    response
}
