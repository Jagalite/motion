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
/// Every OpenAPI 3.1 path-item operation method.
const METHODS: [&str; 8] = [
    "get", "put", "post", "delete", "options", "head", "patch", "trace",
];

fn contract() -> Value {
    serde_json::from_str(v2::system::CONTRACT_JSON).unwrap()
}

/// `(method, path) -> operationId` for every contract operation.
fn operations(contract: &Value) -> BTreeMap<(String, String), String> {
    let mut out = BTreeMap::new();
    for (path, item) in contract["paths"].as_object().unwrap() {
        for method in METHODS {
            if item.get(method).is_some() {
                let id = item[method]["operationId"]
                    .as_str()
                    .unwrap_or_else(|| panic!("{method} {path} lacks an operationId"));
                out.insert((method.to_string(), path.clone()), id.to_string());
            }
        }
    }
    out
}

/// Concrete request path for a templated contract path. The probe value is
/// not a template, so a static route that happens to spell it never matches
/// the parameterised template (`shape` keeps the braces).
fn concrete(path: &str) -> String {
    replace_parameters(path, "contract-probe")
}

/// Axum route templates use the same `{name}` syntax as OpenAPI; compare
/// templates with parameter names erased.
fn shape(path: &str) -> String {
    replace_parameters(path, "{}")
}

fn replace_parameters(path: &str, with: &str) -> String {
    let mut out = String::new();
    let mut rest = path;
    while let Some(start) = rest.find('{') {
        let end = rest[start..].find('}').unwrap() + start;
        out.push_str(&rest[..start]);
        out.push_str(with);
        rest = &rest[end + 1..];
    }
    out + rest
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

fn probe(method: &str, path: &str) -> axum::http::Request<Body> {
    axum::http::Request::builder()
        .method(Method::from_bytes(method.to_uppercase().as_bytes()).unwrap())
        .uri(concrete(path))
        .header("host", "127.0.0.1:8787")
        .body(Body::empty())
        .unwrap()
}

/// Every `(method, contract path)` the production v2 router dispatches to a
/// handler. `route_layer` runs only for matched routes, never the fallback.
/// It also wraps a matched path's method fallback, which answers 405, so each
/// path is first probed with a method no route registers: if that does not
/// answer 405, method-level detection is unreliable there and the test fails
/// instead of guessing.
async fn served(app: App, contract: &Value) -> BTreeSet<(String, String)> {
    let router: Router = Router::new()
        .nest(
            "/api/v2",
            v2::router().route_layer(middleware::from_fn(mark_matched)),
        )
        .with_state(app);
    let mut served = BTreeSet::new();
    for (path, item) in contract["paths"].as_object().unwrap() {
        let response = router
            .clone()
            .oneshot(probe("CONTRACTPROBE", path))
            .await
            .unwrap();
        if response.headers().contains_key(MATCHED) {
            assert_eq!(
                response.status(),
                StatusCode::METHOD_NOT_ALLOWED,
                "{path}: an unregistered method did not reach the 405 method fallback"
            );
        }
        for method in METHODS {
            // Axum answers HEAD from a GET handler; only an explicit HEAD
            // operation in the contract is checked for HEAD.
            if method == "head" && item.get("head").is_none() && item.get("get").is_some() {
                continue;
            }
            let response = router.clone().oneshot(probe(method, path)).await.unwrap();
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

fn sources(dir: &Path, out: &mut Vec<std::path::PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            sources(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

/// String literals passed to `.route(` anywhere under src/v2. The scan relies
/// on every route being registered with a literal at the v2 root, so it
/// rejects nesting and non-literal templates rather than misreading them.
fn route_literals() -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    let mut files = Vec::new();
    sources(&Path::new(ROOT).join("src/v2"), &mut files);
    files.sort();
    for file in files {
        let text = std::fs::read_to_string(&file).unwrap();
        for forbidden in [".nest(", ".nest_service(", ".route_service("] {
            assert!(
                !text.contains(forbidden),
                "{}: {forbidden} is not understood by tests/contract.rs; extend the scan",
                file.display()
            );
        }
        let mut rest = text.as_str();
        while let Some(at) = rest.find(".route(") {
            rest = rest[at + ".route(".len()..].trim_start();
            let literal = rest.strip_prefix('"').unwrap_or_else(|| {
                panic!(
                    "{}: .route( without a string literal template",
                    file.display()
                )
            });
            let end = literal.find('"').unwrap();
            out.insert(format!("/api/v2{}", &literal[..end]));
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
