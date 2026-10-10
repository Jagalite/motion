//! Route-level conformance between the served `/api/v2` router and the
//! reviewed OpenAPI contract. This checks which operations the real router
//! dispatches to a handler; request/response shapes stay with each workstream's
//! HTTP tests. The contract's `x-status` is a planning field, so the set of
//! served operations is pinned separately in `contracts/served_operations.txt`.
use axum::{
    Router,
    body::Body,
    extract::{MatchedPath, Request},
    http::{HeaderValue, Method, StatusCode},
    middleware::{self, Next},
    response::Response,
};
use playscale::{App, db, v2};
use playscale_core::access::AccessMode;
use serde_json::Value;
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
    sync::Arc,
};
use tokio::sync::{Mutex, Semaphore};
use tower::ServiceExt;

const ROOT: &str = env!("CARGO_MANIFEST_DIR");
const MATCHED: &str = "x-contract-test-matched";
const METHODS: [&str; 5] = ["get", "put", "post", "delete", "patch"];

fn contract() -> Value {
    serde_json::from_str(v2::system::CONTRACT_JSON).unwrap()
}

/// `(method, path) -> operationId` for every contract operation.
fn operations(contract: &Value) -> BTreeMap<(String, String), String> {
    let mut out = BTreeMap::new();
    for (path, item) in contract["paths"].as_object().unwrap() {
        for method in METHODS {
            if let Some(id) = item[method]["operationId"].as_str() {
                out.insert((method.to_string(), path.clone()), id.to_string());
            }
        }
    }
    out
}

/// Concrete request path for a templated contract path.
fn concrete(path: &str) -> String {
    let mut out = String::new();
    let mut rest = path;
    while let Some(start) = rest.find('{') {
        let end = rest[start..].find('}').unwrap() + start;
        out.push_str(&rest[..start]);
        out.push_str("contract-probe");
        rest = &rest[end + 1..];
    }
    out + rest
}

/// Axum route templates use the same `{name}` syntax as OpenAPI; compare
/// templates with parameter names erased.
fn shape(path: &str) -> String {
    concrete(path)
}

async fn mark_matched(request: Request, next: Next) -> Response {
    let matched = request
        .extensions()
        .get::<MatchedPath>()
        .map(|m| m.as_str().to_owned());
    let mut response = next.run(request).await;
    if let Some(matched) = matched {
        response
            .headers_mut()
            .insert(MATCHED, HeaderValue::from_str(&matched).unwrap());
    }
    response
}

async fn app(dir: &Path) -> App {
    let db = db::connect(&dir.join("db.sqlite")).await.unwrap();
    let state = dir.join("state");
    std::fs::create_dir(&state).unwrap();
    App {
        health: Arc::new(playscale::operations::Health::new(false)),
        db,
        admin_token: Arc::new("contract-test-operator-token-0123456789".into()),
        origin: Arc::new("http://127.0.0.1:8787".into()),
        authority: Arc::new("127.0.0.1:8787".into()),
        ffprobe: Arc::new("ffprobe".into()),
        jobs: Arc::new(Mutex::new(())),
        streams: Arc::new(Semaphore::new(2)),
        event_streams: Arc::new(Semaphore::new(4)),
        storage: Arc::new(playscale::storage::Runtime::new(state, Default::default())),
        processing: Arc::new(playscale::processing::Runtime::new(
            dir.join("cache"),
            Default::default(),
        )),
        access: Arc::new(v2::Runtime::new(
            AccessMode::TrustedHousehold,
            v2::auth::random_key(),
        )),
    }
}

