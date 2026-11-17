//! The embedded single-page frontend (design §1.3, §3.2).
//!
//! `web/index.html`, `web/css/` and `web/js/` are compiled into the binary.
//! Assets are served under `/assets/<build-id>/…`, where the build id is a
//! hash of all embedded files, so they can be cached forever (`immutable`)
//! and a new release never mixes old and new files. The shell itself is
//! revalidated on every load.
//!
//! `http` mounts the two handlers at [`INDEX_PATH`] and [`ASSET_PATH`]; both
//! are public, because the login screen is rendered from them.

use std::sync::LazyLock;

use axum::extract::Path;
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use include_dir::{Dir, include_dir};
use sha2::{Digest, Sha256};

static INDEX_TEMPLATE: &str = include_str!("../../../web/index.html");
static CSS: Dir<'static> = include_dir!("$CARGO_MANIFEST_DIR/../../web/css");
static JS: Dir<'static> = include_dir!("$CARGO_MANIFEST_DIR/../../web/js");

/// Placeholder in `index.html` replaced by the build id.
const BUILD_PLACEHOLDER: &str = "__BUILD_ID__";

static BUILD_ID: LazyLock<String> = LazyLock::new(|| {
    let mut hasher = Sha256::new();
    hasher.update(INDEX_TEMPLATE.as_bytes());
    for (prefix, dir) in [("css", &CSS), ("js", &JS)] {
        let mut files = Vec::new();
        collect_files(dir, &mut files);
        files.sort_by_key(|file| file.path());
        for file in files {
            hasher.update(prefix.as_bytes());
            hasher.update(file.path().to_string_lossy().as_bytes());
            hasher.update([0]);
            hasher.update(file.contents());
            hasher.update([0]);
        }
    }
    hex::encode(&hasher.finalize()[..6])
});

static INDEX_HTML: LazyLock<String> =
    LazyLock::new(|| INDEX_TEMPLATE.replace(BUILD_PLACEHOLDER, &BUILD_ID));

fn collect_files<'a>(dir: &'a Dir<'a>, out: &mut Vec<&'a include_dir::File<'a>>) {
    out.extend(dir.files());
    for sub in dir.dirs() {
        collect_files(sub, out);
    }
}

/// Path of the shell (`GET`).
pub const INDEX_PATH: &str = "/";
/// Path template of the assets (`GET`); `{build}` must be the current build id.
pub const ASSET_PATH: &str = "/assets/{build}/{*path}";

/// `GET /`: the shell. Public: the login screen is part of it.
pub async fn index() -> Response {
    (
        [
            (
                header::CONTENT_TYPE,
                HeaderValue::from_static("text/html; charset=utf-8"),
            ),
            (header::CACHE_CONTROL, HeaderValue::from_static("no-cache")),
        ],
        INDEX_HTML.as_str(),
    )
        .into_response()
}

/// `GET /assets/{build}/{*path}`: one embedded file.
pub async fn asset(Path((build, path)): Path<(String, String)>) -> Response {
    let file = (build == *BUILD_ID)
        .then(|| {
            let (dir, rest) = path.split_once('/')?;
            match dir {
                "css" => CSS.get_file(rest),
                "js" => JS.get_file(rest),
                _ => None,
            }
        })
        .flatten();
    let Some(file) = file else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let Some(content_type) = content_type(&path) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    (
        [
            (header::CONTENT_TYPE, HeaderValue::from_static(content_type)),
            (
                header::CACHE_CONTROL,
                HeaderValue::from_static("public, max-age=31536000, immutable"),
            ),
        ],
        file.contents(),
    )
        .into_response()
}

/// Only file types the frontend actually ships are served.
fn content_type(path: &str) -> Option<&'static str> {
    let ext = path.rsplit_once('.')?.1;
    Some(match ext {
        "js" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "woff2" => "font/woff2",
        "json" | "webmanifest" => "application/json",
        _ => return None,
    })
}
