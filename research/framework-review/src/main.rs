use salvo::prelude::*;
use std::path::PathBuf;

static FILE: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();

#[handler]
async fn salvo_file(req: &mut Request, res: &mut Response) {
    let file = salvo::fs::NamedFile::open(FILE.get().unwrap())
        .await
        .unwrap();
    if req.method() == salvo::http::Method::HEAD {
        file.send_head(req.headers(), res).await;
    } else {
        file.send(req.headers(), res).await;
    }
}

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().collect();
    let framework = &args[1];
    let address = format!("127.0.0.1:{}", args[2]);
    let path = PathBuf::from(&args[3]);
    FILE.set(path.clone()).unwrap();
    match framework.as_str() {
        "axum" => {
            let app = axum::Router::new()
                .route_service("/media", tower_http::services::ServeFile::new(path));
            let listener = tokio::net::TcpListener::bind(address).await.unwrap();
            axum::serve(listener, app).await.unwrap();
        }
        "poem" => {
            let app = poem::Route::new().at(
                "/media",
                poem::get(poem::endpoint::StaticFileEndpoint::new(path)),
            );
            poem::Server::new(poem::listener::TcpListener::bind(address))
                .run(app)
                .await
                .unwrap();
        }
        "salvo" => {
            let router =
                Router::new().push(Router::with_path("media").get(salvo_file).head(salvo_file));
            let acceptor = salvo::conn::TcpListener::new(address).bind().await;
            salvo::Server::new(acceptor).serve(router).await;
        }
        _ => panic!("unknown framework"),
    }
}