/// Every `(method, contract path)` the production v2 router dispatches to a
/// handler. `route_layer` runs only for matched routes, never the fallback; a
/// matched path with an unrouted method answers 405.
async fn served(app: App, contract: &Value) -> BTreeSet<(String, String)> {
    let router: Router = Router::new()
        .nest(
            "/api/v2",
            v2::router().route_layer(middleware::from_fn(mark_matched)),
        )
        .with_state(app);
    let mut served = BTreeSet::new();
    for path in contract["paths"].as_object().unwrap().keys() {
        for method in METHODS {
            let request = axum::http::Request::builder()
                .method(Method::from_bytes(method.to_uppercase().as_bytes()).unwrap())
                .uri(concrete(path))
                .header("host", "127.0.0.1:8787")
                .body(Body::empty())
                .unwrap();
            let response = router.clone().oneshot(request).await.unwrap();
            let matched = response
                .headers()
                .get(MATCHED)
                .map(|v| v.to_str().unwrap().to_owned());
            if response.status() == StatusCode::METHOD_NOT_ALLOWED {
                continue;
            }
            if let Some(matched) = matched {
                assert_eq!(
                    shape(&matched),
                    shape(path),
                    "{method} {path} was served by a different route template"
                );
                served.insert((method.to_string(), path.clone()));
            }
        }
    }
    served
}

/// String literals passed to `.route(` in the v2 adapter sources.
fn route_literals() -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    let mut files: Vec<_> = std::fs::read_dir(Path::new(ROOT).join("src/v2"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == "rs"))
        .collect();
    files.sort();
    for file in files {
        let text = std::fs::read_to_string(&file).unwrap();
        let mut rest = text.as_str();
        while let Some(at) = rest.find(".route(") {
            rest = rest[at + ".route(".len()..].trim_start();
            if let Some(literal) = rest.strip_prefix('"') {
                let end = literal.find('"').unwrap();
                out.insert(format!("/api/v2{}", &literal[..end]));
            }
        }
    }
    out
}

fn ledger() -> BTreeSet<String> {
    std::fs::read_to_string(Path::new(ROOT).join("contracts/served_operations.txt"))
        .unwrap()
        .lines()
        .map(|l| l.split('#').next().unwrap().trim())
        .filter(|l| !l.is_empty())
        .map(str::to_owned)
        .collect()
}

#[test]
fn every_v2_route_template_is_a_contract_path() {
    let contract = contract();
    let paths: BTreeSet<String> = contract["paths"]
        .as_object()
        .unwrap()
        .keys()
        .map(|p| shape(p))
        .collect();
    let routes = route_literals();
    assert!(routes.len() > 30, "route scan found only {routes:?}");
    let undocumented: Vec<_> = routes
        .iter()
        .filter(|r| !paths.contains(&shape(r)))
        .collect();
    assert!(
        undocumented.is_empty(),
        "v2 routes absent from contracts/Motion_Server_API_v2.yaml: {undocumented:?}"
    );
}

#[test]
fn contract_operations_are_well_formed() {
    let contract = contract();
    let operations = operations(&contract);
    let ids: BTreeSet<_> = operations.values().collect();
    assert_eq!(ids.len(), operations.len(), "operationIds must be unique");
    for ((method, path), id) in &operations {
        let op = &contract["paths"][path][method];
        for field in ["x-owner", "x-milestone", "x-status"] {
            assert!(op[field].is_string(), "{id} lacks {field}");
        }
        assert!(
            path.starts_with("/api/v2/"),
            "{id}: {path} is outside /api/v2"
        );
    }
}

#[tokio::test]
async fn served_operations_match_the_ledger() {
    let dir = tempfile::tempdir().unwrap();
    let contract = contract();
    let operations = operations(&contract);
    let served: BTreeSet<String> = served(app(dir.path()).await, &contract)
        .await
        .into_iter()
        .map(|key| {
            operations.get(&key).cloned().unwrap_or_else(|| {
                panic!(
                    "{} {} is served but is not a contract operation",
                    key.0, key.1
                )
            })
        })
        .collect();
    let ledger = ledger();
    let unknown: Vec<_> = ledger
        .iter()
        .filter(|id| !operations.values().any(|o| o == *id))
        .collect();
    assert!(
        unknown.is_empty(),
        "ledger names unknown operations: {unknown:?}"
    );
    let missing: Vec<_> = ledger.difference(&served).collect();
    let unlisted: Vec<_> = served.difference(&ledger).collect();
    assert!(
        missing.is_empty() && unlisted.is_empty(),
        "contracts/served_operations.txt is out of date.\n\
         listed but not served: {missing:?}\nserved but not listed: {unlisted:?}"
    );
}
