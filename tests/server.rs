//! The HTTP API with the C test filter, without opening a socket.

mod common;

use std::io::Cursor;
use std::path::PathBuf;

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use base64::Engine;
use eightbf_host::catalog::filter_id;
use eightbf_host::config::Config;
use eightbf_host::runner::Environment;
use eightbf_host::server::{router, AppState};
use http_body_util::BodyExt;
use image::{DynamicImage, ImageBuffer, ImageFormat, Rgba};
use tower::ServiceExt;

fn png() -> Vec<u8> {
    let image = DynamicImage::ImageRgba8(ImageBuffer::from_pixel(4, 3, Rgba([10, 20, 30, 255])));
    let mut out = Cursor::new(Vec::new());
    image.write_to(&mut out, ImageFormat::Png).unwrap();
    out.into_inner()
}

struct Setup {
    _dir: tempfile::TempDir,
    app: axum::Router,
    id: String,
}

fn setup(isolate: bool) -> Option<Setup> {
    common::test_filter()?;
    let dir = tempfile::tempdir().unwrap();
    let library = common::install_native(dir.path(), "Invert", "InvertMain");
    let mut config = Config {
        plugin_dirs: vec![dir.path().to_path_buf()],
        cache: false,
        ..Config::default()
    };
    config.server.isolate = isolate;
    let env = Environment {
        self_exe: PathBuf::from(env!("CARGO_BIN_EXE_8bf-host")),
        ..Environment::current()
    };
    let state = AppState::new(config, env);
    let catalog = eightbf_host::host::discovery::read_plugin_path(&library).unwrap();
    let id = filter_id(&catalog[0].key());
    Some(Setup {
        _dir: dir,
        app: router(state),
        id,
    })
}

async fn body_bytes(response: axum::response::Response) -> Vec<u8> {
    response
        .into_body()
        .collect()
        .await
        .unwrap()
        .to_bytes()
        .to_vec()
}

async fn run_raw(isolate: bool) {
    let Some(setup) = setup(isolate) else {
        eprintln!("skipped: needs a C compiler (gcc or clang)");
        return;
    };
    let request = Request::post(format!("/v1/filters/{}/run", setup.id))
        .header(header::CONTENT_TYPE, "image/png")
        .body(Body::from(png()))
        .unwrap();
    let response = setup.app.oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()[header::CONTENT_TYPE], "image/png");
    let image = image::load_from_memory(&body_bytes(response).await)
        .unwrap()
        .to_rgba8();
    assert_eq!(image.get_pixel(1, 1).0, [245, 235, 225, 255]);
}

#[tokio::test]
async fn run_in_an_isolated_worker() {
    run_raw(true).await;
}

#[tokio::test]
async fn run_in_process() {
    run_raw(false).await;
}

#[tokio::test]
async fn multipart_with_options_and_a_json_response() {
    let Some(setup) = setup(false) else {
        return;
    };
    let boundary = "XBOUNDARYX";
    let mut body = Vec::new();
    let mut part = |name: &str, content_type: &str, data: &[u8]| {
        body.extend_from_slice(
            format!(
                "--{boundary}\r\nContent-Disposition: form-data; name=\"{name}\"; filename=\"{name}\"\r\nContent-Type: {content_type}\r\n\r\n"
            )
            .as_bytes(),
        );
        body.extend_from_slice(data);
        body.extend_from_slice(b"\r\n");
    };
    part("image", "image/png", &png());
    part(
        "options",
        "application/json",
        br#"{"format": "pnm", "rect": "0,0,2,3"}"#,
    );
    body.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());
    let request = Request::post(format!("/v1/filters/{}/run", setup.id))
        .header(
            header::CONTENT_TYPE,
            format!("multipart/form-data; boundary={boundary}"),
        )
        .header(header::ACCEPT, "application/json")
        .body(Body::from(body))
        .unwrap();
    let response = setup.app.oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = body_bytes(response).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "{}",
        String::from_utf8_lossy(&bytes)
    );
    let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(json["content_type"], "image/x-portable-anymap");
    assert_eq!(json["preset"]["format"], "8bf-host-preset");
    let image = base64::engine::general_purpose::STANDARD
        .decode(json["image"].as_str().unwrap())
        .unwrap();
    let image = image::load_from_memory(&image).unwrap().to_rgba8();
    assert_eq!(image.get_pixel(1, 1).0, [245, 235, 225, 255]);
    assert_eq!(image.get_pixel(3, 1).0, [10, 20, 30, 255]);
}

#[tokio::test]
async fn bad_images_and_bad_options_are_client_errors() {
    let Some(setup) = setup(false) else {
        return;
    };
    let request = Request::post(format!("/v1/filters/{}/run", setup.id))
        .body(Body::from("not an image"))
        .unwrap();
    let response = setup.app.clone().oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::UNSUPPORTED_MEDIA_TYPE);
    let request = Request::post(format!("/v1/filters/{}/run?rect=bogus", setup.id))
        .body(Body::from(png()))
        .unwrap();
    let response = setup.app.oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}
